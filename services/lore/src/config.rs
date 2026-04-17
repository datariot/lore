//! Filesystem layout conventions.

use std::path::{Component, Path, PathBuf};

/// Directory inside a corpus root that holds Lore's on-disk state.
pub const LORE_DIR: &str = ".lore";

/// Filename for the serialized corpus index.
pub const INDEX_FILE: &str = "index.json";

/// Default markdown extensions to include.
pub const MARKDOWN_EXTENSIONS: &[&str] = &["md", "markdown", "mdx", "mkd"];

/// Return the canonical on-disk path for a corpus index given its root.
pub fn index_path(root: &Path) -> PathBuf {
    root.join(LORE_DIR).join(INDEX_FILE)
}

/// Derive a default `SourceId` string from the corpus root path.
///
/// Uses the basename; falls back to "root" for paths without one (e.g. `/`).
pub fn default_source_id(root: &Path) -> String {
    root.file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "root".to_string())
}

/// Render a path as a POSIX-style string, dropping any `.` / `..` / root
/// components. Used wherever we normalize a `rel_path` for display or
/// for storing in the corpus index — forward slashes, no leading `./`.
pub fn rel_to_posix(rel: &Path) -> String {
    rel.components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str().map(|s| s.to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Compute a POSIX-form `rel_path` relative to a corpus root. If `path`
/// is outside `root`, returns the path unchanged (POSIX-rendered).
pub fn rel_path(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel_to_posix(rel)
}
