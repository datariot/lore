//! CI gate for retrieval effectiveness: run the golden query set
//! (`eval/mini-kb.jsonl`) against the mini-kb fixture and assert the metrics
//! hold. A change that regresses ranking or the coverage verdict fails here.
//!
//! This is the quality counterpart to the Criterion latency bench. The
//! floors are deliberately tight because the fixture is small and fully
//! understood — every query has a known-correct answer. If a legitimate
//! change moves the numbers, update the floors *and* say why in the commit.

use std::fs;
use std::path::Path;

use lore_service::eval_command;
use tempfile::tempdir;

const FIXTURE: &str = "tests/fixtures/mini-kb";
const QUERY_SET: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../eval/mini-kb.jsonl");

fn copy_dir(src: &Path, dst: &Path) {
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

#[test]
fn fixture_retrieval_meets_effectiveness_floor() {
    // Copy the fixture so `eval` can write its `.lore/` index into a tempdir
    // rather than the source tree.
    let dir = tempdir().unwrap();
    copy_dir(Path::new(FIXTURE), dir.path());

    let summary = eval_command(dir.path(), Path::new(QUERY_SET), 10).expect("eval runs");

    // Every judged query has a known-correct answer that should rank first.
    assert!(
        summary.judged >= 6,
        "expected the full golden set: {}",
        summary.judged
    );
    assert_eq!(
        summary.success_at_1, 1.0,
        "every golden query should rank its answer #1 on this fixture"
    );
    assert_eq!(summary.mrr, 1.0, "perfect ranking expected on the fixture");

    // Coverage verdicts (full/partial/none) must all match ground truth —
    // this is the KB-642 out-of-domain signal under test.
    assert_eq!(
        summary.coverage_accuracy, 1.0,
        "coverage verdict regressed: {}/{} correct",
        summary.coverage_correct, summary.coverage_scored
    );
    assert!(
        summary.coverage_scored >= 8,
        "expected coverage labels on every query"
    );
}
