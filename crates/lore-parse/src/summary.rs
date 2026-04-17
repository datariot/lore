//! First-sentence summary extraction.
//!
//! The agent calling `get_section` wants enough context to decide whether to
//! read further. We avoid LLM summarization entirely: the first sentence of a
//! section is almost always a usable hook. If we can't find a sentence break,
//! fall back to the first 240 chars of flattened text.

use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use unicode_segmentation::UnicodeSegmentation;

use crate::options::parser_options;

pub const SUMMARY_MAX: usize = 240;

/// Extract a short, plain-text summary from a markdown slice.
///
/// The slice should be the *body* of a section (without the heading line).
pub fn first_sentence(body: &str) -> String {
    let text = flatten_text(body);
    first_sentence_of(&text)
}

fn first_sentence_of(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    // Scan for ". ", "! ", "? " or a double newline, whichever comes first.
    let mut best = trimmed.len();
    for (i, c) in trimmed.char_indices() {
        if matches!(c, '.' | '!' | '?') {
            let rest = &trimmed[i + c.len_utf8()..];
            if rest.starts_with(char::is_whitespace) || rest.is_empty() {
                best = i + c.len_utf8();
                break;
            }
        }
    }
    let sentence = &trimmed[..best];
    truncate_on_grapheme(sentence, SUMMARY_MAX)
}

fn truncate_on_grapheme(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.trim().to_string();
    }
    let mut end = 0;
    for (idx, _) in s.grapheme_indices(true) {
        if idx > max {
            break;
        }
        end = idx;
    }
    let mut out = s[..end].trim_end().to_string();
    out.push('…');
    out
}

fn flatten_text(body: &str) -> String {
    let parser = Parser::new_ext(body, parser_options());
    let mut out = String::new();
    let mut in_code_block = false;
    for ev in parser {
        match ev {
            Event::Start(Tag::CodeBlock(_)) => in_code_block = true,
            Event::End(TagEnd::CodeBlock) => {
                in_code_block = false;
                if !out.ends_with(' ') {
                    out.push(' ');
                }
            }
            Event::Text(t) if !in_code_block => out.push_str(&t),
            Event::Code(t) if !in_code_block => out.push_str(&t),
            Event::SoftBreak | Event::HardBreak => {
                if !out.ends_with(' ') {
                    out.push(' ');
                }
            }
            Event::End(TagEnd::Paragraph) => {
                if !out.ends_with(' ') {
                    out.push(' ');
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_first_sentence() {
        let body = "This is the hook. This is the rest of the body.\n";
        assert_eq!(first_sentence(body), "This is the hook.");
    }

    #[test]
    fn skips_code_blocks() {
        let body = "```rust\nfn main() {}\n```\nThe real sentence here.\n";
        assert_eq!(first_sentence(body), "The real sentence here.");
    }

    #[test]
    fn truncates_when_no_sentence_break() {
        let body = "a".repeat(400);
        let s = first_sentence(&body);
        assert!(s.len() <= SUMMARY_MAX + 4); // +4 for the ellipsis bytes
        assert!(s.ends_with('…'));
    }

    #[test]
    fn empty_returns_empty() {
        assert_eq!(first_sentence(""), "");
        assert_eq!(first_sentence("   \n\n"), "");
    }
}
