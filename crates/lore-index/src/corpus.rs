//! Corpus-level aggregation of document indices.

use std::path::PathBuf;

use hashbrown::HashMap;
use lore_core::{HeadingPath, NodeId, SourceId};
use serde::{Deserialize, Serialize};

use crate::model::DocumentIndex;

/// Dense identifier for a document within a corpus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DocId(pub u32);

impl DocId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Which field of a node a term was seen in. BM25 applies a per-field
/// weight at scoring time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Title,
    Path,
    Summary,
}

impl Field {
    pub fn as_u8(self) -> u8 {
        match self {
            Field::Title => 0,
            Field::Path => 1,
            Field::Summary => 2,
        }
    }
}

/// A posting in the inverted index: the node where a term occurred,
/// which field it was seen in, and how many times.
#[derive(Debug, Clone, Copy)]
pub struct Posting {
    pub doc: DocId,
    pub node: NodeId,
    pub field: Field,
    pub tf: u16,
}

/// Length of a node's text in each field (in tokens). Index is `DocId`
/// then `NodeId`. Stored as `Vec<Vec<_>>` for cache-friendly access.
#[derive(Debug, Clone, Default)]
pub struct FieldLengths {
    pub title: Vec<Vec<u16>>,
    pub path: Vec<Vec<u16>>,
    pub summary: Vec<Vec<u16>>,
    pub total_nodes: u32,
    pub avg_title: f32,
    pub avg_path: f32,
    pub avg_summary: f32,
}

impl FieldLengths {
    pub fn get(&self, did: DocId, nid: NodeId, field: Field) -> u16 {
        let d = did.index();
        let n = nid.index();
        let arena = match field {
            Field::Title => &self.title,
            Field::Path => &self.path,
            Field::Summary => &self.summary,
        };
        arena.get(d).and_then(|v| v.get(n)).copied().unwrap_or(0)
    }
}

/// Everything known about one corpus: its documents, its root directory on
/// disk, and the lookup tables the MCP server uses at query time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusIndex {
    pub source: SourceId,
    pub root_dir: PathBuf,
    pub documents: Vec<DocumentIndex>,
    /// Map from a full heading path (including document stem? no — just the
    /// heading ancestry) to every node that matches. Most paths are unique;
    /// the `Vec` handles collisions.
    #[serde(skip, default)]
    pub heading_lookup: HashMap<HeadingPath, Vec<(DocId, NodeId)>>,
    /// Trigram index over titles + path segments, used by `lore-search`.
    #[serde(skip, default)]
    pub title_trigrams: HashMap<[u8; 3], Vec<(DocId, NodeId)>>,
    /// Map from `rel_path` to `DocId`, for watch-mode incremental re-index.
    #[serde(skip, default)]
    pub path_to_doc: HashMap<String, DocId>,
    /// Precomputed backlinks: target string -> nodes that link to it.
    #[serde(skip, default)]
    pub backlinks: HashMap<String, Vec<(DocId, NodeId)>>,
    /// Inverted index: normalized token → postings (one per field occurrence).
    #[serde(skip, default)]
    pub inverted: HashMap<String, Vec<Posting>>,
    /// Per-node, per-field token lengths + corpus averages. Lets BM25 run
    /// in O(query_tokens * posting_length) instead of O(total_tokens).
    #[serde(skip, default)]
    pub field_lengths: FieldLengths,
}

impl CorpusIndex {
    pub fn new(source: SourceId, root_dir: PathBuf) -> Self {
        Self {
            source,
            root_dir,
            documents: Vec::new(),
            heading_lookup: HashMap::new(),
            title_trigrams: HashMap::new(),
            path_to_doc: HashMap::new(),
            backlinks: HashMap::new(),
            inverted: HashMap::new(),
            field_lengths: FieldLengths::default(),
        }
    }

    pub fn push_document(&mut self, doc: DocumentIndex) -> DocId {
        let id = DocId(self.documents.len() as u32);
        self.documents.push(doc);
        id
    }

    /// Rebuild all derived indices from scratch. Call after loading from disk
    /// or after incremental re-indexing.
    pub fn rebuild_indices(&mut self) {
        self.heading_lookup.clear();
        self.title_trigrams.clear();
        self.path_to_doc.clear();
        self.backlinks.clear();
        self.inverted.clear();

        let mut title_lens: Vec<Vec<u16>> = Vec::with_capacity(self.documents.len());
        let mut path_lens: Vec<Vec<u16>> = Vec::with_capacity(self.documents.len());
        let mut summary_lens: Vec<Vec<u16>> = Vec::with_capacity(self.documents.len());
        let mut total_nodes: u32 = 0;
        let mut title_total: u64 = 0;
        let mut path_total: u64 = 0;
        let mut summary_total: u64 = 0;

        for (di, doc) in self.documents.iter().enumerate() {
            let did = DocId(di as u32);
            self.path_to_doc.insert(doc.rel_path.clone(), did);
            let mut doc_title = Vec::with_capacity(doc.nodes.len());
            let mut doc_path = Vec::with_capacity(doc.nodes.len());
            let mut doc_summary = Vec::with_capacity(doc.nodes.len());

            for node in &doc.nodes {
                self.heading_lookup
                    .entry(node.path.clone())
                    .or_default()
                    .push((did, node.id));
                for trigram in trigrams_of(&node.title) {
                    self.title_trigrams
                        .entry(trigram)
                        .or_default()
                        .push((did, node.id));
                }
                for path_seg in &node.path.0 {
                    for trigram in trigrams_of(path_seg) {
                        self.title_trigrams
                            .entry(trigram)
                            .or_default()
                            .push((did, node.id));
                    }
                }
                for link in &node.outbound_links {
                    for key in canonical_link_keys(&link.target) {
                        self.backlinks.entry(key).or_default().push((did, node.id));
                    }
                }

                let title_tokens = tokenize(&node.title);
                let path_tokens: Vec<String> =
                    node.path.0.iter().flat_map(|s| tokenize(s)).collect();
                let summary_tokens = tokenize(&node.summary);

                doc_title.push(title_tokens.len().min(u16::MAX as usize) as u16);
                doc_path.push(path_tokens.len().min(u16::MAX as usize) as u16);
                doc_summary.push(summary_tokens.len().min(u16::MAX as usize) as u16);
                title_total += title_tokens.len() as u64;
                path_total += path_tokens.len() as u64;
                summary_total += summary_tokens.len() as u64;
                total_nodes += 1;

                for (field, tokens) in [
                    (Field::Title, &title_tokens),
                    (Field::Path, &path_tokens),
                    (Field::Summary, &summary_tokens),
                ] {
                    add_postings(&mut self.inverted, did, node.id, field, tokens);
                }
            }
            title_lens.push(doc_title);
            path_lens.push(doc_path);
            summary_lens.push(doc_summary);
        }

        let denom = total_nodes.max(1) as f32;
        self.field_lengths = FieldLengths {
            title: title_lens,
            path: path_lens,
            summary: summary_lens,
            total_nodes,
            avg_title: title_total as f32 / denom,
            avg_path: path_total as f32 / denom,
            avg_summary: summary_total as f32 / denom,
        };
    }

    pub fn doc(&self, id: DocId) -> Option<&DocumentIndex> {
        self.documents.get(id.index())
    }

    pub fn doc_mut(&mut self, id: DocId) -> Option<&mut DocumentIndex> {
        self.documents.get_mut(id.index())
    }

    pub fn total_nodes(&self) -> usize {
        self.documents.iter().map(|d| d.nodes.len()).sum()
    }
}

/// Add per-field postings for a node to the inverted index.
fn add_postings(
    inverted: &mut HashMap<String, Vec<Posting>>,
    did: DocId,
    nid: NodeId,
    field: Field,
    tokens: &[String],
) {
    if tokens.is_empty() {
        return;
    }
    let mut counts: HashMap<&str, u16> = HashMap::new();
    for t in tokens {
        *counts.entry(t.as_str()).or_insert(0) += 1;
    }
    for (tok, tf) in counts {
        inverted.entry(tok.to_string()).or_default().push(Posting {
            doc: did,
            node: nid,
            field,
            tf,
        });
    }
}

/// Tokenize text the same way the ranker does. Kept here so the inverted
/// index and ranker can't drift from each other without a compile error.
pub fn tokenize(text: &str) -> Vec<String> {
    use unicode_segmentation::UnicodeSegmentation;

    let mut out = Vec::new();
    for word in UnicodeSegmentation::unicode_words(text) {
        let cleaned: String = word
            .chars()
            .filter(|c: &char| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect();
        if cleaned.len() < 2 {
            continue;
        }
        if STOPWORDS.contains(&cleaned.as_str()) {
            continue;
        }
        out.push(cleaned);
    }
    out
}

const STOPWORDS: &[&str] = &[
    "the", "and", "or", "of", "to", "in", "on", "at", "a", "an", "is", "it", "for", "with", "as",
    "by", "be", "are", "this", "that",
];

/// Every canonical key a link target can be looked up under in `backlinks`.
///
/// Obsidian wiki-links come in several shapes:
///
/// - `[[Page]]` — bare stem
/// - `[[folder/Page]]` — qualified path
/// - `[[Page#Heading]]` — fragment into the page
/// - `[[Page|alias]]` — alias already stripped at parse time
/// - `[[Page.md]]` — extension included
///
/// We want `backlinks("Page")` to find all of these, regardless of how the
/// link was authored. So we index each target under multiple keys: the
/// full target, its basename, and the stem with any `#fragment` or `.md`
/// suffix stripped. All keys are lowercased.
pub fn canonical_link_keys(target: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let base = target.to_lowercase();
    keys.push(base.clone());

    // Strip any fragment ("#heading").
    let no_fragment = match base.find('#') {
        Some(i) => base[..i].to_string(),
        None => base.clone(),
    };
    if no_fragment != base {
        keys.push(no_fragment.clone());
    }

    // Basename (last path segment).
    let basename = no_fragment
        .rsplit('/')
        .next()
        .unwrap_or(&no_fragment)
        .to_string();
    if basename != no_fragment {
        keys.push(basename.clone());
    }

    // Strip common markdown extension.
    for key in &[basename.clone(), no_fragment.clone()] {
        if let Some(stem) = key.strip_suffix(".md") {
            keys.push(stem.to_string());
        }
        if let Some(stem) = key.strip_suffix(".markdown") {
            keys.push(stem.to_string());
        }
    }

    keys.sort();
    keys.dedup();
    keys
}

/// Lowercased ASCII trigrams. Non-ASCII characters split trigram runs.
pub fn trigrams_of(s: &str) -> impl Iterator<Item = [u8; 3]> + '_ {
    let bytes = s.as_bytes();
    (0..bytes.len().saturating_sub(2)).filter_map(move |i| {
        let a = normalize(bytes[i])?;
        let b = normalize(bytes[i + 1])?;
        let c = normalize(bytes[i + 2])?;
        Some([a, b, c])
    })
}

fn normalize(b: u8) -> Option<u8> {
    if b.is_ascii_alphanumeric() {
        Some(b.to_ascii_lowercase())
    } else if b.is_ascii_whitespace() || b == b'-' || b == b'_' {
        Some(b' ')
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::build_document;

    #[test]
    fn indices_built_after_push() {
        let mut corp = CorpusIndex::new(SourceId::new("k"), PathBuf::from("/tmp"));
        let doc = build_document(SourceId::new("k"), "a.md", "# Hello\n\n## Sub\n").unwrap();
        corp.push_document(doc);
        corp.rebuild_indices();

        assert!(
            corp.heading_lookup
                .contains_key(&HeadingPath::new(["Hello"]))
        );
        assert!(
            corp.heading_lookup
                .contains_key(&HeadingPath::new(["Hello", "Sub"]))
        );
        assert_eq!(corp.path_to_doc.get("a.md"), Some(&DocId(0)));
    }

    #[test]
    fn trigrams_lowercase_and_ascii_only() {
        let ts: Vec<[u8; 3]> = trigrams_of("Hello world").collect();
        assert!(ts.contains(b"hel"));
        assert!(ts.contains(b"o w"));
    }

    #[test]
    fn canonical_keys_cover_obsidian_shapes() {
        let keys = canonical_link_keys("folder/Page.md#Heading");
        assert!(keys.contains(&"folder/page.md#heading".to_string()));
        assert!(keys.contains(&"folder/page.md".to_string()));
        assert!(keys.contains(&"folder/page".to_string()));
        assert!(keys.contains(&"page.md".to_string()));
        assert!(keys.contains(&"page".to_string()));
    }

    #[test]
    fn canonical_keys_handle_bare_stem() {
        let keys = canonical_link_keys("Page");
        assert_eq!(keys, vec!["page"]);
    }

    #[test]
    fn backlinks_found_regardless_of_link_shape() {
        let mut corp = CorpusIndex::new(SourceId::new("k"), PathBuf::from("/t"));
        let src = "# A\n\nsee [[docs/arch.md#Caching|the cache]] and [[arch]].\n";
        let doc = crate::builder::build_document(SourceId::new("k"), "a.md", src).unwrap();
        corp.push_document(doc);
        corp.rebuild_indices();
        // All three written forms should resolve to the same target node.
        assert!(corp.backlinks.contains_key("arch"));
        assert!(corp.backlinks.contains_key("arch.md"));
        assert!(corp.backlinks.contains_key("docs/arch"));
        assert!(corp.backlinks.contains_key("docs/arch.md"));
    }
}
