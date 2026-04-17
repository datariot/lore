//! BM25 latency benchmark.
//!
//! Builds a synthetic 2,000-node corpus and measures `search_bm25` under
//! five representative queries: high-frequency single term, low-frequency
//! single term, multi-word, typo, and empty. Criterion is driven via the
//! `--bench search` harness.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use lore_core::SourceId;
use lore_index::{CorpusIndex, build_document};
use std::path::PathBuf;

fn synthetic_corpus(docs: usize, headings_per_doc: usize) -> CorpusIndex {
    let mut corp = CorpusIndex::new(SourceId::new("bench"), PathBuf::from("/tmp/bench"));
    for di in 0..docs {
        let mut src = String::new();
        src.push_str(&format!("# Document {di}\n\n"));
        src.push_str("This document covers a variety of synthetic topics.\n\n");
        for hi in 0..headings_per_doc {
            let topic = TOPICS[(di * 7 + hi) % TOPICS.len()];
            src.push_str(&format!("## {topic} {hi}\n\n"));
            src.push_str(&format!(
                "The {topic} section discusses implementation concerns. \
                 See also the related component for wider context. \
                 Token: marker{hi}.\n\n"
            ));
            src.push_str(&format!(
                "### Detail {hi}\n\nMore detail about {topic}.\n\n"
            ));
        }
        let rel = format!("doc-{di:04}.md");
        let d = build_document(SourceId::new("bench"), &rel, &src).unwrap();
        corp.push_document(d);
    }
    corp.rebuild_indices();
    corp
}

const TOPICS: &[&str] = &[
    "caching",
    "retrieval",
    "storage",
    "networking",
    "security",
    "observability",
    "indexing",
    "ranking",
    "parsing",
    "serialization",
    "concurrency",
    "deployment",
];

fn bench_search(c: &mut Criterion) {
    // ~24 headings per doc * 100 docs = ~2,400 nodes, representative of a
    // medium repo's documentation set.
    let corpus = synthetic_corpus(100, 8);
    assert!(corpus.total_nodes() > 1000, "need a meaty corpus");

    c.bench_function("search/single_rare", |b| {
        b.iter(|| lore_search::search(black_box(&corpus), black_box("observability"), 10))
    });
    c.bench_function("search/single_common", |b| {
        b.iter(|| lore_search::search(black_box(&corpus), black_box("document"), 10))
    });
    c.bench_function("search/multi_word", |b| {
        b.iter(|| {
            lore_search::search(
                black_box(&corpus),
                black_box("caching strategy retrieval"),
                10,
            )
        })
    });
    c.bench_function("search/unknown_token", |b| {
        b.iter(|| lore_search::search(black_box(&corpus), black_box("zzzzunknownzzzz"), 10))
    });
    c.bench_function("search/empty", |b| {
        b.iter(|| lore_search::search(black_box(&corpus), black_box(""), 10))
    });
}

criterion_group!(benches, bench_search);
criterion_main!(benches);
