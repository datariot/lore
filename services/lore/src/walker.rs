//! Directory walking with gitignore awareness.
//!
//! We use the `ignore` crate (the same one ripgrep uses) because it already
//! handles `.gitignore`, `.ignore`, hidden files, symlink loops, and parallel
//! iteration. We layer on extension filtering and exclude Lore's own state
//! directory (`.lore`) so re-indexing never picks up the previous index file.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ignore::{WalkBuilder, WalkState};

use crate::config::{LORE_DIR, MARKDOWN_EXTENSIONS};

pub use crate::config::rel_path;

#[derive(Debug, Clone)]
pub struct WalkOptions {
    pub follow_links: bool,
    pub respect_gitignore: bool,
    pub include_hidden: bool,
    pub extensions: Vec<String>,
}

impl Default for WalkOptions {
    fn default() -> Self {
        Self {
            follow_links: false,
            respect_gitignore: true,
            include_hidden: false,
            extensions: MARKDOWN_EXTENSIONS.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// Walk `root` and return every markdown file path in a deterministic order.
///
/// Deterministic = sorted lexicographically. This keeps index output
/// reproducible across runs — valuable for diffing and for CI-stable benches.
pub fn walk_markdown(root: &Path, opts: &WalkOptions) -> Vec<PathBuf> {
    let mut out = walk_markdown_parallel(root, opts);
    out.sort();
    out
}

fn walk_markdown_parallel(root: &Path, opts: &WalkOptions) -> Vec<PathBuf> {
    use std::sync::Mutex;

    let hits: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
    // Normalize to lowercase once, share across worker threads. `Arc<[_]>`
    // is a cheap clone (refcount bump) while `Vec<String>::clone` would
    // re-allocate the backing storage per thread.
    let extensions: Arc<[String]> = opts
        .extensions
        .iter()
        .map(|s| s.to_lowercase())
        .collect::<Vec<_>>()
        .into();

    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(!opts.include_hidden)
        .git_ignore(opts.respect_gitignore)
        .git_exclude(opts.respect_gitignore)
        .git_global(opts.respect_gitignore)
        .require_git(false)
        .follow_links(opts.follow_links)
        .filter_entry(move |entry| {
            // Never recurse into Lore's own state directory.
            entry.file_name() != LORE_DIR
        });

    builder.build_parallel().run(|| {
        let hits = &hits;
        let extensions = extensions.clone();
        Box::new(move |result| {
            let Ok(entry) = result else {
                return WalkState::Continue;
            };
            let Some(ft) = entry.file_type() else {
                return WalkState::Continue;
            };
            if !ft.is_file() {
                return WalkState::Continue;
            }
            if !has_matching_extension(entry.path(), &extensions) {
                return WalkState::Continue;
            }
            if let Ok(mut g) = hits.lock() {
                g.push(entry.path().to_path_buf());
            }
            WalkState::Continue
        })
    });

    hits.into_inner().unwrap_or_default()
}

fn has_matching_extension(path: &Path, extensions: &[String]) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    let lower = ext.to_lowercase();
    extensions.contains(&lower)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn finds_markdown_files_respecting_extensions() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.md"), "# a\n").unwrap();
        fs::write(dir.path().join("b.txt"), "nope").unwrap();
        fs::write(dir.path().join("c.markdown"), "# c\n").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/d.md"), "# d\n").unwrap();

        let hits = walk_markdown(dir.path(), &WalkOptions::default());
        assert_eq!(hits.len(), 3);
        assert!(hits[0].ends_with("a.md"));
        assert!(hits.last().unwrap().ends_with("d.md"));
    }

    #[test]
    fn respects_gitignore_by_default() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(".gitignore"), "ignored.md\n").unwrap();
        fs::write(dir.path().join("kept.md"), "# k\n").unwrap();
        fs::write(dir.path().join("ignored.md"), "# i\n").unwrap();

        let hits = walk_markdown(dir.path(), &WalkOptions::default());
        assert_eq!(hits.len(), 1);
        assert!(hits[0].ends_with("kept.md"));
    }

    #[test]
    fn skips_lore_state_directory() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.md"), "# a\n").unwrap();
        fs::create_dir(dir.path().join(".lore")).unwrap();
        fs::write(dir.path().join(".lore/index.json"), "{}").unwrap();
        // .lore is hidden AND filter-entry excluded; it should never appear
        // even if we flip include_hidden.
        let opts = WalkOptions {
            include_hidden: true,
            ..WalkOptions::default()
        };
        let hits = walk_markdown(dir.path(), &opts);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].ends_with("a.md"));
    }

    #[test]
    fn rel_path_uses_forward_slashes() {
        let root = Path::new("/tmp/kb");
        let file = Path::new("/tmp/kb/docs/intro.md");
        assert_eq!(rel_path(root, file), "docs/intro.md");
    }
}
