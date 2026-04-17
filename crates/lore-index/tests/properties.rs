//! Invariants that must hold for *any* valid markdown document.
//!
//! Property tests are the highest-value tests for the indexer because the
//! failure modes we care about — missing bytes, overlapping siblings, wrong
//! ancestry — are structural and apply uniformly across all inputs.

use lore_core::{ByteRange, NodeId, SourceId};
use lore_index::{DocumentIndex, build_document};
use proptest::prelude::*;

// -----------------------------------------------------------------------------
// Generators
// -----------------------------------------------------------------------------

/// A simplified markdown generator that produces a valid but non-trivial doc.
/// We care about the *structure* of generated inputs, not their character-level
/// detail, so we generate by tree-shape and stringify.
fn arb_heading_tree() -> impl Strategy<Value = Vec<(u8, String)>> {
    // Produce a sequence of (level, title) pairs where level is 1..=4 and
    // adjacent levels may differ but must be >= 1.
    let seg = "[A-Za-z][A-Za-z0-9 ]{0,16}";
    prop::collection::vec((1u8..=4u8, seg), 1..=24)
}

fn render_headings(headings: &[(u8, String)]) -> String {
    let mut out = String::new();
    for (i, (level, title)) in headings.iter().enumerate() {
        let hashes = "#".repeat(*level as usize);
        out.push_str(&hashes);
        out.push(' ');
        out.push_str(title);
        out.push('\n');
        out.push('\n');
        // Varying body: blank, a sentence, or a wiki-link
        match i % 3 {
            0 => out.push_str("some body text.\n\n"),
            1 => out.push_str("see [[another page|alias]] for detail.\n\n"),
            _ => out.push_str("a paragraph with [a link](https://x).\n\n"),
        }
    }
    out
}

// -----------------------------------------------------------------------------
// Invariants
// -----------------------------------------------------------------------------

/// Union of sibling ranges at every level covers the doc body contiguously.
fn assert_siblings_contiguous(doc: &DocumentIndex) {
    // Roots must cover [body_offset, source_len].
    assert_contiguous(&doc.roots, doc, doc.body_offset, doc.source_len);
    // Recursively check children of every node.
    for n in &doc.nodes {
        if n.children.is_empty() {
            continue;
        }
        // Children cover [parent.content_range.start, parent.byte_range.end].
        // But note: parent.content_range.start is *after* the heading line.
        // Children may start after some prose too — our invariant is weaker:
        // the first child starts >= parent.content_range.start, the last child
        // ends == parent.byte_range.end, and siblings are pairwise non-overlapping.
        for pair in n.children.windows(2) {
            let a = &doc.nodes[pair[0].index()];
            let b = &doc.nodes[pair[1].index()];
            assert!(
                a.byte_range.end <= b.byte_range.start,
                "sibling overlap: {:?} .. {:?}",
                a.byte_range,
                b.byte_range
            );
        }
        let last = &doc.nodes[n.children.last().unwrap().index()];
        assert_eq!(
            last.byte_range.end, n.byte_range.end,
            "last child must close parent range exactly"
        );
    }
}

fn assert_contiguous(ids: &[NodeId], doc: &DocumentIndex, start: u32, end: u32) {
    if ids.is_empty() {
        return;
    }
    let first = &doc.nodes[ids[0].index()];
    let last = &doc.nodes[ids.last().unwrap().index()];
    assert!(
        first.byte_range.start >= start,
        "first root starts before corpus body"
    );
    assert_eq!(
        last.byte_range.end, end,
        "last root must end at source_len (end={end}, got {})",
        last.byte_range.end
    );
    for pair in ids.windows(2) {
        let a = &doc.nodes[pair[0].index()];
        let b = &doc.nodes[pair[1].index()];
        assert_eq!(
            a.byte_range.end, b.byte_range.start,
            "root siblings must be contiguous: {:?} / {:?}",
            a.byte_range, b.byte_range
        );
    }
}

/// Every node's depth in the tree equals its heading path length.
fn assert_depth_matches_path(doc: &DocumentIndex) {
    for n in &doc.nodes {
        assert_eq!(
            n.path.depth(),
            depth_of(doc, n.id),
            "path depth mismatch at {}",
            n.id
        );
    }
}

fn depth_of(doc: &DocumentIndex, id: NodeId) -> usize {
    let mut d = 1;
    let mut cur = id;
    while let Some(parent) = doc.nodes[cur.index()].parent {
        d += 1;
        cur = parent;
    }
    d
}

/// No sibling range overlaps any other sibling range (in source order).
fn assert_no_overlap(doc: &DocumentIndex) {
    let mut prev_end = doc.body_offset;
    for &root in &doc.roots {
        let r = doc.nodes[root.index()].byte_range;
        assert!(r.start >= prev_end, "root overlaps predecessor");
        prev_end = r.end;
    }
}

/// Serialize/deserialize round-trip produces an equivalent tree.
fn assert_round_trip(doc: &DocumentIndex) {
    let bytes = serde_json::to_vec(doc).unwrap();
    let back: DocumentIndex = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(back.nodes.len(), doc.nodes.len());
    for (a, b) in doc.nodes.iter().zip(back.nodes.iter()) {
        assert_eq!(a.title, b.title);
        assert_eq!(a.level, b.level);
        assert_eq!(a.path, b.path);
        assert_eq!(a.byte_range, b.byte_range);
        assert_eq!(a.children, b.children);
        assert_eq!(a.parent, b.parent);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn siblings_contiguous_and_no_overlap(tree in arb_heading_tree()) {
        let src = render_headings(&tree);
        let doc = build_document(SourceId::new("p"), "p.md", &src).unwrap();
        assert_siblings_contiguous(&doc);
        assert_no_overlap(&doc);
    }

    #[test]
    fn tree_depth_matches_heading_path_length(tree in arb_heading_tree()) {
        let src = render_headings(&tree);
        let doc = build_document(SourceId::new("p"), "p.md", &src).unwrap();
        assert_depth_matches_path(&doc);
    }

    #[test]
    fn serialize_round_trip(tree in arb_heading_tree()) {
        let src = render_headings(&tree);
        let doc = build_document(SourceId::new("p"), "p.md", &src).unwrap();
        assert_round_trip(&doc);
    }

    #[test]
    fn every_byte_in_body_covered_exactly_once(tree in arb_heading_tree()) {
        let src = render_headings(&tree);
        let doc = build_document(SourceId::new("p"), "p.md", &src).unwrap();
        // Each byte in [body_offset, source_len) must be inside exactly one root range.
        for byte in doc.body_offset..doc.source_len {
            let covering: Vec<_> = doc.roots.iter().filter(|&&r| {
                let range: ByteRange = doc.nodes[r.index()].byte_range;
                range.contains(byte)
            }).collect();
            prop_assert_eq!(covering.len(), 1, "byte {} covered {} times", byte, covering.len());
        }
    }
}
