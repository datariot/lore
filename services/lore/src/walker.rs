//! Directory walking with gitignore awareness.
//!
//! We use the `ignore` crate (the same one ripgrep uses) because it already
//! handles `.gitignore`, `.ignore`, hidden files, symlink loops, and parallel
//! iteration. We layer on extension filtering and exclude Lore's own state
//! directory (`.lore`) so re-indexing never picks up the previous index file.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::{Match, WalkBuilder, WalkState};
use parking_lot::Mutex;

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

/// Decide, for a *single* path, whether `walk_markdown` would have yielded it.
///
/// The bulk walk gets gitignore and hidden-file handling for free from the
/// `ignore` crate's tree traversal. The watcher does not — it is handed one
/// absolute path at a time by the OS and has no walk to piggyback on. Without
/// this, a file the indexer deliberately skipped gets spliced into the corpus
/// the moment it is written: `.claude/worktrees/` is hidden, so a `git
/// worktree add` inside a watched root silently doubled a vault (KB, 2026-08-13).
///
/// `PathFilter` reproduces those rules for one path. It is a *mirror*, and
/// mirrors drift, so `filter_agrees_with_walk_on_a_mixed_tree` walks a fixture
/// tree and asserts the two verdicts match file for file.
///
/// Gitignore matchers are built lazily per directory and cached. Call
/// [`PathFilter::invalidate`] when an ignore file changes on disk.
pub struct PathFilter {
    root: PathBuf,
    opts: WalkOptions,
    extensions: Vec<String>,
    global: Arc<Gitignore>,
    /// Keyed by absolute directory path.
    dir_matchers: Mutex<HashMap<PathBuf, Arc<Gitignore>>>,
}

/// Filenames that carry ignore rules. A change to one of these invalidates
/// every cached matcher.
pub const IGNORE_FILES: &[&str] = &[".gitignore", ".ignore"];

impl PathFilter {
    pub fn new(root: impl Into<PathBuf>, opts: WalkOptions) -> Self {
        let extensions = opts.extensions.iter().map(|s| s.to_lowercase()).collect();
        // `Gitignore::global` reads core.excludesFile. It reports a partial
        // error rather than failing; an unreadable global file just means no
        // global rules, which is the same as not having one.
        let global = if opts.respect_gitignore {
            Gitignore::global().0
        } else {
            Gitignore::empty()
        };
        Self {
            root: root.into(),
            opts,
            extensions,
            global: Arc::new(global),
            dir_matchers: Mutex::new(HashMap::new()),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Drop every cached gitignore matcher. Cheap; the next `accepts` rebuilds
    /// only the directories it actually needs.
    pub fn invalidate(&self) {
        self.dir_matchers.lock().clear();
    }

    /// True if `path` is one of the files whose contents this filter caches.
    pub fn is_ignore_file(path: &Path) -> bool {
        path.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| IGNORE_FILES.contains(&n))
    }

    /// Would `walk_markdown(self.root, self.opts)` have yielded `path`?
    ///
    /// `path` is expected to be absolute and under `root`; anything outside
    /// the root is rejected, since the walk could not have reached it.
    pub fn accepts(&self, path: &Path) -> bool {
        if !has_matching_extension(path, &self.extensions) {
            return false;
        }
        let Ok(rel) = path.strip_prefix(&self.root) else {
            return false;
        };

        let components: Vec<&std::ffi::OsStr> = rel
            .components()
            .filter_map(|c| match c {
                Component::Normal(s) => Some(s),
                _ => None,
            })
            .collect();
        if components.is_empty() {
            return false;
        }

        // Lore's own state directory is excluded unconditionally — the walk
        // does it with `filter_entry`, which outranks `include_hidden`.
        if components.iter().any(|c| *c == LORE_DIR) {
            return false;
        }
        if !self.opts.include_hidden && components.iter().any(|c| is_hidden(c)) {
            return false;
        }
        if !self.opts.respect_gitignore {
            return true;
        }

        !self.gitignored(&components)
    }

    /// Walk `root` downward applying each directory's ignore rules to the
    /// component beneath it, exactly as git does: a matcher applies to
    /// everything below its own directory, deeper matchers take precedence,
    /// and an ignored *directory* prunes the whole subtree.
    fn gitignored(&self, components: &[&std::ffi::OsStr]) -> bool {
        let mut matchers: Vec<Arc<Gitignore>> = vec![self.global.clone()];
        let mut dir = self.root.clone();
        matchers.push(self.matcher_for(&dir));

        let last = components.len() - 1;
        for (i, comp) in components.iter().enumerate() {
            let child = dir.join(comp);
            let is_dir = i < last;
            for m in matchers.iter().rev() {
                match m.matched(&child, is_dir) {
                    Match::Ignore(_) => return true,
                    // An explicit `!rule` at this depth settles it; shallower
                    // matchers do not get to re-ignore the path.
                    Match::Whitelist(_) => break,
                    Match::None => continue,
                }
            }
            if is_dir {
                dir = child;
                matchers.push(self.matcher_for(&dir));
            }
        }
        false
    }

    fn matcher_for(&self, dir: &Path) -> Arc<Gitignore> {
        if let Some(hit) = self.dir_matchers.lock().get(dir) {
            return hit.clone();
        }
        let mut builder = GitignoreBuilder::new(dir);
        for name in IGNORE_FILES {
            // `add` returns the parse error rather than failing; a malformed
            // ignore file should narrow what we skip, never crash the watcher.
            let _ = builder.add(dir.join(name));
        }
        if dir == self.root {
            let _ = builder.add(dir.join(".git").join("info").join("exclude"));
        }
        let built = Arc::new(builder.build().unwrap_or_else(|_| Gitignore::empty()));
        self.dir_matchers
            .lock()
            .insert(dir.to_path_buf(), built.clone());
        built
    }
}

fn is_hidden(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|s| s.starts_with('.'))
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

    /// Build the tree that exercises every rule the filter mirrors. Returns
    /// the root. Shared by the unit cases and the agreement property.
    fn mixed_tree(dir: &Path) {
        fs::write(
            dir.join(".gitignore"),
            "ignored.md\nbuild/\n!build/keep.md\n",
        )
        .unwrap();
        fs::write(dir.join("kept.md"), "# k\n").unwrap();
        fs::write(dir.join("ignored.md"), "# i\n").unwrap();
        fs::write(dir.join("notes.txt"), "not markdown").unwrap();

        // Hidden directory — the shape that caused the vault to double.
        fs::create_dir_all(dir.join(".claude/worktrees/wt/Entities")).unwrap();
        fs::write(dir.join(".claude/worktrees/wt/Entities/dup.md"), "# d\n").unwrap();

        // Lore's own state directory.
        fs::create_dir_all(dir.join(".lore")).unwrap();
        fs::write(dir.join(".lore/notes.md"), "# state\n").unwrap();

        // Gitignored directory, plus a negation inside it. Git cannot rescue
        // a file under an ignored *directory*, so keep.md stays excluded.
        fs::create_dir_all(dir.join("build")).unwrap();
        fs::write(dir.join("build/out.md"), "# o\n").unwrap();
        fs::write(dir.join("build/keep.md"), "# keep\n").unwrap();

        // Nested .gitignore, deeper than the root's.
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub/.gitignore"), "draft-*.md\n").unwrap();
        fs::write(dir.join("sub/final.md"), "# f\n").unwrap();
        fs::write(dir.join("sub/draft-one.md"), "# d1\n").unwrap();
    }

    #[test]
    fn filter_rejects_hidden_dirs_gitignores_and_lore_state() {
        let dir = tempdir().unwrap();
        mixed_tree(dir.path());
        let f = PathFilter::new(dir.path(), WalkOptions::default());

        assert!(f.accepts(&dir.path().join("kept.md")));
        assert!(f.accepts(&dir.path().join("sub/final.md")));

        // The regression this whole change exists for.
        assert!(!f.accepts(&dir.path().join(".claude/worktrees/wt/Entities/dup.md")));

        assert!(!f.accepts(&dir.path().join("ignored.md")));
        assert!(!f.accepts(&dir.path().join(".lore/notes.md")));
        assert!(!f.accepts(&dir.path().join("build/out.md")));
        assert!(!f.accepts(&dir.path().join("build/keep.md")));
        assert!(!f.accepts(&dir.path().join("sub/draft-one.md")));
        assert!(!f.accepts(&dir.path().join("notes.txt")));
        assert!(!f.accepts(Path::new("/somewhere/else/outside.md")));
    }

    /// The mirror must not drift from the thing it mirrors. Walk the fixture
    /// for real, then assert `PathFilter` returns the same verdict for every
    /// markdown file on disk — included and excluded alike.
    #[test]
    fn filter_agrees_with_walk_on_a_mixed_tree() {
        let dir = tempdir().unwrap();
        mixed_tree(dir.path());
        let opts = WalkOptions::default();

        let walked: std::collections::HashSet<PathBuf> =
            walk_markdown(dir.path(), &opts).into_iter().collect();
        assert!(!walked.is_empty(), "fixture should yield something");

        let filter = PathFilter::new(dir.path(), opts);
        let mut checked = 0;
        for path in every_markdown_file(dir.path()) {
            let expected = walked.contains(&path);
            assert_eq!(
                filter.accepts(&path),
                expected,
                "filter and walk disagree on {}",
                path.display()
            );
            checked += 1;
        }
        assert!(
            checked > walked.len(),
            "fixture must contain excluded files too ({checked} seen, {} walked)",
            walked.len()
        );
    }

    /// Unfiltered recursive listing — deliberately *not* using the `ignore`
    /// crate, so the test cannot inherit the behaviour it is checking.
    fn every_markdown_file(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            let Ok(entries) = fs::read_dir(&d) else {
                continue;
            };
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().and_then(|e| e.to_str()) == Some("md") {
                    out.push(p);
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn invalidate_picks_up_a_rewritten_gitignore() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(".gitignore"), "").unwrap();
        fs::write(dir.path().join("a.md"), "# a\n").unwrap();
        let f = PathFilter::new(dir.path(), WalkOptions::default());
        assert!(f.accepts(&dir.path().join("a.md")));

        fs::write(dir.path().join(".gitignore"), "a.md\n").unwrap();
        assert!(
            f.accepts(&dir.path().join("a.md")),
            "stale cache still allows"
        );
        f.invalidate();
        assert!(!f.accepts(&dir.path().join("a.md")));
    }

    #[test]
    fn recognizes_ignore_files_by_name() {
        assert!(PathFilter::is_ignore_file(Path::new("/v/.gitignore")));
        assert!(PathFilter::is_ignore_file(Path::new("/v/sub/.ignore")));
        assert!(!PathFilter::is_ignore_file(Path::new("/v/notes.md")));
    }

    #[test]
    fn rel_path_uses_forward_slashes() {
        let root = Path::new("/tmp/kb");
        let file = Path::new("/tmp/kb/docs/intro.md");
        assert_eq!(rel_path(root, file), "docs/intro.md");
    }
}
