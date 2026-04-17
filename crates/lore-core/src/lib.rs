//! Shared types for the Lore markdown-index MCP server.
//!
//! This crate carries only value types and error definitions. It has no I/O,
//! no parsing, and no ranking logic — those live in `lore-parse`,
//! `lore-index`, and `lore-search` respectively.

#![deny(unsafe_op_in_unsafe_fn)]

use std::fmt;

use serde::{Deserialize, Serialize};

/// Identifier for a corpus — typically the basename of the root directory.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceId(pub String);

impl SourceId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Dense per-document identifier for a heading node.
///
/// Stored as `u32` because a single markdown document is vanishingly unlikely
/// to contain more than four billion headings and halving the width keeps the
/// arena cache-friendly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub u32);

impl NodeId {
    pub const ROOT: NodeId = NodeId(0);
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}", self.0)
    }
}

/// Full heading ancestry, root to leaf, e.g. `["Engineering", "Rust", "Tokio"]`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HeadingPath(pub Vec<String>);

impl HeadingPath {
    pub fn new<I, S>(segments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self(segments.into_iter().map(Into::into).collect())
    }

    pub fn push(&mut self, segment: impl Into<String>) {
        self.0.push(segment.into());
    }

    pub fn depth(&self) -> usize {
        self.0.len()
    }

    pub fn leaf(&self) -> Option<&str> {
        self.0.last().map(String::as_str)
    }

    pub fn segments(&self) -> &[String] {
        &self.0
    }
}

impl fmt::Display for HeadingPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for seg in &self.0 {
            if !first {
                f.write_str(" > ")?;
            }
            first = false;
            f.write_str(seg)?;
        }
        Ok(())
    }
}

/// Half-open byte range `[start, end)` into a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteRange {
    pub start: u32,
    pub end: u32,
}

impl ByteRange {
    pub fn new(start: u32, end: u32) -> Self {
        debug_assert!(start <= end, "ByteRange start ({start}) > end ({end})");
        Self { start, end }
    }

    pub fn empty(at: u32) -> Self {
        Self { start: at, end: at }
    }

    pub fn len(self) -> u32 {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(self) -> bool {
        self.end == self.start
    }

    pub fn contains(self, offset: u32) -> bool {
        offset >= self.start && offset < self.end
    }

    pub fn slice(self, src: &[u8]) -> &[u8] {
        &src[self.start as usize..self.end as usize]
    }
}

/// A link discovered inside a document, paired with the node it appears under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    /// Raw target as it appears in source (e.g. `docs/architecture.md#Caching`
    /// or `Some Page|alias`).
    pub target: String,
    /// Display text, if distinct from `target`.
    pub text: Option<String>,
    /// Which flavor of link this is.
    pub kind: LinkKind,
    /// Byte offset of the link in the source file.
    pub offset: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    /// `[text](target)` CommonMark inline link.
    Inline,
    /// `[[target]]` or `[[target|alias]]` Obsidian-style wiki-link.
    Wiki,
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("parse error: {0}")]
    Parse(String),

    #[error("io error: {0}")]
    Io(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("serialization error: {0}")]
    Serialize(#[from] serde_json::Error),
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_path_display_joins_with_arrow() {
        let p = HeadingPath::new(["A", "B", "C"]);
        assert_eq!(p.to_string(), "A > B > C");
        assert_eq!(p.depth(), 3);
        assert_eq!(p.leaf(), Some("C"));
    }

    #[test]
    fn byte_range_slice_and_len() {
        let r = ByteRange::new(2, 5);
        assert_eq!(r.len(), 3);
        assert_eq!(r.slice(b"hello world"), b"llo");
        assert!(!r.is_empty());
        assert!(ByteRange::empty(4).is_empty());
    }

    #[test]
    fn node_id_pads_for_display() {
        assert_eq!(NodeId(42).to_string(), "0042");
    }
}
