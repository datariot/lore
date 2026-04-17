//! Heading event extraction.
//!
//! We walk `pulldown-cmark`'s offset-bearing event stream and emit one
//! `HeadingEvent` per ATX/setext heading we encounter. Offsets are shifted by
//! `body_offset` so they reference the original source (with frontmatter).

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};

use crate::options::parser_options;

/// A heading discovered in source order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadingEvent {
    /// Heading depth, 1–6.
    pub level: u8,
    /// Heading text with inline formatting stripped (plain text only).
    pub text: String,
    /// Byte offset of the heading *line start* in the original source.
    pub offset: u32,
    /// Byte offset immediately after the heading text + its trailing newline.
    pub body_start: u32,
}

/// Collect every heading from `body`, shifted by `body_offset`.
pub fn collect(body: &str, body_offset: u32) -> Vec<HeadingEvent> {
    let parser = Parser::new_ext(body, parser_options()).into_offset_iter();

    let mut out = Vec::new();
    let mut current: Option<InProgress> = None;

    for (event, range) in parser {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                current = Some(InProgress {
                    level: heading_level(level),
                    start: range.start,
                    end: range.end,
                    text: String::new(),
                });
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(cur) = current.take() {
                    out.push(HeadingEvent {
                        level: cur.level,
                        text: normalize_whitespace(&cur.text),
                        offset: (cur.start as u32).saturating_add(body_offset),
                        body_start: (cur.end as u32).saturating_add(body_offset),
                    });
                }
            }
            Event::Text(t) | Event::Code(t) => {
                if let Some(cur) = current.as_mut() {
                    cur.text.push_str(&t);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(cur) = current.as_mut() {
                    cur.text.push(' ');
                }
            }
            _ => {}
        }
    }

    out
}

struct InProgress {
    level: u8,
    start: usize,
    end: usize,
    text: String,
}

fn heading_level(l: HeadingLevel) -> u8 {
    match l {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn normalize_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_space && !out.is_empty() {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atx_headings_detected() {
        let src = "# A\n\n## B\n\n### C\n";
        let hs = collect(src, 0);
        assert_eq!(hs.len(), 3);
        assert_eq!(hs[0].level, 1);
        assert_eq!(hs[0].text, "A");
        assert_eq!(hs[2].level, 3);
        assert_eq!(hs[2].text, "C");
    }

    #[test]
    fn inline_code_in_heading_is_included_as_text() {
        let src = "## Use `cargo test`\n";
        let hs = collect(src, 0);
        assert_eq!(hs[0].text, "Use cargo test");
    }

    #[test]
    fn offsets_are_shifted_by_body_offset() {
        let body = "# Title\n";
        let shifted = collect(body, 100);
        assert_eq!(shifted[0].offset, 100);
    }

    #[test]
    fn setext_headings_detected() {
        let src = "Title\n=====\n\nBody\n";
        let hs = collect(src, 0);
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].level, 1);
        assert_eq!(hs[0].text, "Title");
    }
}
