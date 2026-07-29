//! Request and response types for Lore's MCP tools.
//!
//! Kept separate from `server.rs` so the `#[tool_router]` impl stays readable
//! and so we can share types with integration tests.

use std::collections::BTreeMap;

use lore_core::{HeadingPath, NodeId};
use lore_index::{DocId, DocumentIndex};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// -----------------------------------------------------------------------------
// list_sources
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SourceSummary {
    pub source_id: String,
    pub root_dir: String,
    pub documents: usize,
    pub nodes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ListSourcesResponse {
    pub sources: Vec<SourceSummary>,
}

// -----------------------------------------------------------------------------
// table_of_contents
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TocRequest {
    /// Corpus identifier.
    pub source_id: String,
    /// Optional document path to narrow to a single file. If omitted, returns
    /// every document in the corpus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rel_path: Option<String>,
    /// Optional folder-prefix filter on `rel_path`. When set, only documents
    /// whose path starts with this prefix are returned. Use forward slashes;
    /// the comparison is byte-wise after stripping any leading `./`.
    /// Ignored when `rel_path` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_prefix: Option<String>,
    /// Maximum heading depth to include. `None` means no limit. Agents should
    /// start at depth 2 or 3 and drill down with a second call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u8>,
    /// When true, attach each document's frontmatter (YAML decoded as JSON)
    /// to the response. Off by default because frontmatter can be large.
    #[serde(default)]
    pub include_frontmatter: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TocEntry {
    pub node_id: u32,
    pub level: u8,
    pub title: String,
    pub heading_path: Vec<String>,
    /// True when the node has child headings in the document — even when
    /// `max_depth` pruned them from `children`. An agent seeing
    /// `has_children: true` with empty `children` should drill down.
    pub has_children: bool,
    /// Structural tag, when detected. `"dataview"` for Obsidian Dataview
    /// blocks; `None` for ordinary prose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Child headings, nested. Empty for leaves and at the `max_depth` cap.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<TocEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TocDocument {
    pub rel_path: String,
    pub doc_id: u32,
    /// Top-level headings, each carrying its subtree in `children`.
    pub roots: Vec<TocEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frontmatter: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TocResponse {
    pub source_id: String,
    pub documents: Vec<TocDocument>,
}

// -----------------------------------------------------------------------------
// corpus_map
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CorpusMapRequest {
    pub source_id: String,
    /// Restrict the map to documents whose `rel_path` starts with this
    /// folder prefix (byte-wise, forward slashes). Omit for the whole corpus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_prefix: Option<String>,
    /// Maximum heading depth to include under each document. `None` (the
    /// default) shows only each document's top-level heading(s) as a
    /// preview — call `table_of_contents` on a specific document to drill in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u8>,
}

/// A document leaf in the corpus map: its identity, curated hooks, and a
/// heading preview.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CorpusMapDoc {
    pub rel_path: String,
    pub doc_id: u32,
    /// First level-1 heading title, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Author-written frontmatter `description`, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// OKF `type` (concept kind), when the document carries OKF frontmatter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concept_type: Option<String>,
    /// OKF `status` (`draft` | `stable` | `deprecated`), when declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Age of the document in whole days at query time. `None` if mtime unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_days: Option<u32>,
    /// Heading tree for the document, depth-capped by the request's
    /// `max_depth` (top-level headings only by default).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headings: Vec<TocEntry>,
}

/// A folder node: everything under one directory prefix, with its
/// subfolders and the documents that live directly in it.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CorpusFolder {
    /// Folder path relative to the corpus root, ending in `/` (empty for the
    /// corpus root itself).
    pub path: String,
    /// Number of documents anywhere in this folder's subtree.
    pub doc_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folders: Vec<CorpusFolder>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub documents: Vec<CorpusMapDoc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CorpusMapResponse {
    pub source_id: String,
    pub root: CorpusFolder,
}

/// Heading preview for a document in the corpus map: the full depth-capped
/// tree when `max_depth` is set, otherwise just the roots (no children) so
/// the map stays a navigation aid rather than a full TOC dump.
fn doc_headings(doc: &DocumentIndex, max_depth: Option<u8>) -> Vec<TocEntry> {
    match max_depth {
        Some(_) => toc_tree(doc, max_depth),
        None => doc
            .roots
            .iter()
            .filter_map(|&nid| {
                let n = doc.node(nid)?;
                Some(TocEntry {
                    node_id: n.id.0,
                    level: n.level,
                    title: n.title.clone(),
                    heading_path: n.path.0.clone(),
                    has_children: !n.children.is_empty(),
                    kind: n.kind.clone(),
                    children: Vec::new(),
                })
            })
            .collect(),
    }
}

/// Mutable intermediate for assembling the folder tree from flat rel_paths.
#[derive(Default)]
struct FolderBuild {
    subfolders: BTreeMap<String, FolderBuild>,
    documents: Vec<CorpusMapDoc>,
}

impl FolderBuild {
    fn insert(&mut self, segments: &[String], doc: CorpusMapDoc) {
        match segments.split_first() {
            None => self.documents.push(doc),
            Some((head, rest)) => self
                .subfolders
                .entry(head.clone())
                .or_default()
                .insert(rest, doc),
        }
    }

    fn into_folder(self, path: String) -> CorpusFolder {
        let mut doc_count = self.documents.len();
        let mut folders = Vec::with_capacity(self.subfolders.len());
        for (name, child) in self.subfolders {
            let folder = child.into_folder(format!("{path}{name}/"));
            doc_count += folder.doc_count;
            folders.push(folder);
        }
        let mut documents = self.documents;
        documents.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        CorpusFolder {
            path,
            doc_count,
            folders,
            documents,
        }
    }
}

/// Build the corpus map: folders → documents → heading preview. Documents
/// are filtered by `path_prefix` and placed into the folder tree implied by
/// their `rel_path`. Pure projection over the loaded index — no derived
/// state, safe to run at query time (invariant #3 covers *indexing* work).
pub(crate) fn build_corpus_map(
    corpus: &lore_index::CorpusIndex,
    path_prefix: Option<&str>,
    max_depth: Option<u8>,
    now_unix: u64,
) -> CorpusFolder {
    let mut build = FolderBuild::default();
    for (i, doc) in corpus.documents.iter().enumerate() {
        if let Some(p) = path_prefix
            && !doc.rel_path.starts_with(p)
        {
            continue;
        }
        // Directory segments = everything but the final path component.
        let mut segments: Vec<String> = doc.rel_path.split('/').map(str::to_string).collect();
        segments.pop(); // drop the filename
        let map_doc = CorpusMapDoc {
            rel_path: doc.rel_path.clone(),
            doc_id: i as u32,
            title: doc.nodes.first().map(|n| n.title.clone()),
            description: doc.description().map(str::to_string),
            concept_type: doc.okf_type().map(str::to_string),
            status: doc.okf_status().map(str::to_string),
            age_days: doc.age_days(now_unix),
            headings: doc_headings(doc, max_depth),
        };
        build.insert(&segments, map_doc);
    }
    build.into_folder(String::new())
}

// -----------------------------------------------------------------------------
// get_section
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GetSectionRequest {
    pub source_id: String,
    /// Either `rel_path` + (`heading_path` or `node_id`) must be supplied.
    pub rel_path: String,
    /// Full heading ancestry, e.g. `["Architecture", "Caching"]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading_path: Option<Vec<String>>,
    /// Direct node id, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<u32>,
    /// When true, exclude the heading line itself (body only).
    #[serde(default)]
    pub body_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SectionResponse {
    pub source_id: String,
    pub rel_path: String,
    pub node_id: u32,
    pub level: u8,
    pub heading_path: Vec<String>,
    pub byte_range: [u32; 2],
    pub content: String,
    pub outbound_links: Vec<LinkInfo>,
    /// OKF `type` — the concept kind — when the document carries Open Knowledge
    /// Format frontmatter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concept_type: Option<String>,
    /// OKF `status` (`draft` | `stable` | `deprecated`), when declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// OKF trust tier (`human-reviewed` | `machine-confirmed`), when verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust: Option<String>,
    /// Age of the source document in whole days at fetch time. `None` if the
    /// filesystem didn't report an mtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_days: Option<u32>,
    /// `true` when the author's OKF `stale_after` date has passed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LinkInfo {
    pub target: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

// -----------------------------------------------------------------------------
// search
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GroupBy {
    /// One row per matching heading node — the default. Same-document
    /// hits produce multiple rows. Useful when sections are independently
    /// useful or the agent wants the highest-scoring N regardless of
    /// document boundaries.
    #[default]
    Section,
    /// One row per matching document, with the top-scoring section as the
    /// primary hit and up to `secondary_limit` additional same-document
    /// sections nested under it.
    Doc,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchRequest {
    pub source_id: String,
    pub query: String,
    /// Section mode: max sections returned. Doc mode: max documents
    /// returned (each with its own secondary list).
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Result granularity. Defaults to `section` (legacy behavior).
    #[serde(default)]
    pub group_by: GroupBy,
    /// In `group_by: "doc"` mode, max additional same-document sections to
    /// nest under each primary. Ignored in section mode.
    #[serde(default = "default_secondary_limit")]
    pub secondary_limit: usize,
    /// When set, each hit gets a `stale` boolean: `true` if its document is
    /// older than this many days. Lets the agent get a verdict instead of
    /// reasoning about `age_days` itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_after_days: Option<u32>,
}

fn default_limit() -> usize {
    20
}

fn default_secondary_limit() -> usize {
    3
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchHit {
    pub rel_path: String,
    pub doc_id: u32,
    pub node_id: u32,
    pub level: u8,
    pub heading_path: Vec<String>,
    pub summary: String,
    /// The document's author-written frontmatter `description`, when present.
    /// A curated retrieval hook — read this before the body-derived `summary`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// OKF `type` — the concept kind — when the document carries Open Knowledge
    /// Format frontmatter. Absent for ordinary notes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concept_type: Option<String>,
    /// OKF `status` (`draft` | `stable` | `deprecated`), when declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// OKF trust tier from the `verified` family: `human-reviewed` or
    /// `machine-confirmed`. Absent when the document is unverified. Prefer a
    /// human-reviewed hit over a machine-confirmed one when both answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust: Option<String>,
    /// Age of the source document in whole days at query time. Older hits are
    /// more likely stale — verify before asserting. `None` if mtime unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_days: Option<u32>,
    /// `true` when this hit should be treated as stale. Set when the author's
    /// OKF `stale_after` date has passed (authoritative, independent of any
    /// request), or when `stale_after_days` was passed and the document is
    /// older than that threshold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale: Option<bool>,
    pub score: f32,
    /// In `group_by: "doc"` mode, additional matching sections from the
    /// same document, ranked by score. Empty in section mode.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secondary_hits: Vec<SectionHit>,
}

/// A same-document secondary match. `rel_path` and `doc_id` are elided —
/// they're identical to the parent `SearchHit`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SectionHit {
    pub node_id: u32,
    pub level: u8,
    pub heading_path: Vec<String>,
    pub summary: String,
    pub score: f32,
}

/// Term-presence verdict for a query against a corpus. `full` = every
/// content term exists somewhere; `partial` = some do; `none` = the corpus
/// doesn't contain the query vocabulary at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CoverageLevel {
    Full,
    Partial,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchCoverage {
    pub level: CoverageLevel,
    /// Normalized (stemmed) query terms with postings in this corpus.
    pub matched_terms: Vec<String>,
    /// Normalized query terms with no postings — likely typos or vocabulary
    /// this corpus simply doesn't use.
    pub unmatched_terms: Vec<String>,
}

impl From<lore_search::CoverageReport> for SearchCoverage {
    fn from(r: lore_search::CoverageReport) -> Self {
        let level = match r.level {
            lore_search::Coverage::Full => CoverageLevel::Full,
            lore_search::Coverage::Partial => CoverageLevel::Partial,
            lore_search::Coverage::None => CoverageLevel::None,
        };
        SearchCoverage {
            level,
            matched_terms: r.matched_terms,
            unmatched_terms: r.unmatched_terms,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchResponse {
    pub source_id: String,
    pub query: String,
    /// Whether the corpus contains the query's vocabulary at all. Read this
    /// before trusting an empty or weak `hits` list: `none` means stop, not
    /// rephrase.
    pub coverage: SearchCoverage,
    pub hits: Vec<SearchHit>,
}

// -----------------------------------------------------------------------------
// add_source
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AddSourceRequest {
    /// Absolute path to the corpus root directory. Lore will run a full index
    /// pass on it and register the result under `source_id` (defaulting to
    /// the directory basename).
    pub root: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    /// When true, rebuild the index even if `.lore/index.json` already exists.
    #[serde(default)]
    pub rebuild: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AddSourceResponse {
    pub source_id: String,
    pub root_dir: String,
    pub documents: usize,
    pub nodes: usize,
    pub indexed: bool,
}

// -----------------------------------------------------------------------------
// backlinks
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BacklinksRequest {
    pub source_id: String,
    /// Link target string to look up — typically a document stem
    /// (`architecture` for `[[architecture]]`) or a `path#fragment` form.
    pub target: String,
    /// Narrow to links aimed at one *section* of the target document: the
    /// heading text as authored (`Purpose` for `[[intro#Purpose]]`), matched
    /// case-insensitively. Only links written with a resolving `#fragment`
    /// count. Omit for document-level backlinks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_anchor: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

/// The section a `target` + `target_anchor` pair resolved to.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ResolvedTarget {
    pub rel_path: String,
    pub doc_id: u32,
    pub node_id: u32,
    pub heading_path: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Backlink {
    pub rel_path: String,
    pub doc_id: u32,
    pub node_id: u32,
    pub level: u8,
    pub heading_path: Vec<String>,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_days: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BacklinksResponse {
    pub source_id: String,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_anchor: Option<String>,
    /// Sections the anchor resolved to — normally one; more when the target
    /// spelling is ambiguous across documents. Empty in document-level mode
    /// and when the anchor didn't match any heading.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolved_targets: Vec<ResolvedTarget>,
    pub backlinks: Vec<Backlink>,
}

// -----------------------------------------------------------------------------
// recent_hot
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HotRequest {
    pub source_id: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HotNode {
    pub rel_path: String,
    pub doc_id: u32,
    pub node_id: u32,
    pub level: u8,
    pub heading_path: Vec<String>,
    pub summary: String,
    /// Total accesses ever (monotonic, never decayed).
    pub access_count: u32,
    /// The time-decayed score this node was ranked by (two-week half-life).
    pub decayed_score: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_days: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HotResponse {
    pub source_id: String,
    pub nodes: Vec<HotNode>,
}

// -----------------------------------------------------------------------------
// neighbors
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NeighborsRequest {
    pub source_id: String,
    pub rel_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading_path: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NeighborRef {
    pub node_id: u32,
    pub level: u8,
    pub title: String,
    pub heading_path: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NeighborsResponse {
    pub source_id: String,
    pub rel_path: String,
    pub node_id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<NeighborRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev_sibling: Option<NeighborRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_sibling: Option<NeighborRef>,
    pub children: Vec<NeighborRef>,
}

// -----------------------------------------------------------------------------
// list_documents
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ListDocumentsRequest {
    pub source_id: String,
    /// Optional folder-prefix filter on `rel_path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_prefix: Option<String>,
    /// Optional frontmatter equality filters. Each `key: value` pair must
    /// match the document's frontmatter — for scalar fields, JSON equality;
    /// for array fields, the filter value must appear as an element. All
    /// filters AND together. Pass `{}` or omit for no frontmatter filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frontmatter: Option<serde_json::Map<String, serde_json::Value>>,
    /// When true, attach each document's frontmatter to the response.
    #[serde(default)]
    pub include_frontmatter: bool,
    #[serde(default = "default_doc_list_limit")]
    pub limit: usize,
}

fn default_doc_list_limit() -> usize {
    1_000
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DocumentSummary {
    pub rel_path: String,
    pub doc_id: u32,
    /// Title of the first level-1 heading, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Author-written frontmatter `description`, when present — the curated
    /// one-line hook for the document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// OKF `type` (concept kind), when the document carries OKF frontmatter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concept_type: Option<String>,
    /// OKF `status` (`draft` | `stable` | `deprecated`), when declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// OKF trust tier (`human-reviewed` | `machine-confirmed`), when verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust: Option<String>,
    pub node_count: usize,
    /// Age of the document in whole days. `None` if mtime unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_days: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frontmatter: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ListDocumentsResponse {
    pub source_id: String,
    pub documents: Vec<DocumentSummary>,
    /// `true` when `documents.len() == limit` and more matches existed.
    pub truncated: bool,
}

/// Match a request filter map against a document's frontmatter value.
///
/// Semantics: every `(key, expected)` pair in `filters` must be satisfied by
/// the document's frontmatter. A document with no frontmatter never matches a
/// non-empty filter set. A scalar `expected` matches an array value via
/// element-equality (e.g. `tags: "project"` matches `tags: [project, work]`).
/// Otherwise `==` on JSON values.
pub(crate) fn frontmatter_matches(
    filters: &serde_json::Map<String, serde_json::Value>,
    fm: Option<&serde_json::Value>,
) -> bool {
    if filters.is_empty() {
        return true;
    }
    let Some(serde_json::Value::Object(map)) = fm else {
        return false;
    };
    for (key, expected) in filters {
        let Some(actual) = map.get(key) else {
            return false;
        };
        match (actual, expected) {
            (serde_json::Value::Array(items), exp) if !exp.is_array() => {
                if !items.iter().any(|v| v == exp) {
                    return false;
                }
            }
            (a, e) => {
                if a != e {
                    return false;
                }
            }
        }
    }
    true
}

// -----------------------------------------------------------------------------
// get_by_path
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GetByPathRequest {
    pub source_id: String,
    /// Qualified path of the form `path/to/file.md#Heading > Subheading`.
    /// The `#` portion is optional — omit to return the whole document.
    pub qualified_path: String,
    #[serde(default)]
    pub body_only: bool,
}

// -----------------------------------------------------------------------------
// helpers used by the server impl
// -----------------------------------------------------------------------------

/// Project a document's heading tree into nested `TocEntry` values, walking
/// `roots` → `children` and pruning subtrees below `max_depth`.
pub(crate) fn toc_tree(doc: &lore_index::DocumentIndex, max_depth: Option<u8>) -> Vec<TocEntry> {
    doc.roots
        .iter()
        .filter_map(|&nid| toc_subtree(doc, nid, max_depth))
        .collect()
}

fn toc_subtree(
    doc: &lore_index::DocumentIndex,
    node_id: NodeId,
    max_depth: Option<u8>,
) -> Option<TocEntry> {
    let node = doc.node(node_id)?;
    if let Some(limit) = max_depth
        && node.level > limit
    {
        return None;
    }
    Some(TocEntry {
        node_id: node.id.0,
        level: node.level,
        title: node.title.clone(),
        heading_path: node.path.0.clone(),
        has_children: !node.children.is_empty(),
        kind: node.kind.clone(),
        children: node
            .children
            .iter()
            .filter_map(|&cid| toc_subtree(doc, cid, max_depth))
            .collect(),
    })
}

pub(crate) fn to_heading_path(segments: &[String]) -> HeadingPath {
    HeadingPath(segments.to_vec())
}

/// `(DocId, NodeId)` resolved from a request that may pass either node_id or
/// heading_path.
pub(crate) struct ResolvedNode<'a> {
    pub doc: &'a lore_index::DocumentIndex,
    pub node: &'a lore_index::HeadingNode,
    #[allow(dead_code)]
    pub doc_id: DocId,
    pub node_id: NodeId,
}

#[cfg(test)]
mod tests {
    use super::*;
    use lore_core::SourceId;
    use lore_index::{CorpusIndex, build_document};
    use std::path::PathBuf;

    fn corpus_of(paths: &[&str]) -> CorpusIndex {
        let mut c = CorpusIndex::new(SourceId::new("t"), PathBuf::from("/tmp"));
        for p in paths {
            // Give each doc one H1 so `title` / heading preview are populated.
            let title = p.rsplit('/').next().unwrap();
            let src = format!("# {title}\n\nbody.\n");
            c.push_document(build_document(SourceId::new("t"), *p, &src).unwrap());
        }
        c.rebuild_indices();
        c
    }

    #[test]
    fn corpus_map_nests_folders_and_counts_subtrees() {
        let corpus = corpus_of(&[
            "README.md",
            "docs/intro.md",
            "docs/guide/setup.md",
            "docs/guide/deploy.md",
        ]);
        let root = build_corpus_map(&corpus, None, None, 0);

        assert_eq!(root.path, "");
        assert_eq!(root.doc_count, 4, "recursive count over the whole tree");
        assert_eq!(root.documents.len(), 1, "only README at the root");
        assert_eq!(root.documents[0].rel_path, "README.md");

        assert_eq!(root.folders.len(), 1);
        let docs = &root.folders[0];
        assert_eq!(docs.path, "docs/");
        assert_eq!(docs.doc_count, 3, "intro + guide subtree");
        assert_eq!(docs.documents.len(), 1, "intro.md directly under docs/");

        let guide = &docs.folders[0];
        assert_eq!(guide.path, "docs/guide/");
        assert_eq!(guide.doc_count, 2);
        // Documents are sorted by rel_path for stable output.
        assert_eq!(guide.documents[0].rel_path, "docs/guide/deploy.md");
        assert_eq!(guide.documents[1].rel_path, "docs/guide/setup.md");
    }

    #[test]
    fn corpus_map_path_prefix_selects_subtree() {
        let corpus = corpus_of(&["README.md", "docs/intro.md", "other/note.md"]);
        let root = build_corpus_map(&corpus, Some("docs/"), None, 0);
        assert_eq!(root.doc_count, 1, "only docs/ matched");
        assert!(
            root.documents.is_empty(),
            "nothing lives at the root of docs/-only"
        );
        assert_eq!(root.folders.len(), 1);
        assert_eq!(root.folders[0].path, "docs/");
    }

    #[test]
    fn corpus_map_default_preview_is_shallow() {
        // A doc with nested headings: default (max_depth None) shows only the
        // root heading with no children; has_children flags the deeper tree.
        let mut c = CorpusIndex::new(SourceId::new("t"), PathBuf::from("/tmp"));
        c.push_document(
            build_document(SourceId::new("t"), "a.md", "# Root\n\n## Child\n\nbody.\n").unwrap(),
        );
        c.rebuild_indices();
        let root = build_corpus_map(&c, None, None, 0);
        let doc = &root.documents[0];
        assert_eq!(doc.headings.len(), 1, "root heading only");
        assert!(
            doc.headings[0].children.is_empty(),
            "no nested children in preview"
        );
        assert!(
            doc.headings[0].has_children,
            "but flags that a child exists"
        );

        // With max_depth 2 the child is included.
        let deep = build_corpus_map(&c, None, Some(2), 0);
        assert_eq!(deep.documents[0].headings[0].children.len(), 1);
    }
}
