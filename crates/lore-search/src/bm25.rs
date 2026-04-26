//! BM25 ranker over three fields: title, path-segments, summary.
//!
//! Uses the inverted index and per-field lengths that `lore-index` precomputes
//! during `rebuild_indices`. Query latency is O(tokens_in_query *
//! avg_posting_length), not O(total_corpus_tokens) — so a keyword search over
//! the whole 14K-node knowledge-base costs a handful of `HashMap` lookups.
//!
//! BM25 formula for a single field:
//! ```text
//! score = idf * tf * (k1 + 1) / (tf + k1 * (1 - b + b * len / avgdl))
//! ```

use std::collections::HashMap;

use lore_core::NodeId;
use lore_index::{CorpusIndex, DocId, Field, tokenize};
use serde::Serialize;

/// Tunables for the BM25 ranker.
///
/// Per-field weights bias scoring toward the heading hierarchy (titles and
/// path segments beat summary prose). `k1` and `b` are the standard BM25
/// knobs — `k1` controls term-frequency saturation, `b` controls
/// length-normalization. `access_boost` is Lore's own extension: every time
/// an agent fetches a section its access counter increments, and we add
/// `access_boost * ln(count + 1)` to the ranked score so frequently-used
/// sections climb over equivalent neighbors.
pub struct Ranker {
    pub title_weight: f32,
    pub path_weight: f32,
    pub summary_weight: f32,
    pub k1: f32,
    pub b: f32,
    pub access_boost: f32,
}

impl Default for Ranker {
    fn default() -> Self {
        Self {
            title_weight: 3.0,
            path_weight: 2.0,
            summary_weight: 1.0,
            k1: 1.2,
            b: 0.75,
            access_boost: 0.25,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub doc: DocId,
    pub node: NodeId,
    pub score: f32,
}

/// Rank every node whose tokens intersect `query` and return the top `limit`.
pub fn search(corpus: &CorpusIndex, query: &str, limit: usize) -> Vec<SearchHit> {
    search_bm25(corpus, query, limit, &Ranker::default())
}

/// One row per matching document: the top-scoring section as `primary`, plus
/// up to `secondary_per_doc` additional matching sections from the same
/// document. Useful when an agent searches a narrow term that concentrates
/// in one file — it sees the whole doc's structure instead of N rows of the
/// same file.
#[derive(Debug, Clone, Serialize)]
pub struct GroupedSearchHit {
    pub primary: SearchHit,
    pub secondary: Vec<SearchHit>,
}

/// Group the BM25 result set by document. The first hit per document
/// becomes the primary; up to `secondary_per_doc` additional same-document
/// hits are attached, in score order. `doc_limit` caps the total document
/// count returned. Uses the default `Ranker`.
pub fn search_grouped(
    corpus: &CorpusIndex,
    query: &str,
    doc_limit: usize,
    secondary_per_doc: usize,
) -> Vec<GroupedSearchHit> {
    search_grouped_bm25(
        corpus,
        query,
        doc_limit,
        secondary_per_doc,
        &Ranker::default(),
    )
}

pub fn search_grouped_bm25(
    corpus: &CorpusIndex,
    query: &str,
    doc_limit: usize,
    secondary_per_doc: usize,
    ranker: &Ranker,
) -> Vec<GroupedSearchHit> {
    let all = score_all(corpus, query, ranker);
    if all.is_empty() {
        return Vec::new();
    }
    // Single linear pass: the first time we see a doc, start a group; the
    // next `secondary_per_doc` hits with the same doc append to its
    // secondary list; further hits drop. Stop once we've started
    // `doc_limit` groups.
    let mut groups: Vec<GroupedSearchHit> = Vec::new();
    let mut idx_by_doc: HashMap<DocId, usize> = HashMap::new();
    for hit in all {
        if let Some(&i) = idx_by_doc.get(&hit.doc) {
            if groups[i].secondary.len() < secondary_per_doc {
                groups[i].secondary.push(hit);
            }
        } else if groups.len() < doc_limit {
            idx_by_doc.insert(hit.doc, groups.len());
            groups.push(GroupedSearchHit {
                primary: hit,
                secondary: Vec::new(),
            });
        }
        // else: doc_limit reached and this hit is for a new doc — drop.
    }
    groups
}

/// Split a raw query string into positive and negative tokens.
///
/// A whitespace-separated word starting with `-` is a negation: subsequent
/// scoring proceeds against the rest of the query, then any node matched by
/// a negation is dropped from the result. A bare `-` or a hyphen inside a
/// word (`kafka-connect`) is not a negation. Negation tokens go through the
/// same normalization (`tokenize`) as positives so casing and stopword
/// rules stay consistent.
pub fn parse_query(query: &str) -> (Vec<String>, Vec<String>) {
    let mut pos = Vec::new();
    let mut neg = Vec::new();
    for word in query.split_whitespace() {
        if let Some(rest) = word.strip_prefix('-') {
            if rest.is_empty() {
                continue;
            }
            neg.extend(tokenize(rest));
        } else {
            pos.extend(tokenize(word));
        }
    }
    (pos, neg)
}

pub fn search_bm25(
    corpus: &CorpusIndex,
    query: &str,
    limit: usize,
    ranker: &Ranker,
) -> Vec<SearchHit> {
    let mut scored = score_all(corpus, query, ranker);
    scored.truncate(limit);
    scored
}

/// Run the BM25 scoring pass and return every matched node sorted by
/// score, descending. No truncation. Shared by `search_bm25` (which
/// truncates to a section limit) and `search_grouped_bm25` (which
/// truncates by document count after grouping).
fn score_all(corpus: &CorpusIndex, query: &str, ranker: &Ranker) -> Vec<SearchHit> {
    let (q_tokens, neg_tokens) = parse_query(query);
    if q_tokens.is_empty() {
        return Vec::new();
    }

    // Collect every (doc, node) that any negative token matches. A node is
    // excluded from results if it appears in this set, regardless of how
    // strongly the positive terms scored it.
    let mut excluded: std::collections::HashSet<(DocId, NodeId)> = std::collections::HashSet::new();
    for token in &neg_tokens {
        if let Some(postings) = corpus.inverted.get(token) {
            for p in postings {
                excluded.insert((p.doc, p.node));
            }
        }
    }

    // In Lore, BM25's "document" unit is a *heading node*, not a file.
    // Everything below — IDF, length normalization — is per-node.
    let total_nodes = corpus.field_lengths.total_nodes.max(1) as f32;
    let mut accum: HashMap<(DocId, NodeId), f32> = HashMap::new();

    for token in &q_tokens {
        let Some(postings) = corpus.inverted.get(token) else {
            continue;
        };
        // IDF is measured across fields — count each unique node once.
        let unique_nodes: std::collections::HashSet<(DocId, NodeId)> =
            postings.iter().map(|p| (p.doc, p.node)).collect();
        let n = unique_nodes.len() as f32;
        let idf = ((total_nodes - n + 0.5) / (n + 0.5) + 1.0).ln();
        if idf <= 0.0 {
            continue;
        }
        for p in postings {
            let (avgdl, weight, len) = match p.field {
                Field::Title => (
                    corpus.field_lengths.avg_title,
                    ranker.title_weight,
                    corpus.field_lengths.get(p.doc, p.node, Field::Title),
                ),
                Field::Path => (
                    corpus.field_lengths.avg_path,
                    ranker.path_weight,
                    corpus.field_lengths.get(p.doc, p.node, Field::Path),
                ),
                Field::Summary => (
                    corpus.field_lengths.avg_summary,
                    ranker.summary_weight,
                    corpus.field_lengths.get(p.doc, p.node, Field::Summary),
                ),
            };
            let norm = 1.0 - ranker.b + ranker.b * (len as f32) / avgdl.max(1.0);
            let tf = p.tf as f32;
            let field_score = idf * tf * (ranker.k1 + 1.0) / (tf + ranker.k1 * norm);
            *accum.entry((p.doc, p.node)).or_insert(0.0) += field_score * weight;
        }
    }

    // Drop nodes that any negative term matched.
    if !excluded.is_empty() {
        accum.retain(|key, _| !excluded.contains(key));
    }

    // Access-count boost: log1p so heavy hitters don't drown out exact
    // matches on under-accessed sections. Apply once per node.
    if ranker.access_boost > 0.0 {
        for (&(did, nid), score) in accum.iter_mut() {
            if let Some(doc) = corpus.doc(did)
                && let Some(node) = doc.node(nid)
            {
                let access = node.access_count.get();
                if access > 0 {
                    *score += (access as f32 + 1.0).ln() * ranker.access_boost;
                }
            }
        }
    }

    let mut scored: Vec<SearchHit> = accum
        .into_iter()
        .map(|((doc, node), score)| SearchHit { doc, node, score })
        .collect();
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    scored
}

#[cfg(test)]
mod tests {
    use super::*;
    use lore_core::SourceId;
    use lore_index::build_document;
    use std::path::PathBuf;

    fn corpus_of(docs: &[(&str, &str)]) -> CorpusIndex {
        let mut corp = CorpusIndex::new(SourceId::new("t"), PathBuf::from("/tmp"));
        for (rel, src) in docs {
            let d = build_document(SourceId::new("t"), *rel, src).unwrap();
            corp.push_document(d);
        }
        corp.rebuild_indices();
        corp
    }

    #[test]
    fn title_match_beats_summary_match() {
        let corpus = corpus_of(&[
            ("a.md", "# Caching Strategy\n\nOverview.\n"),
            ("b.md", "# Overview\n\nWe discuss caching in passing.\n"),
        ]);
        let hits = search(&corpus, "caching", 10);
        assert!(!hits.is_empty());
        let top = &hits[0];
        let top_doc = corpus.doc(top.doc).unwrap();
        assert_eq!(top_doc.rel_path, "a.md");
    }

    #[test]
    fn path_segments_contribute_to_score() {
        let corpus = corpus_of(&[
            ("a.md", "# Architecture\n\n## Tokio Runtime\n\nunrelated.\n"),
            ("b.md", "# Architecture\n\n## Caching\n\nalso unrelated.\n"),
        ]);
        let hits = search(&corpus, "tokio", 10);
        assert!(!hits.is_empty());
        let top = &hits[0];
        let node = corpus.doc(top.doc).unwrap().node(top.node).unwrap();
        assert_eq!(node.title, "Tokio Runtime");
    }

    #[test]
    fn empty_query_returns_no_hits() {
        let corpus = corpus_of(&[("a.md", "# Hi\n")]);
        assert!(search(&corpus, "", 5).is_empty());
        assert!(search(&corpus, "   ", 5).is_empty());
    }

    #[test]
    fn idf_prefers_rare_terms() {
        let corpus = corpus_of(&[
            ("a.md", "# Caching\n\noverview.\n"),
            ("b.md", "# Overview\n\noverview overview.\n"),
            ("c.md", "# Overview\n\noverview.\n"),
        ]);
        let hits_caching = search(&corpus, "caching", 5);
        let hits_overview = search(&corpus, "overview", 5);
        assert!(hits_caching[0].score > 0.0);
        assert!(hits_overview[0].score > 0.0);
        assert!(hits_caching[0].score > hits_overview[0].score);
    }

    #[test]
    fn stemming_unifies_inflectional_variants() {
        // Doc indexed as `alarms` must surface for query `alarm` and
        // vice versa; same for the deploy/deployed pair. This is the
        // alert/alarm gap from the dogfood pass — the inverted-index
        // key is the Porter stem, so the two sides agree by
        // construction.
        let corpus = corpus_of(&[
            ("alarm.md", "# Critical Alarms\n\nbody.\n"),
            ("deploy.md", "# Deployed Pipelines\n\nbody.\n"),
            ("decoy.md", "# Documentation\n\nunrelated.\n"),
        ]);

        // Query a different surface form than the one in the index.
        let by_alarm = search(&corpus, "alarm", 10);
        assert!(
            by_alarm
                .iter()
                .any(|h| corpus.doc(h.doc).unwrap().rel_path == "alarm.md"),
            "query 'alarm' should reach 'Critical Alarms' via stemming"
        );
        let by_alarming = search(&corpus, "alarming", 10);
        assert!(
            by_alarming
                .iter()
                .any(|h| corpus.doc(h.doc).unwrap().rel_path == "alarm.md"),
            "query 'alarming' should reach 'Critical Alarms'"
        );

        let by_deploy = search(&corpus, "deploy", 10);
        assert!(
            by_deploy
                .iter()
                .any(|h| corpus.doc(h.doc).unwrap().rel_path == "deploy.md"),
            "query 'deploy' should reach 'Deployed Pipelines'"
        );
    }

    #[test]
    fn negation_excludes_matching_nodes() {
        let corpus = corpus_of(&[
            ("a.md", "# Kafka Connect\n\nstreaming.\n"),
            ("b.md", "# Kafka Lambda\n\nfunction.\n"),
        ]);
        let plain = search(&corpus, "kafka", 10);
        assert_eq!(plain.len(), 2, "both docs match positive 'kafka'");

        let negated = search(&corpus, "kafka -lambda", 10);
        assert_eq!(negated.len(), 1, "lambda doc must be excluded");
        let top_doc = corpus.doc(negated[0].doc).unwrap();
        assert_eq!(top_doc.rel_path, "a.md");
    }

    #[test]
    fn negation_only_query_returns_no_hits() {
        let corpus = corpus_of(&[("a.md", "# Anything\n\nbody.\n")]);
        assert!(search(&corpus, "-anything", 10).is_empty());
    }

    #[test]
    fn negation_does_not_split_words_on_internal_hyphen() {
        // `kafka-connect` is a single shell word; the leading character is
        // `k`, not `-`. Internal hyphens must stay inside the term.
        let (pos, neg) = parse_query("kafka-connect");
        assert!(neg.is_empty());
        assert!(
            !pos.is_empty(),
            "internal-hyphen token should produce positives"
        );
    }

    #[test]
    fn negation_leading_dash_alone_is_ignored() {
        let (pos, neg) = parse_query("- kafka");
        assert!(neg.is_empty());
        assert_eq!(pos, vec!["kafka".to_string()]);
    }

    #[test]
    fn grouped_collapses_same_doc_to_one_primary_with_secondaries() {
        // Single document with three matching sections; two unrelated docs
        // contribute one match each. Without grouping the narrow corpus
        // produces 3 hits from the same doc; grouped, doc_a appears once
        // with two secondaries.
        let corpus = corpus_of(&[
            (
                "a.md",
                "# Andrew\n\n## Background\n\nAndrew here.\n\n## Notes\n\nAndrew there.\n",
            ),
            ("b.md", "# Andrew the Great\n\nbrief mention.\n"),
            ("c.md", "# Unrelated\n\nempty.\n"),
        ]);

        let flat = search(&corpus, "andrew", 10);
        let from_a = flat
            .iter()
            .filter(|h| corpus.doc(h.doc).unwrap().rel_path == "a.md")
            .count();
        assert!(from_a >= 2, "fixture should have multiple matches in a.md");

        let grouped = search_grouped(&corpus, "andrew", 10, 3);
        let docs: Vec<&str> = grouped
            .iter()
            .map(|g| corpus.doc(g.primary.doc).unwrap().rel_path.as_str())
            .collect();
        assert!(docs.contains(&"a.md"), "a.md must appear once: {docs:?}");
        // Each doc appears at most once as a primary.
        let mut sorted = docs.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            docs.len(),
            "duplicate docs in primary set: {docs:?}"
        );
        // The a.md group should carry secondary hits drawn from the same doc.
        let a_group = grouped
            .iter()
            .find(|g| corpus.doc(g.primary.doc).unwrap().rel_path == "a.md")
            .unwrap();
        assert!(
            !a_group.secondary.is_empty(),
            "a.md primary should have secondary hits"
        );
        for sec in &a_group.secondary {
            assert_eq!(sec.doc, a_group.primary.doc, "secondary doc mismatch");
            assert!(
                sec.score <= a_group.primary.score,
                "secondary outscored primary"
            );
        }
    }

    #[test]
    fn grouped_doc_limit_caps_unique_documents() {
        let corpus = corpus_of(&[
            ("a.md", "# Topic\n\nbody.\n"),
            ("b.md", "# Topic\n\nbody.\n"),
            ("c.md", "# Topic\n\nbody.\n"),
        ]);
        let two = search_grouped(&corpus, "topic", 2, 0);
        assert_eq!(two.len(), 2, "doc_limit=2 must cap at 2 documents");
        for g in &two {
            assert!(
                g.secondary.is_empty(),
                "secondary_per_doc=0 → no secondaries"
            );
        }
    }

    #[test]
    fn grouped_secondary_limit_truncates_extras() {
        // Five matching sections in one doc; secondary_per_doc=2 keeps top
        // two beyond the primary, drops the rest.
        let mut src = String::from("# Root\n\nhit.\n");
        for i in 0..5 {
            src.push_str(&format!("\n## Section {i}\n\nhit hit hit.\n"));
        }
        let corpus = corpus_of(&[("a.md", src.as_str())]);
        let grouped = search_grouped(&corpus, "hit", 10, 2);
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].secondary.len(), 2);
    }

    #[test]
    fn grouped_empty_query_returns_empty() {
        let corpus = corpus_of(&[("a.md", "# Hi\n")]);
        assert!(search_grouped(&corpus, "", 10, 3).is_empty());
        assert!(search_grouped(&corpus, "-only", 10, 3).is_empty());
    }

    #[test]
    fn access_count_boosts_hot_sections() {
        let corpus = corpus_of(&[
            ("a.md", "# Deployment\n\nhow to deploy.\n"),
            ("b.md", "# Deployment\n\nhow to deploy.\n"),
        ]);
        let node = &corpus.documents[1].nodes[0];
        for _ in 0..100 {
            node.access_count.bump();
        }
        let hits = search(&corpus, "deployment", 10);
        let top = &hits[0];
        let top_doc = corpus.doc(top.doc).unwrap();
        assert_eq!(top_doc.rel_path, "b.md");
    }
}
