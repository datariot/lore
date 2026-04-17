//! `lore index <dir>` — walk a directory, parse every markdown file, and
//! write the corpus index to `<dir>/.lore/index.json`.

use std::path::{Path, PathBuf};
use std::time::Instant;

use lore_core::{Error, Result, SourceId};
use lore_index::{CorpusIndex, build_document, write_index};
use serde::Serialize;
use tracing::{info, warn};

use crate::config::{default_source_id, index_path};
use crate::walker::{WalkOptions, rel_path, walk_markdown};

#[derive(Debug, Clone)]
pub struct IndexOptions {
    /// Corpus root on disk.
    pub root: PathBuf,
    /// Override for the corpus identifier. Defaults to the root's basename.
    pub source_id: Option<String>,
    /// Walker options.
    pub walk: WalkOptions,
}

impl IndexOptions {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            source_id: None,
            walk: WalkOptions::default(),
        }
    }
}

/// Summary of an indexing run.
#[derive(Debug, Clone, Serialize)]
pub struct IndexReport {
    pub source_id: String,
    pub root: PathBuf,
    pub index_path: PathBuf,
    pub files_indexed: usize,
    pub files_failed: usize,
    pub total_nodes: usize,
    pub build_millis: u64,
    pub write_millis: u64,
}

/// Execute `lore index`.
pub fn index_command(opts: IndexOptions) -> Result<IndexReport> {
    let root = canonicalize(&opts.root)?;
    let source_id = SourceId::new(
        opts.source_id
            .clone()
            .unwrap_or_else(|| default_source_id(&root)),
    );

    let build_started = Instant::now();
    let files = walk_markdown(&root, &opts.walk);
    info!(
        root = %root.display(),
        discovered = files.len(),
        "walked corpus"
    );

    let mut corpus = CorpusIndex::new(source_id.clone(), root.clone());
    let mut failed = 0usize;
    for path in &files {
        match read_and_build(&source_id, &root, path) {
            Ok(doc) => {
                corpus.push_document(doc);
            }
            Err(e) => {
                failed += 1;
                warn!(path = %path.display(), err = %e, "failed to index file");
            }
        }
    }
    corpus.rebuild_indices();
    let build_millis = build_started.elapsed().as_millis() as u64;

    let out_path = index_path(&root);
    let write_started = Instant::now();
    write_index(&out_path, &corpus)?;
    let write_millis = write_started.elapsed().as_millis() as u64;

    let report = IndexReport {
        source_id: source_id.to_string(),
        root: root.clone(),
        index_path: out_path,
        files_indexed: corpus.documents.len(),
        files_failed: failed,
        total_nodes: corpus.total_nodes(),
        build_millis,
        write_millis,
    };
    info!(
        indexed = report.files_indexed,
        nodes = report.total_nodes,
        failed = report.files_failed,
        build_ms = report.build_millis,
        write_ms = report.write_millis,
        "index written"
    );
    Ok(report)
}

fn read_and_build(
    source: &SourceId,
    root: &Path,
    path: &Path,
) -> Result<lore_index::DocumentIndex> {
    let rel = rel_path(root, path);
    let bytes = std::fs::read(path)?;
    let src = std::str::from_utf8(&bytes).map_err(|e| Error::Parse(format!("{rel}: {e}")))?;
    build_document(source.clone(), rel, src)
}

fn canonicalize(p: &Path) -> Result<PathBuf> {
    std::fs::canonicalize(p).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn fixture(dir: &Path) {
        fs::write(
            dir.join("intro.md"),
            "---\ntitle: Intro\n---\n# Intro\n\nHello there.\n\n## Details\n\n[x](y)\n",
        )
        .unwrap();
        fs::create_dir(dir.join("docs")).unwrap();
        fs::write(
            dir.join("docs/arch.md"),
            "# Architecture\n\n## Data Layer\n\n### Caching\n\nUses an LRU.\n",
        )
        .unwrap();
        fs::write(dir.join("docs/README.md"), "# Docs\n\nSee [[arch]].\n").unwrap();
        // Non-markdown should be ignored.
        fs::write(dir.join("other.txt"), "not me").unwrap();
        // Hidden dir should be ignored by default.
        fs::create_dir(dir.join(".hidden")).unwrap();
        fs::write(dir.join(".hidden/secret.md"), "# Secret\n").unwrap();
    }

    #[test]
    fn end_to_end_index_and_load() {
        let dir = tempdir().unwrap();
        fixture(dir.path());

        let report = index_command(IndexOptions::new(dir.path())).unwrap();
        assert_eq!(report.files_indexed, 3);
        assert_eq!(report.files_failed, 0);
        assert!(report.total_nodes >= 5);
        assert!(report.index_path.exists());

        // Reload and verify derived indices repopulate.
        let loaded = lore_index::load_index(&report.index_path).unwrap();
        assert_eq!(loaded.documents.len(), 3);
        assert!(!loaded.heading_lookup.is_empty());
        // Deterministic document ordering.
        assert_eq!(loaded.documents[0].rel_path, "docs/README.md");
    }

    #[test]
    fn failed_files_counted_but_do_not_abort() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("ok.md"), "# ok\n").unwrap();
        // Non-UTF-8 bytes should fail to parse as &str.
        fs::write(dir.path().join("bad.md"), [0xFF, 0xFE, 0xFD]).unwrap();

        let report = index_command(IndexOptions::new(dir.path())).unwrap();
        assert_eq!(report.files_indexed, 1);
        assert_eq!(report.files_failed, 1);
    }
}
