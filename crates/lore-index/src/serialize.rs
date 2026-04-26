//! On-disk index format.
//!
//! We use `serde_json` with pretty-printing disabled. At ~15K headings the
//! file is well under 20 MB and deserializes in ~20 ms. Using the simplest
//! possible format also lets humans diff and inspect the index by hand during
//! development.

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;

use lore_core::Result;

use crate::corpus::CorpusIndex;

// v2 bumped the magic when Porter stemming landed in `tokenize`. v1
// indexes have unstemmed inverted-index keys and would mis-rank against a
// v2 query stream; reject them so users re-run `lore index`.
const MAGIC: &str = "lore-index-v2";

/// Write-side envelope — borrows the corpus so we don't clone 20 MB of data
/// to stamp a magic string on top.
#[derive(serde::Serialize)]
struct OnDiskRef<'a> {
    magic: &'static str,
    corpus: &'a CorpusIndex,
}

/// Read-side envelope — owns the corpus because deserialization can't
/// borrow out of a `BufReader`.
#[derive(serde::Deserialize)]
struct OnDiskOwned {
    magic: String,
    corpus: CorpusIndex,
}

pub fn write_index(path: &Path, corpus: &CorpusIndex) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let f = File::create(path)?;
    let w = BufWriter::new(f);
    let disk = OnDiskRef {
        magic: MAGIC,
        corpus,
    };
    serde_json::to_writer(w, &disk)?;
    Ok(())
}

pub fn load_index(path: &Path) -> Result<CorpusIndex> {
    let f = File::open(path)?;
    let r = BufReader::new(f);
    let disk: OnDiskOwned = serde_json::from_reader(r)?;
    if disk.magic != MAGIC {
        return Err(lore_core::Error::Parse(format!(
            "unknown index magic: {}",
            disk.magic
        )));
    }
    let mut corpus = disk.corpus;
    corpus.rebuild_indices();
    Ok(corpus)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::build_document;
    use lore_core::SourceId;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn round_trip_through_disk() {
        let dir = tempdir().unwrap();
        let idx_path = dir.path().join(".lore/index.json");

        let mut corpus = CorpusIndex::new(SourceId::new("kb"), PathBuf::from("/fake"));
        let doc = build_document(
            SourceId::new("kb"),
            "hello.md",
            "# A\n\ntext.\n\n## B\n\nmore.\n",
        )
        .unwrap();
        corpus.push_document(doc);
        corpus.rebuild_indices();

        write_index(&idx_path, &corpus).unwrap();
        let loaded = load_index(&idx_path).unwrap();
        assert_eq!(loaded.documents.len(), 1);
        assert_eq!(loaded.documents[0].nodes.len(), 2);
        assert_eq!(loaded.documents[0].nodes[0].title, "A");
        // derived indices rebuilt
        assert!(!loaded.heading_lookup.is_empty());
    }
}

#[cfg(test)]
mod bench_deps {
    // Pull tempfile into the test-graph so `cargo test` compiles it.
    #[allow(unused_imports)]
    use tempfile as _;
}
