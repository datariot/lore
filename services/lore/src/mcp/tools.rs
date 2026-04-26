//! Request and response types for Lore's MCP tools.
//!
//! Kept separate from `server.rs` so the `#[tool_router]` impl stays readable
//! and so we can share types with integration tests.

use lore_core::{HeadingPath, NodeId};
use lore_index::DocId;
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
    pub path: Vec<String>,
    pub has_children: bool,
    /// Structural tag, when detected. `"dataview"` for Obsidian Dataview
    /// blocks; `None` for ordinary prose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TocDocument {
    pub rel_path: String,
    pub doc_id: u32,
    pub entries: Vec<TocEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frontmatter: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TocResponse {
    pub source_id: String,
    pub documents: Vec<TocDocument>,
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
    pub path: Vec<String>,
    pub byte_range: [u32; 2],
    pub content: String,
    pub outbound_links: Vec<LinkInfo>,
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchRequest {
    pub source_id: String,
    pub query: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    20
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchHit {
    pub rel_path: String,
    pub doc_id: u32,
    pub node_id: u32,
    pub level: u8,
    pub path: Vec<String>,
    pub summary: String,
    pub score: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchResponse {
    pub source_id: String,
    pub query: String,
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
    #[serde(default = "default_limit")]
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Backlink {
    pub rel_path: String,
    pub doc_id: u32,
    pub node_id: u32,
    pub level: u8,
    pub path: Vec<String>,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BacklinksResponse {
    pub source_id: String,
    pub target: String,
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
    pub path: Vec<String>,
    pub summary: String,
    pub access_count: u32,
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
    pub path: Vec<String>,
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
    pub node_count: usize,
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

/// Expand `HeadingPath` to `TocEntry` vec.
pub(crate) fn flatten_toc(doc: &lore_index::DocumentIndex, max_depth: Option<u8>) -> Vec<TocEntry> {
    let mut out = Vec::with_capacity(doc.nodes.len());
    for node in &doc.nodes {
        if let Some(limit) = max_depth
            && node.level > limit
        {
            continue;
        }
        out.push(TocEntry {
            node_id: node.id.0,
            level: node.level,
            title: node.title.clone(),
            path: node.path.0.clone(),
            has_children: !node.children.is_empty(),
            kind: node.kind.clone(),
        });
    }
    out
}

pub(crate) fn to_heading_path(segments: &[String]) -> HeadingPath {
    HeadingPath(segments.to_vec())
}

/// `(DocId, NodeId)` resolved from a request that may pass either node_id or
/// heading_path.
pub(crate) struct ResolvedNode<'a> {
    #[allow(dead_code)]
    pub doc: &'a lore_index::DocumentIndex,
    pub node: &'a lore_index::HeadingNode,
    #[allow(dead_code)]
    pub doc_id: DocId,
    pub node_id: NodeId,
}
