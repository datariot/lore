//! End-to-end: `lore index` the fixture corpus, load the index back,
//! assert structural properties.

use std::fs;

use lore_core::HeadingPath;
use lore_index::load_index;
use lore_service::{IndexOptions, index_command};
use tempfile::tempdir;

const FIXTURE: &str = "tests/fixtures/mini-kb";

fn copy_fixture_to(dest: &std::path::Path) {
    fn copy_dir(src: &std::path::Path, dst: &std::path::Path) {
        fs::create_dir_all(dst).unwrap();
        for entry in fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let from = entry.path();
            let to = dst.join(entry.file_name());
            if from.is_dir() {
                copy_dir(&from, &to);
            } else {
                fs::copy(&from, &to).unwrap();
            }
        }
    }
    copy_dir(std::path::Path::new(FIXTURE), dest);
}

#[test]
fn indexes_fixture_and_round_trips() {
    let dir = tempdir().unwrap();
    copy_fixture_to(dir.path());

    let report = index_command(IndexOptions::new(dir.path())).unwrap();
    assert_eq!(report.files_indexed, 3);
    assert_eq!(report.files_failed, 0);
    assert!(report.index_path.exists());

    let corpus = load_index(&report.index_path).unwrap();
    assert_eq!(corpus.documents.len(), 3);

    // Every document has at least one root heading.
    for doc in &corpus.documents {
        assert!(!doc.roots.is_empty(), "{} has no roots", doc.rel_path);
    }

    // `HeadingPath::new(["Introduction", "Purpose", "Why"])` exists as a node.
    let traversal = lore_index::Traversal::new(&corpus);
    let hits = traversal.resolve_path(&HeadingPath::new(["Introduction", "Purpose", "Why"]));
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].node.level, 3);

    // The README's frontmatter should be preserved.
    let readme = corpus
        .documents
        .iter()
        .find(|d| d.rel_path == "README.md")
        .expect("README.md in corpus");
    let fm = readme.frontmatter.as_ref().expect("frontmatter present");
    assert_eq!(fm["title"], "Mini KB");

    // The wiki-link inside a code fence must NOT appear as a link anywhere.
    for doc in &corpus.documents {
        for node in &doc.nodes {
            for link in &node.outbound_links {
                assert_ne!(link.target, "should_not_count");
            }
        }
    }

    // Section retrieval by byte range returns the right bytes.
    let raw = fs::read_to_string(dir.path().join(&readme.rel_path)).unwrap();
    let overview = readme
        .nodes
        .iter()
        .find(|n| n.title == "Overview")
        .expect("Overview heading");
    let slice = &raw[overview.byte_range.start as usize..overview.byte_range.end as usize];
    assert!(slice.starts_with("## Overview"));
}
