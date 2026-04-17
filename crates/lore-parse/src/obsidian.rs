//! Obsidian-specific extensions.
//!
//! Currently this module detects Dataview code blocks — fenced code blocks
//! whose info string is `dataview`. These are dynamic queries, not prose,
//! so we surface them as a distinct kind on the owning heading node and
//! skip their bodies when computing summaries.

use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag, TagEnd};

use crate::options::parser_options;

/// Summary token used when a section is dominated by a Dataview query and
/// has no other prose. Callers can replace this with something more
/// user-friendly; storing a marker in the node lets an agent decide.
pub const DATAVIEW_MARKER: &str = "[dataview query]";

/// Return every Dataview code-block range in `body`, shifted by
/// `body_offset` so they reference the original source.
pub fn detect_dataview_ranges(body: &str, body_offset: u32) -> Vec<(u32, u32)> {
    let parser = Parser::new_ext(body, parser_options()).into_offset_iter();

    let mut out = Vec::new();
    let mut open: Option<(usize, bool)> = None;
    for (ev, range) in parser {
        match ev {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(lang))) => {
                let is_dv = lang.trim().eq_ignore_ascii_case("dataview");
                open = Some((range.start, is_dv));
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some((start, is_dv)) = open.take()
                    && is_dv
                {
                    let s = (start as u32).saturating_add(body_offset);
                    let e = (range.end as u32).saturating_add(body_offset);
                    out.push((s, e));
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
    fn dataview_block_is_detected() {
        let body = "# A\n\n```dataview\nTABLE x FROM \"notes\"\n```\n\nrest\n";
        let ranges = detect_dataview_ranges(body, 0);
        assert_eq!(ranges.len(), 1);
        let (s, e) = ranges[0];
        assert!((s as usize) < (e as usize));
        assert!(body[s as usize..e as usize].contains("dataview"));
    }

    #[test]
    fn non_dataview_blocks_are_ignored() {
        let body = "```rust\nfn main() {}\n```\n```python\nprint('x')\n```\n";
        assert!(detect_dataview_ranges(body, 0).is_empty());
    }

    #[test]
    fn dataviewjs_is_not_dataview() {
        // We only match the plain `dataview` infostring. `dataviewjs` has
        // different semantics and Lore doesn't special-case it today.
        let body = "```dataviewjs\ndv.table([], [])\n```\n";
        assert!(detect_dataview_ranges(body, 0).is_empty());
    }

    #[test]
    fn ranges_shift_by_body_offset() {
        let body = "```dataview\nTABLE x\n```\n";
        let ranges = detect_dataview_ranges(body, 100);
        assert_eq!(ranges[0].0, 100);
    }
}
