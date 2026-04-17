//! On-disk and in-memory data model for the index.

use crate::access::AccessCounter;
use lore_core::{ByteRange, HeadingPath, Link, NodeId, SourceId};
use serde::{Deserialize, Serialize};

/// A single heading and its body range in a document.
///
/// `Clone` and `PartialEq` derive cleanly because `AccessCounter` wraps the
/// `AtomicU32` with a `Clone` that copies the current value and a
/// `PartialEq` that always returns `true` (access counts don't participate
/// in node equality — see `access.rs`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeadingNode {
    pub id: NodeId,
    pub level: u8,
    pub title: String,
    pub path: HeadingPath,
    /// From the start of this heading's line through the start of the next
    /// sibling (or end-of-document). Includes the heading line itself.
    pub byte_range: ByteRange,
    /// Body of the section — excludes the heading line itself.
    pub content_range: ByteRange,
    /// First-sentence summary, <= 240 chars. Plain text, no markdown.
    pub summary: String,
    /// Links found inside this node's body (not inside any child).
    pub outbound_links: Vec<Link>,
    /// Children of this node, by id.
    pub children: Vec<NodeId>,
    /// Parent, if any. Root nodes have `None`.
    pub parent: Option<NodeId>,
    /// Structural kind of this section, when we can detect one — e.g.
    /// `"dataview"` for sections dominated by an Obsidian Dataview query.
    /// `None` for ordinary prose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Hot-path access counter. Not serialized — counters live in-memory only.
    #[serde(skip, default)]
    pub access_count: AccessCounter,
}

/// Index for a single markdown document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentIndex {
    pub source: SourceId,
    /// Path relative to the corpus root, POSIX separators, no leading `./`.
    pub rel_path: String,
    /// xxh3 of the original file bytes. Used for invalidation during re-index.
    pub file_hash: u64,
    /// Decoded frontmatter, if any.
    pub frontmatter: Option<serde_json::Value>,
    /// Arena of nodes. Index into this vec equals the `NodeId.0`.
    pub nodes: Vec<HeadingNode>,
    /// Top-level nodes (level-1 headings, or first-heading ancestry roots).
    pub roots: Vec<NodeId>,
    /// Total source length in bytes (including frontmatter).
    pub source_len: u32,
    /// Byte offset where the body starts (i.e. where frontmatter ends).
    pub body_offset: u32,
}

impl DocumentIndex {
    pub fn node(&self, id: NodeId) -> Option<&HeadingNode> {
        self.nodes.get(id.index())
    }
    pub fn node_mut(&mut self, id: NodeId) -> Option<&mut HeadingNode> {
        self.nodes.get_mut(id.index())
    }
}
