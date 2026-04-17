//! Markdown parsing for Lore.
//!
//! The output of this crate is a flat `ParsedDoc` containing:
//!
//! - optional YAML frontmatter (deserialized as JSON),
//! - a sequence of heading events with byte offsets into the *original* source
//!   (not the post-frontmatter body), and
//! - a sequence of link events (inline markdown links + Obsidian wiki-links)
//!   with byte offsets into the original source.
//!
//! Tree building is deliberately deferred to `lore-index` so this crate remains
//! pure, testable, and free of policy decisions about how to group events.

#![deny(unsafe_op_in_unsafe_fn)]

mod frontmatter;
mod headings;
mod links;
mod obsidian;
mod options;
mod summary;

pub use frontmatter::{Frontmatter, split_frontmatter};
pub use headings::HeadingEvent;
pub use links::{LinkEvent, extract_links};
pub use obsidian::{DATAVIEW_MARKER, detect_dataview_ranges};
pub use options::parser_options;
pub use summary::first_sentence;

use lore_core::Result;

/// The full structural parse of a markdown document.
#[derive(Debug, Clone, Default)]
pub struct ParsedDoc {
    /// Raw frontmatter source (with delimiters stripped) and decoded JSON value.
    pub frontmatter: Option<Frontmatter>,
    /// Headings in source order. Offsets are relative to the *original* source.
    pub headings: Vec<HeadingEvent>,
    /// Links in source order. Offsets are relative to the *original* source.
    pub links: Vec<LinkEvent>,
    /// Byte ranges of Obsidian Dataview blocks. Half-open `[start, end)`.
    pub dataview_ranges: Vec<(u32, u32)>,
    /// Length of the original source in bytes.
    pub source_len: u32,
}

/// Parse a markdown document into its structural events.
///
/// The returned offsets reference the *original* `src`. Callers never need to
/// track the frontmatter split separately.
pub fn parse_document(src: &str) -> Result<ParsedDoc> {
    let (fm, body, body_offset) = frontmatter::split_frontmatter(src);
    let headings = headings::collect(body, body_offset);
    let links = links::extract_links(body, body_offset);
    let dataview_ranges = obsidian::detect_dataview_ranges(body, body_offset);
    Ok(ParsedDoc {
        frontmatter: fm,
        headings,
        links,
        dataview_ranges,
        source_len: src.len() as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_doc() {
        let src = "# Title\n\nparagraph\n\n## Child\n\nmore\n";
        let doc = parse_document(src).unwrap();
        assert!(doc.frontmatter.is_none());
        assert_eq!(doc.headings.len(), 2);
        assert_eq!(doc.headings[0].level, 1);
        assert_eq!(doc.headings[0].text, "Title");
        assert_eq!(doc.headings[1].level, 2);
        assert_eq!(doc.headings[1].text, "Child");
    }

    #[test]
    fn offsets_reference_original_source_even_with_frontmatter() {
        let src = "---\ntitle: Hi\n---\n# Heading\n";
        let doc = parse_document(src).unwrap();
        assert!(doc.frontmatter.is_some());
        assert_eq!(doc.headings.len(), 1);
        let h = &doc.headings[0];
        assert_eq!(&src.as_bytes()[h.offset as usize..][..1], b"#");
    }
}
