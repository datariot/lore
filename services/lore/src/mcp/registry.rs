//! In-memory registry of loaded corpus indices.
//!
//! A single Lore instance can serve many corpora. The registry owns each
//! `CorpusIndex` behind an `Arc<RwLock<…>>` so the watcher (Phase 5) can
//! mutate it in place while queries continue to read concurrently. We use
//! `parking_lot::RwLock` for its fair, poison-free semantics — the
//! protected section is short enough that a synchronous lock is fine on
//! the tokio runtime.
//!
//! The registry also caches `memmap2::Mmap`s of the underlying source files
//! keyed by `(source_id, rel_path)` so `get_section` is an O(1) byte-range
//! slice. Caches are invalidated when the corpus is reloaded *or* when a
//! specific document is re-indexed.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dashmap::DashMap;
use lore_core::{Error, Result, SourceId};
use lore_index::{CorpusIndex, build_document, load_index};
use memmap2::Mmap;
use parking_lot::RwLock;
use tracing::{debug, info};

use crate::config::index_path;

/// Shared handle to a loaded corpus.
pub type CorpusHandle = Arc<RwLock<CorpusIndex>>;

/// Thread-safe store of corpora keyed by `SourceId`.
#[derive(Clone, Default)]
pub struct CorpusRegistry {
    corpora: Arc<DashMap<SourceId, CorpusHandle>>,
    /// Per-document mmap cache.
    mmaps: Arc<DashMap<(SourceId, String), Arc<Mmap>>>,
    /// Absolute corpus roots in the order they were registered — used by
    /// the watcher to map a changed path back to a source.
    roots: Arc<RwLock<Vec<(SourceId, PathBuf)>>>,
}

impl CorpusRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load and register an index file.
    pub fn load_from_path(&self, path: &Path) -> Result<CorpusHandle> {
        let corpus = load_index(path)?;
        let source_id = corpus.source.clone();
        let root_dir = corpus.root_dir.clone();
        let handle = Arc::new(RwLock::new(corpus));
        self.install(source_id.clone(), root_dir.clone(), handle.clone());
        let guard = handle.read();
        info!(
            source = %source_id,
            root = %root_dir.display(),
            docs = guard.documents.len(),
            nodes = guard.total_nodes(),
            "corpus loaded"
        );
        drop(guard);
        Ok(handle)
    }

    /// Load the canonical `.lore/index.json` inside `root`.
    pub fn load_from_root(&self, root: &Path) -> Result<CorpusHandle> {
        let p = index_path(root);
        self.load_from_path(&p)
    }

    fn install(&self, id: SourceId, root: PathBuf, handle: CorpusHandle) {
        // Canonicalize so `locate()` can match against notify's canonical
        // paths on platforms that resolve `/tmp` → `/private/tmp`. If
        // canonicalization fails (root removed? permission error?) keep
        // the original path — the watcher just won't find changes for it.
        let canonical = root.canonicalize().unwrap_or(root);
        self.corpora.insert(id.clone(), handle);
        self.mmaps.retain(|key, _| key.0 != id);
        let mut roots = self.roots.write();
        roots.retain(|(sid, _)| *sid != id);
        roots.push((id, canonical));
    }

    pub fn get(&self, source: &SourceId) -> Option<CorpusHandle> {
        self.corpora.get(source).map(|r| r.value().clone())
    }

    pub fn ids(&self) -> Vec<SourceId> {
        let mut v: Vec<_> = self.corpora.iter().map(|r| r.key().clone()).collect();
        v.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        v
    }

    pub fn len(&self) -> usize {
        self.corpora.len()
    }

    pub fn is_empty(&self) -> bool {
        self.corpora.is_empty()
    }

    /// Return every registered corpus root — `(source_id, root_dir)` pairs.
    pub fn roots(&self) -> Vec<(SourceId, PathBuf)> {
        self.roots.read().clone()
    }

    /// Given a path to a file on disk, find the source it belongs to and
    /// the `rel_path` inside that corpus. Works even for deleted files —
    /// we try `canonicalize` first (covers symlinked roots) and fall back
    /// to a non-canonicalizing prefix match so Remove events locate.
    pub fn locate(&self, path: &Path) -> Option<(SourceId, String)> {
        let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let roots = self.roots.read();
        for (sid, root) in roots.iter() {
            if let Ok(rel) = abs.strip_prefix(root) {
                return Some((sid.clone(), crate::config::rel_to_posix(rel)));
            }
        }
        None
    }

    /// Re-parse a single file on disk and splice its `DocumentIndex` into
    /// the corresponding corpus, replacing the previous entry or inserting
    /// a new one. Triggers a full `rebuild_indices` so derived tables
    /// (inverted index, backlinks, heading_lookup, trigrams) stay
    /// consistent. Invalidates the file's mmap cache entry.
    pub fn reindex_document(&self, source: &SourceId, rel_path: &str) -> Result<()> {
        let handle = self
            .corpora
            .get(source)
            .ok_or_else(|| Error::NotFound(format!("source {source} not loaded")))?
            .value()
            .clone();

        let full = {
            let guard = handle.read();
            guard.root_dir.join(rel_path)
        };

        let bytes =
            std::fs::read(&full).map_err(|e| Error::Io(format!("read {}: {e}", full.display())))?;
        let src =
            std::str::from_utf8(&bytes).map_err(|e| Error::Parse(format!("{rel_path}: {e}")))?;
        let new_doc = build_document(source.clone(), rel_path, src)?;

        {
            let mut guard = handle.write();
            let existing = guard.documents.iter().position(|d| d.rel_path == rel_path);
            match existing {
                Some(idx) => guard.documents[idx] = new_doc,
                None => {
                    guard.documents.push(new_doc);
                    // Keep documents sorted — `lore index` relies on
                    // deterministic ordering.
                    guard.documents.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
                }
            }
            guard.rebuild_indices();
        }

        self.mmaps.remove(&(source.clone(), rel_path.to_string()));
        debug!(%source, rel_path, "document reindexed");
        Ok(())
    }

    /// Drop a document from its corpus (file deleted). Rebuilds derived
    /// indices so stale postings disappear.
    pub fn remove_document(&self, source: &SourceId, rel_path: &str) {
        let Some(handle) = self.get(source) else {
            return;
        };
        let mut guard = handle.write();
        let before = guard.documents.len();
        guard.documents.retain(|d| d.rel_path != rel_path);
        if guard.documents.len() != before {
            guard.rebuild_indices();
            self.mmaps.remove(&(source.clone(), rel_path.to_string()));
            debug!(%source, rel_path, "document removed");
        }
    }

    /// Memory-map a document file, caching the map for subsequent reads.
    pub fn mmap_document(&self, source: &SourceId, rel_path: &str) -> Result<Arc<Mmap>> {
        let key = (source.clone(), rel_path.to_string());
        if let Some(hit) = self.mmaps.get(&key) {
            return Ok(hit.value().clone());
        }

        let handle = self
            .corpora
            .get(source)
            .ok_or_else(|| Error::NotFound(format!("source {source} not loaded")))?
            .value()
            .clone();
        let full = {
            let guard = handle.read();
            guard.root_dir.join(rel_path)
        };
        let file =
            File::open(&full).map_err(|e| Error::Io(format!("open {}: {e}", full.display())))?;
        // SAFETY: mmap of a read-only file. We never mutate the mapping and
        // never hand out raw pointers; slices derived from the `Mmap` live
        // only as long as the `Arc<Mmap>` kept in the cache.
        let map = unsafe { Mmap::map(&file) }
            .map_err(|e| Error::Io(format!("mmap {}: {e}", full.display())))?;
        let arc = Arc::new(map);
        debug!(%source, rel_path, bytes = arc.len(), "mmapped document");
        self.mmaps.insert(key, arc.clone());
        Ok(arc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lore_index::build_document;
    use std::fs;
    use tempfile::tempdir;

    fn seed(dir: &Path, rel: &str, src: &str) -> CorpusIndex {
        let full = dir.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&full, src).unwrap();
        let doc = build_document(SourceId::new("kb"), rel, src).unwrap();
        let mut corpus = CorpusIndex::new(SourceId::new("kb"), dir.to_path_buf());
        corpus.push_document(doc);
        corpus.rebuild_indices();
        corpus
    }

    #[test]
    fn load_and_lookup() {
        let dir = tempdir().unwrap();
        let corpus = seed(dir.path(), "a.md", "# A\n\n## B\n");
        let idx_path = dir.path().join(".lore/index.json");
        lore_index::write_index(&idx_path, &corpus).unwrap();

        let reg = CorpusRegistry::new();
        reg.load_from_path(&idx_path).unwrap();
        assert_eq!(reg.len(), 1);
        assert!(reg.get(&SourceId::new("kb")).is_some());
    }

    #[test]
    fn mmap_document_caches() {
        let dir = tempdir().unwrap();
        let corpus = seed(dir.path(), "a.md", "# A\n\n## B\n");
        let idx_path = dir.path().join(".lore/index.json");
        lore_index::write_index(&idx_path, &corpus).unwrap();

        let reg = CorpusRegistry::new();
        reg.load_from_path(&idx_path).unwrap();
        let a = reg.mmap_document(&SourceId::new("kb"), "a.md").unwrap();
        let b = reg.mmap_document(&SourceId::new("kb"), "a.md").unwrap();
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn locate_finds_registered_root() {
        let dir = tempdir().unwrap();
        let corpus = seed(dir.path(), "docs/intro.md", "# Intro\n");
        let idx_path = dir.path().join(".lore/index.json");
        lore_index::write_index(&idx_path, &corpus).unwrap();

        let reg = CorpusRegistry::new();
        reg.load_from_path(&idx_path).unwrap();
        let hit = reg.locate(&dir.path().join("docs/intro.md")).unwrap();
        assert_eq!(hit.0.as_str(), "kb");
        assert_eq!(hit.1, "docs/intro.md");

        // Outside the root.
        let other = tempdir().unwrap();
        assert!(reg.locate(&other.path().join("x.md")).is_none());
    }

    #[test]
    fn reindex_document_replaces_existing_entry() {
        let dir = tempdir().unwrap();
        let corpus = seed(dir.path(), "a.md", "# A\n");
        let idx_path = dir.path().join(".lore/index.json");
        lore_index::write_index(&idx_path, &corpus).unwrap();

        let reg = CorpusRegistry::new();
        reg.load_from_path(&idx_path).unwrap();

        // Modify on disk.
        fs::write(dir.path().join("a.md"), "# A\n\n## New\n").unwrap();
        reg.reindex_document(&SourceId::new("kb"), "a.md").unwrap();

        let handle = reg.get(&SourceId::new("kb")).unwrap();
        let g = handle.read();
        let doc = g
            .documents
            .iter()
            .find(|d| d.rel_path == "a.md")
            .expect("still there");
        assert_eq!(doc.nodes.len(), 2);
        assert_eq!(doc.nodes[1].title, "New");
    }

    #[test]
    fn reindex_document_inserts_when_new() {
        let dir = tempdir().unwrap();
        let corpus = seed(dir.path(), "a.md", "# A\n");
        let idx_path = dir.path().join(".lore/index.json");
        lore_index::write_index(&idx_path, &corpus).unwrap();

        let reg = CorpusRegistry::new();
        reg.load_from_path(&idx_path).unwrap();

        fs::write(dir.path().join("b.md"), "# B\n").unwrap();
        reg.reindex_document(&SourceId::new("kb"), "b.md").unwrap();

        let handle = reg.get(&SourceId::new("kb")).unwrap();
        let g = handle.read();
        let rel_paths: Vec<_> = g.documents.iter().map(|d| d.rel_path.as_str()).collect();
        assert_eq!(rel_paths, vec!["a.md", "b.md"]);
    }

    #[test]
    fn remove_document_drops_it() {
        let dir = tempdir().unwrap();
        let corpus = seed(dir.path(), "a.md", "# A\n");
        let idx_path = dir.path().join(".lore/index.json");
        lore_index::write_index(&idx_path, &corpus).unwrap();
        let reg = CorpusRegistry::new();
        reg.load_from_path(&idx_path).unwrap();
        reg.remove_document(&SourceId::new("kb"), "a.md");
        let handle = reg.get(&SourceId::new("kb")).unwrap();
        assert!(handle.read().documents.is_empty());
    }
}
