//! Convert a `ParsedDoc` into a `DocumentIndex`.
//!
//! The algorithm is a single linear walk of the heading stream maintaining a
//! stack of open ancestors. A new heading pops the stack until it finds a
//! strictly-shallower ancestor, then links in as that ancestor's child.
//! Byte ranges are closed as we go: each node's `byte_range` extends from its
//! line start to the next heading of equal-or-shallower depth.

use lore_core::{ByteRange, HeadingPath, Link, NodeId, Result, SourceId};
use lore_parse::{DATAVIEW_MARKER, HeadingEvent, LinkEvent, ParsedDoc, parse_document};

use crate::access::AccessCounter;
use crate::model::{DocumentIndex, HeadingNode};

/// Build a `DocumentIndex` from raw source bytes.
pub fn build_document(
    source: SourceId,
    rel_path: impl Into<String>,
    src: &str,
) -> Result<DocumentIndex> {
    let parsed = parse_document(src)?;
    let file_hash = xxhash_rust::xxh3::xxh3_64(src.as_bytes());
    let body_offset = parsed
        .frontmatter
        .as_ref()
        .map(|_| first_nonfrontmatter_offset(&parsed))
        .unwrap_or(0);

    let doc = assemble(source, rel_path.into(), file_hash, src, parsed, body_offset);
    Ok(doc)
}

fn first_nonfrontmatter_offset(parsed: &ParsedDoc) -> u32 {
    // The earliest heading or link offset is a safe upper bound on the body
    // start. If there are no structural events, fall back to 0 — this is the
    // conservative behavior (treat the whole file as body).
    let min_h = parsed.headings.iter().map(|h| h.offset).min();
    let min_l = parsed.links.iter().map(|l| l.offset).min();
    match (min_h, min_l) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => 0,
    }
}

fn assemble(
    source: SourceId,
    rel_path: String,
    file_hash: u64,
    src: &str,
    parsed: ParsedDoc,
    body_offset: u32,
) -> DocumentIndex {
    let ParsedDoc {
        frontmatter,
        headings,
        links,
        dataview_ranges,
        source_len,
    } = parsed;

    let mut nodes: Vec<HeadingNode> = Vec::with_capacity(headings.len());
    let mut roots: Vec<NodeId> = Vec::new();
    // Stack of ancestor node indices. Each frame holds (node_id, level).
    let mut stack: Vec<(NodeId, u8)> = Vec::with_capacity(6);

    for (idx, h) in headings.iter().enumerate() {
        let id = NodeId(idx as u32);

        // Pop ancestors that are not strictly shallower than this heading.
        while let Some(&(_pid, plevel)) = stack.last() {
            if plevel >= h.level {
                stack.pop();
            } else {
                break;
            }
        }

        let parent = stack.last().map(|&(pid, _)| pid);
        let path = build_path(&nodes, parent, &h.text);

        let node = HeadingNode {
            id,
            level: h.level,
            title: h.text.clone(),
            path,
            // Ranges are finalised in a later pass once all headings are known.
            byte_range: ByteRange::empty(h.offset),
            content_range: ByteRange::empty(h.body_start),
            summary: String::new(),
            outbound_links: Vec::new(),
            children: Vec::new(),
            parent,
            kind: None,
            access_count: AccessCounter::new(),
        };
        nodes.push(node);

        if let Some(pid) = parent {
            nodes[pid.index()].children.push(id);
        } else {
            roots.push(id);
        }

        stack.push((id, h.level));
    }

    finalise_ranges(&mut nodes, &headings, source_len);
    attach_links(&mut nodes, &links);
    attach_dataview(&mut nodes, &dataview_ranges);
    fill_summaries(&mut nodes, src);

    DocumentIndex {
        source,
        rel_path,
        file_hash,
        frontmatter: frontmatter.map(|f| f.data),
        nodes,
        roots,
        source_len,
        body_offset,
    }
}

fn build_path(nodes: &[HeadingNode], parent: Option<NodeId>, title: &str) -> HeadingPath {
    let mut segments: Vec<String> = match parent {
        Some(pid) => nodes[pid.index()].path.0.clone(),
        None => Vec::new(),
    };
    segments.push(title.to_string());
    HeadingPath(segments)
}

/// Close each node's `byte_range` by finding the offset of the next heading
/// at equal-or-shallower level (or end of document).
fn finalise_ranges(nodes: &mut [HeadingNode], headings: &[HeadingEvent], source_len: u32) {
    for i in 0..nodes.len() {
        let my_level = nodes[i].level;
        let mut end = source_len;
        for later in &headings[i + 1..] {
            if later.level <= my_level {
                end = later.offset;
                break;
            }
        }
        let start = nodes[i].byte_range.start;
        nodes[i].byte_range = ByteRange::new(start, end);
        // content_range excludes the heading line itself.
        let body_start = nodes[i].content_range.start.min(end);
        nodes[i].content_range = ByteRange::new(body_start, end);
    }
}

/// Attach each link to the deepest node whose `byte_range` contains it.
fn attach_links(nodes: &mut [HeadingNode], links: &[LinkEvent]) {
    if nodes.is_empty() {
        return;
    }
    for l in links {
        let Some(idx) = node_containing(nodes, l.offset) else {
            continue;
        };
        nodes[idx].outbound_links.push(Link {
            target: l.target.clone(),
            text: l.text.clone(),
            kind: l.kind,
            offset: l.offset,
        });
    }
}

/// Return the index of the deepest node whose `byte_range` contains `offset`.
///
/// Nodes are in source order; children always start strictly later than
/// their parent, so the last node whose range contains the offset is the
/// deepest. Shared by link and Dataview attachment — both want the
/// narrowest enclosing section.
fn node_containing(nodes: &[HeadingNode], offset: u32) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (i, n) in nodes.iter().enumerate() {
        if n.byte_range.contains(offset) {
            best = Some(i);
        } else if offset < n.byte_range.start {
            break;
        }
    }
    best
}

fn fill_summaries(nodes: &mut [HeadingNode], src: &str) {
    let bytes = src.as_bytes();
    for n in nodes.iter_mut() {
        let start = n.content_range.start as usize;
        let end = n.content_range.end as usize;
        if start >= end || end > bytes.len() {
            continue;
        }
        let slice = &bytes[start..end];
        if let Ok(s) = std::str::from_utf8(slice) {
            let sentence = lore_parse::first_sentence(s);
            // If the section is dominated by a Dataview query (no prose
            // outside the block), the flattened summary will be empty —
            // surface a short marker so agents don't see a blank field.
            if sentence.is_empty() && n.kind.as_deref() == Some("dataview") {
                n.summary = DATAVIEW_MARKER.to_string();
            } else {
                n.summary = sentence;
            }
        }
    }
}

/// Tag any section whose `byte_range` contains a Dataview block with
/// `kind = "dataview"`. If parent *and* child both contain the block we
/// credit the deepest node — the one the author wrote the block under.
fn attach_dataview(nodes: &mut [HeadingNode], ranges: &[(u32, u32)]) {
    if nodes.is_empty() || ranges.is_empty() {
        return;
    }
    for &(start, _end) in ranges {
        if let Some(i) = node_containing(nodes, start) {
            nodes[i].kind = Some("dataview".to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(src: &str) -> DocumentIndex {
        build_document(SourceId::new("test"), "t.md", src).unwrap()
    }

    #[test]
    fn single_heading_document() {
        let src = "# Only\n\nbody.\n";
        let doc = build(src);
        assert_eq!(doc.nodes.len(), 1);
        let n = &doc.nodes[0];
        assert_eq!(n.level, 1);
        assert_eq!(n.title, "Only");
        assert_eq!(n.byte_range.end, src.len() as u32);
    }

    #[test]
    fn nested_headings_build_tree() {
        let src = "# A\n\n## B\n\n### C\n\n## D\n\n# E\n";
        let doc = build(src);
        assert_eq!(doc.nodes.len(), 5);
        assert_eq!(doc.roots, vec![NodeId(0), NodeId(4)]);
        assert_eq!(doc.nodes[0].children, vec![NodeId(1), NodeId(3)]);
        assert_eq!(doc.nodes[1].children, vec![NodeId(2)]);
        assert_eq!(doc.nodes[4].children, vec![]);
        assert_eq!(doc.nodes[2].path.to_string(), "A > B > C");
    }

    #[test]
    fn byte_ranges_are_contiguous_at_each_level() {
        let src = "# A\n\nbody\n\n# B\n\nmore\n";
        let doc = build(src);
        assert_eq!(doc.nodes[0].byte_range.end, doc.nodes[1].byte_range.start);
        assert_eq!(doc.nodes[1].byte_range.end, src.len() as u32);
    }

    #[test]
    fn link_attached_to_deepest_node() {
        let src = "# A\n\n## B\n\nsee [x](y)\n";
        let doc = build(src);
        assert_eq!(doc.nodes[0].outbound_links.len(), 0);
        assert_eq!(doc.nodes[1].outbound_links.len(), 1);
        assert_eq!(doc.nodes[1].outbound_links[0].target, "y");
    }

    #[test]
    fn summary_is_first_sentence() {
        let src = "# H\n\nHook sentence. Extra prose.\n";
        let doc = build(src);
        assert_eq!(doc.nodes[0].summary, "Hook sentence.");
    }

    #[test]
    fn skipped_heading_level_still_parents_correctly() {
        // H1 -> H3 should treat H3 as child of H1 even though H2 is missing.
        let src = "# A\n\n### C\n";
        let doc = build(src);
        assert_eq!(doc.nodes[1].parent, Some(NodeId(0)));
        assert_eq!(doc.nodes[0].children, vec![NodeId(1)]);
    }
}
