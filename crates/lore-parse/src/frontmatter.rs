//! YAML frontmatter extraction.
//!
//! Markdown files in Obsidian and most static-site generators open with a
//! YAML block delimited by `---` on its own line. We peel that off before
//! parsing so the markdown parser only sees body content — and so every byte
//! offset we emit for headings and links references the original source.

use gray_matter::Matter;
use gray_matter::engine::YAML;
use serde_json::Value;

/// Decoded frontmatter plus its raw text.
#[derive(Debug, Clone)]
pub struct Frontmatter {
    /// The raw YAML text, with `---` delimiters stripped.
    pub raw: String,
    /// Decoded as a JSON value. Opaque at this layer; callers can project it.
    pub data: Value,
}

/// Split `src` into `(frontmatter, body, body_offset)`.
///
/// `body_offset` is the byte index in `src` where the body begins. For a file
/// with no frontmatter this is `0`.
pub fn split_frontmatter(src: &str) -> (Option<Frontmatter>, &str, u32) {
    if !src.starts_with("---") {
        return (None, src, 0);
    }

    // gray_matter handles the delimiter detection and YAML parsing. We use it
    // for correctness but we also need the body offset into the *original*
    // string so we recompute that ourselves.
    let matter: Matter<YAML> = Matter::new();
    let parsed = matter.parse(src);

    if parsed.matter.is_empty() {
        return (None, src, 0);
    }

    // Find the second `---` that closes the block so we can compute body_offset.
    // The opening delimiter is the first three bytes; search for the next one.
    let Some(body_offset) = find_body_offset(src) else {
        return (None, src, 0);
    };

    // Destructure once and move; no need to clone `matter` when we own it.
    let raw = parsed.matter;
    let data = parsed
        .data
        .and_then(|pod| pod.deserialize::<Value>().ok())
        .unwrap_or(Value::Null);

    let body = &src[body_offset as usize..];
    (Some(Frontmatter { raw, data }), body, body_offset)
}

fn find_body_offset(src: &str) -> Option<u32> {
    // Must open with "---" followed by newline or end-of-string.
    let bytes = src.as_bytes();
    if !bytes.starts_with(b"---") {
        return None;
    }
    let mut cursor = 3;
    // Skip CR/LF after opening delimiter.
    if bytes.get(cursor) == Some(&b'\r') {
        cursor += 1;
    }
    if bytes.get(cursor) == Some(&b'\n') {
        cursor += 1;
    } else if bytes.len() != cursor {
        return None;
    }

    // Scan line by line for a closing `---` on its own line.
    while cursor < bytes.len() {
        let line_end = bytes[cursor..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|n| cursor + n)
            .unwrap_or(bytes.len());
        let line = &bytes[cursor..line_end];
        let trimmed = trim_cr(line);
        if trimmed == b"---" || trimmed == b"..." {
            let after = line_end + if line_end < bytes.len() { 1 } else { 0 };
            return Some(after as u32);
        }
        cursor = line_end + 1;
    }
    None
}

fn trim_cr(line: &[u8]) -> &[u8] {
    if line.last() == Some(&b'\r') {
        &line[..line.len() - 1]
    } else {
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_frontmatter_returns_src_unchanged() {
        let src = "# Hi\n";
        let (fm, body, off) = split_frontmatter(src);
        assert!(fm.is_none());
        assert_eq!(body, src);
        assert_eq!(off, 0);
    }

    #[test]
    fn yaml_frontmatter_peeled() {
        let src = "---\ntitle: Hello\ntags:\n  - a\n---\n# Body\n";
        let (fm, body, off) = split_frontmatter(src);
        let fm = fm.expect("should have frontmatter");
        assert_eq!(fm.data["title"], "Hello");
        assert!(body.starts_with("# Body"));
        // The offset must point to the start of "# Body" in the original src.
        assert_eq!(&src[off as usize..off as usize + 6], "# Body");
    }

    #[test]
    fn handles_crlf_line_endings() {
        let src = "---\r\ntitle: x\r\n---\r\n# Body\r\n";
        let (fm, body, off) = split_frontmatter(src);
        assert!(fm.is_some());
        assert!(body.starts_with("# Body"));
        assert_eq!(&src[off as usize..off as usize + 6], "# Body");
    }

    #[test]
    fn unterminated_frontmatter_falls_through() {
        let src = "---\ntitle: x\n# No close\n";
        let (fm, body, off) = split_frontmatter(src);
        assert!(fm.is_none());
        assert_eq!(off, 0);
        assert_eq!(body, src);
    }
}
