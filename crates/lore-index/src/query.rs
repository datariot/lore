//! Query primitives over the index.

use lore_core::{HeadingPath, NodeId};

use crate::corpus::{CorpusIndex, DocId};
use crate::model::{DocumentIndex, HeadingNode};

/// A `(DocId, NodeId)` pair plus references into the corpus.
#[derive(Debug, Clone, Copy)]
pub struct NodeRef<'a> {
    pub doc: &'a DocumentIndex,
    pub node: &'a HeadingNode,
    pub doc_id: DocId,
}

/// Traversal helpers.
pub struct Traversal<'a> {
    pub corpus: &'a CorpusIndex,
}

impl<'a> Traversal<'a> {
    pub fn new(corpus: &'a CorpusIndex) -> Self {
        Self { corpus }
    }

    /// Find all nodes matching a full heading path (case-insensitive).
    pub fn resolve_path(&self, path: &HeadingPath) -> Vec<NodeRef<'a>> {
        // Exact match fast-path.
        if let Some(hits) = self.corpus.heading_lookup.get(path) {
            return hits
                .iter()
                .filter_map(|&(did, nid)| self.as_ref(did, nid))
                .collect();
        }
        // Fall back to case-insensitive segment-wise match.
        let lower: Vec<String> = path.0.iter().map(|s| s.to_lowercase()).collect();
        let mut out = Vec::new();
        for (key, hits) in &self.corpus.heading_lookup {
            if key.0.len() != lower.len() {
                continue;
            }
            let equal = key
                .0
                .iter()
                .zip(lower.iter())
                .all(|(a, b)| a.to_lowercase() == *b);
            if equal {
                for &(did, nid) in hits {
                    if let Some(r) = self.as_ref(did, nid) {
                        out.push(r);
                    }
                }
            }
        }
        out
    }

    fn as_ref(&self, did: DocId, nid: NodeId) -> Option<NodeRef<'a>> {
        let doc = self.corpus.doc(did)?;
        let node = doc.node(nid)?;
        Some(NodeRef {
            doc,
            node,
            doc_id: did,
        })
    }

    /// Walk the heading tree of a single document, depth-first, yielding each
    /// `(node, depth_in_tree)`.
    pub fn walk_doc<'b>(
        &self,
        doc: &'b DocumentIndex,
    ) -> impl Iterator<Item = (&'b HeadingNode, usize)> + 'b {
        let mut stack: Vec<(NodeId, usize)> = doc.roots.iter().rev().map(|&r| (r, 0)).collect();
        std::iter::from_fn(move || {
            let (nid, depth) = stack.pop()?;
            let node = doc.node(nid)?;
            // Push children in reverse so they pop in source order.
            for &c in node.children.iter().rev() {
                stack.push((c, depth + 1));
            }
            Some((node, depth))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::build_document;
    use lore_core::SourceId;
    use std::path::PathBuf;

    #[test]
    fn resolve_path_exact_match() {
        let mut corp = CorpusIndex::new(SourceId::new("k"), PathBuf::from("/t"));
        let doc = build_document(SourceId::new("k"), "a.md", "# Root\n\n## Leaf\n").unwrap();
        corp.push_document(doc);
        corp.rebuild_indices();
        let tr = Traversal::new(&corp);
        let hits = tr.resolve_path(&HeadingPath::new(["Root", "Leaf"]));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].node.title, "Leaf");
    }

    #[test]
    fn resolve_path_case_insensitive() {
        let mut corp = CorpusIndex::new(SourceId::new("k"), PathBuf::from("/t"));
        let doc = build_document(SourceId::new("k"), "a.md", "# Hello\n").unwrap();
        corp.push_document(doc);
        corp.rebuild_indices();
        let tr = Traversal::new(&corp);
        let hits = tr.resolve_path(&HeadingPath::new(["HELLO"]));
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn walk_doc_returns_source_order() {
        let mut corp = CorpusIndex::new(SourceId::new("k"), PathBuf::from("/t"));
        let doc = build_document(SourceId::new("k"), "a.md", "# A\n## B\n### C\n## D\n").unwrap();
        corp.push_document(doc);
        corp.rebuild_indices();
        let tr = Traversal::new(&corp);
        let titles: Vec<&str> = tr
            .walk_doc(&corp.documents[0])
            .map(|(n, _)| n.title.as_str())
            .collect();
        assert_eq!(titles, vec!["A", "B", "C", "D"]);
    }
}
