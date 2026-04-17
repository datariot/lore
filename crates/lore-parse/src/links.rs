//! Link extraction.
//!
//! Two shapes:
//!
//! - **Inline** — `[text](target)`, surfaced by pulldown-cmark as
//!   `Event::Start(Tag::Link { .. })`.
//! - **Wiki** — `[[target]]` or `[[target|alias]]`, an Obsidian-ism that
//!   CommonMark does not know about. We scan for these with a regex over the
//!   body, then filter out any hits that fell inside a fenced code block by
//!   replaying the event stream.

use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use regex::Regex;
use std::sync::OnceLock;

use crate::options::parser_options;
use lore_core::LinkKind;

/// A link discovered during parsing. Offsets reference the *original* source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkEvent {
    pub target: String,
    pub text: Option<String>,
    pub kind: LinkKind,
    pub offset: u32,
}

pub fn extract_links(body: &str, body_offset: u32) -> Vec<LinkEvent> {
    let mut out = Vec::new();
    extract_inline(body, body_offset, &mut out);
    extract_wiki(body, body_offset, &mut out);
    out.sort_by_key(|l| l.offset);
    out
}

fn extract_inline(body: &str, body_offset: u32, out: &mut Vec<LinkEvent>) {
    let parser = Parser::new_ext(body, parser_options()).into_offset_iter();
    let mut open_link: Option<OpenLink> = None;
    for (event, range) in parser {
        match event {
            Event::Start(Tag::Link { dest_url, .. }) => {
                open_link = Some(OpenLink {
                    target: dest_url.to_string(),
                    text: String::new(),
                    start: range.start,
                });
            }
            Event::Text(t) => {
                if let Some(o) = open_link.as_mut() {
                    o.text.push_str(&t);
                }
            }
            Event::End(TagEnd::Link) => {
                if let Some(o) = open_link.take() {
                    let text = if o.text.is_empty() || o.text == o.target {
                        None
                    } else {
                        Some(o.text)
                    };
                    out.push(LinkEvent {
                        target: o.target,
                        text,
                        kind: LinkKind::Inline,
                        offset: (o.start as u32).saturating_add(body_offset),
                    });
                }
            }
            _ => {}
        }
    }
}

struct OpenLink {
    target: String,
    text: String,
    start: usize,
}

fn extract_wiki(body: &str, body_offset: u32, out: &mut Vec<LinkEvent>) {
    let re = wiki_regex();
    let code_mask = build_code_mask(body);
    for cap in re.captures_iter(body) {
        let m = cap.get(0).unwrap();
        if code_mask[m.start()] {
            continue;
        }
        let inner = cap.get(1).unwrap().as_str();
        let (target, text) = if let Some(pipe) = inner.find('|') {
            (
                inner[..pipe].trim().to_string(),
                Some(inner[pipe + 1..].trim().to_string()),
            )
        } else {
            (inner.trim().to_string(), None)
        };
        if target.is_empty() {
            continue;
        }
        out.push(LinkEvent {
            target,
            text,
            kind: LinkKind::Wiki,
            offset: (m.start() as u32).saturating_add(body_offset),
        });
    }
}

fn wiki_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\[\[([^\[\]\n]+)\]\]").unwrap())
}

/// A byte-indexed boolean mask: `true` means "inside a fenced code block or
/// inline code span". We replay pulldown-cmark so we exclude wiki-links that
/// are actually just examples printed in a code block.
fn build_code_mask(body: &str) -> Vec<bool> {
    let len = body.len();
    let mut mask = vec![false; len + 1];
    let parser = Parser::new_ext(body, parser_options()).into_offset_iter();

    let mut depth = 0u32;
    for (event, range) in parser {
        match event {
            Event::Start(Tag::CodeBlock(_)) => {
                depth += 1;
                mark(&mut mask, range.start, range.end);
            }
            Event::End(TagEnd::CodeBlock) => {
                depth = depth.saturating_sub(1);
            }
            Event::Code(_) => mark(&mut mask, range.start, range.end),
            _ => {
                if depth > 0 {
                    mark(&mut mask, range.start, range.end);
                }
            }
        }
    }
    mask
}

fn mark(mask: &mut [bool], start: usize, end: usize) {
    let end = end.min(mask.len());
    for b in &mut mask[start..end] {
        *b = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_link_with_text() {
        let body = "see [the docs](https://example.com)";
        let links = extract_links(body, 0);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].kind, LinkKind::Inline);
        assert_eq!(links[0].target, "https://example.com");
        assert_eq!(links[0].text.as_deref(), Some("the docs"));
    }

    #[test]
    fn wiki_link_with_alias() {
        let body = "hello [[Some Page|its alias]] world";
        let links = extract_links(body, 0);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].kind, LinkKind::Wiki);
        assert_eq!(links[0].target, "Some Page");
        assert_eq!(links[0].text.as_deref(), Some("its alias"));
    }

    #[test]
    fn wiki_link_in_code_fence_is_ignored() {
        let body = "```\n[[should_not_count]]\n```\n[[real_one]]\n";
        let links = extract_links(body, 0);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "real_one");
    }

    #[test]
    fn wiki_link_in_inline_code_is_ignored() {
        let body = "Use `[[code]]` or [[real]]";
        let links = extract_links(body, 0);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "real");
    }

    #[test]
    fn offsets_shifted_by_body_offset() {
        let body = "[x](y)";
        let links = extract_links(body, 100);
        assert_eq!(links[0].offset, 100);
    }
}
