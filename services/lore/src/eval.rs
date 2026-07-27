//! Retrieval effectiveness harness.
//!
//! The quality counterpart to the Criterion latency bench: given a labeled
//! query set, run each query through the real ranker and report how well the
//! relevant section ranks. Turns "I think this change helps" into a number
//! you can diff across commits.
//!
//! A query set is JSONL — one [`EvalQuery`] per line:
//! ```json
//! {"query": "kafka connect alerts", "relevant": ["docs/obs.md#Alarms"], "expected_coverage": "full"}
//! {"query": "helm chart rollout", "relevant": [], "expected_coverage": "none"}
//! ```
//! `relevant` targets are `rel_path` (any section of the document counts) or
//! `rel_path#Heading > Sub` (that exact section). A query with no `relevant`
//! targets is an out-of-domain probe: it scores coverage accuracy only.

use std::path::Path;

use lore_core::{Error, Result};
use lore_index::CorpusIndex;
use serde::{Deserialize, Serialize};

use crate::cli::{IndexOptions, index_command};
use crate::config::index_path;
use crate::mcp::CorpusRegistry;

/// One labeled query.
#[derive(Debug, Clone, Deserialize)]
pub struct EvalQuery {
    pub query: String,
    /// Acceptable answers: `rel_path` or `rel_path#Heading > Sub`. Empty for
    /// an out-of-domain probe (coverage-only).
    #[serde(default)]
    pub relevant: Vec<String>,
    /// Expected coverage verdict (`full` | `partial` | `none`), if scored.
    #[serde(default)]
    pub expected_coverage: Option<String>,
    /// Free-text note, ignored by the harness (kept for curators).
    #[serde(default)]
    pub note: Option<String>,
}

/// A parsed relevance target.
struct Target {
    rel_path: String,
    heading: Option<Vec<String>>,
}

fn parse_target(s: &str) -> Target {
    match s.split_once('#') {
        Some((rel, frag)) => {
            let heading: Vec<String> = frag
                .split('>')
                .map(|seg| seg.trim().to_string())
                .filter(|seg| !seg.is_empty())
                .collect();
            Target {
                rel_path: rel.to_string(),
                heading: (!heading.is_empty()).then_some(heading),
            }
        }
        None => Target {
            rel_path: s.to_string(),
            heading: None,
        },
    }
}

/// Does a hit satisfy a target? Doc-level targets match any section of the
/// document; section-level targets require the exact heading path.
fn hit_matches(hit_rel: &str, hit_heading: &[String], t: &Target) -> bool {
    hit_rel == t.rel_path
        && match &t.heading {
            None => true,
            Some(segs) => hit_heading == segs.as_slice(),
        }
}

/// Outcome for one query.
#[derive(Debug, Clone, Serialize)]
pub struct QueryResult {
    pub query: String,
    /// 1-based rank of the first relevant hit, or `None` if no relevant hit
    /// landed in the top `limit`. Always `None` for out-of-domain probes.
    pub rank: Option<usize>,
    /// Whether this query carried `relevant` targets (counts toward Success/MRR).
    pub judged: bool,
    /// Actual coverage verdict returned by search.
    pub coverage: String,
    /// `Some(true/false)` when `expected_coverage` was set, else `None`.
    pub coverage_ok: Option<bool>,
}

/// Aggregate metrics over an eval run.
#[derive(Debug, Clone, Serialize)]
pub struct EvalSummary {
    pub total: usize,
    pub judged: usize,
    pub success_at_1: f64,
    pub success_at_3: f64,
    pub success_at_10: f64,
    pub mrr: f64,
    pub coverage_scored: usize,
    pub coverage_correct: usize,
    pub coverage_accuracy: f64,
    pub results: Vec<QueryResult>,
}

/// Run the query set against a loaded corpus and compute metrics. Pure over
/// the corpus + queries — no I/O — so it's unit-testable.
pub fn run_eval(corpus: &CorpusIndex, queries: &[EvalQuery], limit: usize) -> EvalSummary {
    let mut results = Vec::with_capacity(queries.len());
    let mut judged = 0usize;
    let (mut s1, mut s3, mut s10) = (0usize, 0usize, 0usize);
    let mut rr_sum = 0.0f64;
    let mut cov_scored = 0usize;
    let mut cov_correct = 0usize;

    for q in queries {
        let hits = lore_search::search(corpus, &q.query, limit);
        let targets: Vec<Target> = q.relevant.iter().map(|s| parse_target(s)).collect();

        // Rank of the first hit matching any relevant target.
        let rank = if targets.is_empty() {
            None
        } else {
            hits.iter().position(|h| {
                let Some(doc) = corpus.doc(h.doc) else {
                    return false;
                };
                let Some(node) = doc.node(h.node) else {
                    return false;
                };
                targets
                    .iter()
                    .any(|t| hit_matches(&doc.rel_path, &node.path.0, t))
            })
        };

        let is_judged = !targets.is_empty();
        if is_judged {
            judged += 1;
            if let Some(pos) = rank {
                let r = pos + 1; // 1-based
                if r <= 1 {
                    s1 += 1;
                }
                if r <= 3 {
                    s3 += 1;
                }
                if r <= 10 {
                    s10 += 1;
                }
                rr_sum += 1.0 / r as f64;
            }
        }

        let coverage = lore_search::coverage(corpus, &q.query);
        let coverage_str = coverage_level_str(coverage.level);
        let coverage_ok = q.expected_coverage.as_deref().map(|exp| {
            let ok = exp.eq_ignore_ascii_case(coverage_str);
            cov_scored += 1;
            if ok {
                cov_correct += 1;
            }
            ok
        });

        results.push(QueryResult {
            query: q.query.clone(),
            rank: rank.map(|p| p + 1),
            judged: is_judged,
            coverage: coverage_str.to_string(),
            coverage_ok,
        });
    }

    let jf = judged.max(1) as f64;
    EvalSummary {
        total: queries.len(),
        judged,
        success_at_1: s1 as f64 / jf,
        success_at_3: s3 as f64 / jf,
        success_at_10: s10 as f64 / jf,
        mrr: rr_sum / jf,
        coverage_scored: cov_scored,
        coverage_correct: cov_correct,
        coverage_accuracy: if cov_scored == 0 {
            1.0
        } else {
            cov_correct as f64 / cov_scored as f64
        },
        results,
    }
}

fn coverage_level_str(level: lore_search::Coverage) -> &'static str {
    match level {
        lore_search::Coverage::Full => "full",
        lore_search::Coverage::Partial => "partial",
        lore_search::Coverage::None => "none",
    }
}

/// Parse a JSONL query set. Blank lines and `//`-comment lines are skipped.
pub fn parse_query_set(text: &str) -> Result<Vec<EvalQuery>> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        let q: EvalQuery = serde_json::from_str(trimmed)
            .map_err(|e| Error::Parse(format!("query set line {}: {e}", i + 1)))?;
        out.push(q);
    }
    Ok(out)
}

/// CLI entry point: ensure the corpus is indexed, load it, run the query set
/// from `queries_path`, and return the summary.
pub fn eval_command(root: &Path, queries_path: &Path, limit: usize) -> Result<EvalSummary> {
    let idx = index_path(root);
    if !idx.exists() {
        index_command(IndexOptions::new(root.to_path_buf()))?;
    }
    let registry = CorpusRegistry::new();
    let handle = registry.load_from_root(root)?;
    let corpus = handle.read();

    let text = std::fs::read_to_string(queries_path)
        .map_err(|e| Error::Io(format!("read {}: {e}", queries_path.display())))?;
    let queries = parse_query_set(&text)?;
    Ok(run_eval(&corpus, &queries, limit))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lore_core::SourceId;
    use lore_index::build_document;
    use std::path::PathBuf;

    fn corpus() -> CorpusIndex {
        let mut c = CorpusIndex::new(SourceId::new("t"), PathBuf::from("/tmp"));
        c.push_document(
            build_document(
                SourceId::new("t"),
                "arch.md",
                "# Architecture\n\n## Caching\n\nWe cache sections by node id.\n",
            )
            .unwrap(),
        );
        c.push_document(
            build_document(SourceId::new("t"), "intro.md", "# Intro\n\nwelcome.\n").unwrap(),
        );
        c.rebuild_indices();
        c
    }

    #[test]
    fn parse_target_doc_and_section() {
        let d = parse_target("a/b.md");
        assert_eq!(d.rel_path, "a/b.md");
        assert!(d.heading.is_none());
        let s = parse_target("a/b.md#Foo > Bar");
        assert_eq!(s.rel_path, "a/b.md");
        assert_eq!(s.heading.unwrap(), vec!["Foo", "Bar"]);
    }

    #[test]
    fn doc_level_target_matches_any_section() {
        let t = parse_target("arch.md");
        assert!(hit_matches(
            "arch.md",
            &["Architecture".into(), "Caching".into()],
            &t
        ));
        assert!(!hit_matches("intro.md", &["Intro".into()], &t));
    }

    #[test]
    fn section_target_requires_exact_heading() {
        let t = parse_target("arch.md#Architecture > Caching");
        assert!(hit_matches(
            "arch.md",
            &["Architecture".into(), "Caching".into()],
            &t
        ));
        assert!(!hit_matches("arch.md", &["Architecture".into()], &t));
    }

    #[test]
    fn run_eval_scores_rank_and_coverage() {
        let c = corpus();
        let queries = vec![
            EvalQuery {
                query: "caching".to_string(),
                relevant: vec!["arch.md#Architecture > Caching".to_string()],
                expected_coverage: Some("full".to_string()),
                note: None,
            },
            EvalQuery {
                // out-of-domain probe: no relevant, expect coverage none
                query: "kubernetes helm".to_string(),
                relevant: vec![],
                expected_coverage: Some("none".to_string()),
                note: None,
            },
        ];
        let s = run_eval(&c, &queries, 10);
        assert_eq!(s.total, 2);
        assert_eq!(s.judged, 1, "only the caching query is judged");
        assert_eq!(
            s.success_at_1, 1.0,
            "caching ranks the Caching section first"
        );
        assert_eq!(s.mrr, 1.0);
        assert_eq!(s.coverage_scored, 2);
        assert_eq!(s.coverage_correct, 2, "full and none both correct");
        assert_eq!(s.coverage_accuracy, 1.0);
    }

    #[test]
    fn missing_relevant_hit_lowers_mrr() {
        let c = corpus();
        let queries = vec![EvalQuery {
            query: "caching".to_string(),
            // ground truth points at a doc the query can't reach
            relevant: vec!["intro.md".to_string()],
            expected_coverage: None,
            note: None,
        }];
        let s = run_eval(&c, &queries, 10);
        assert_eq!(s.judged, 1);
        assert_eq!(s.success_at_10, 0.0, "no relevant hit in range");
        assert_eq!(s.mrr, 0.0);
    }
}
