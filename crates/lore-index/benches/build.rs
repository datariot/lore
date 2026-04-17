//! Index-build throughput benchmark.
//!
//! Measures three phases independently:
//!
//! - `build_document` — parse + tree + summary extraction for one doc
//! - `push + rebuild_indices` — aggregate N documents into a `CorpusIndex`
//!   and build derived tables (inverted index, backlinks, trigrams, …)
//! - Full corpus build from raw strings — what `lore index` actually does
//!
//! The synthetic corpus shape is calibrated against the author's personal
//! knowledge-base (~985 docs, ~14K headings, ~150 lines per doc). CI stays
//! stable because we generate content in-process rather than cloning an
//! external repo.

use criterion::{BatchSize, Criterion, black_box, criterion_group, criterion_main};
use lore_core::SourceId;
use lore_index::{CorpusIndex, build_document};
use std::path::PathBuf;

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

fn render_doc(di: usize, headings_per_doc: usize) -> String {
    let mut src = String::new();
    src.push_str(&format!(
        "---\ntitle: Document {di}\ntype: bench\ntags:\n  - synthetic\n---\n\n"
    ));
    src.push_str(&format!("# Document {di}\n\n"));
    src.push_str("An overview paragraph with a [[cross|alias]] link.\n\n");
    for hi in 0..headings_per_doc {
        let topic = TOPICS[(di * 7 + hi) % TOPICS.len()];
        src.push_str(&format!("## {topic} {hi}\n\n"));
        src.push_str(&format!(
            "Prose about {topic}. See [[other-doc-{other}]] for context.\n\n",
            other = (di + hi) % 1000
        ));
        src.push_str(&format!(
            "### Detail {hi}\n\nMore detail about {topic}.\n\n"
        ));
    }
    src
}

fn synthetic_corpus(docs: usize, headings_per_doc: usize) -> CorpusIndex {
    let mut corp = CorpusIndex::new(SourceId::new("bench"), PathBuf::from("/tmp/bench"));
    for di in 0..docs {
        let src = render_doc(di, headings_per_doc);
        let rel = format!("doc-{di:04}.md");
        let d = build_document(SourceId::new("bench"), &rel, &src).unwrap();
        corp.push_document(d);
    }
    corp.rebuild_indices();
    corp
}

fn bench_build_document(c: &mut Criterion) {
    c.bench_function("build_document/small", |b| {
        let src = render_doc(0, 4);
        b.iter(|| {
            let _ = build_document(
                black_box(SourceId::new("x")),
                black_box("d.md"),
                black_box(&src),
            )
            .unwrap();
        })
    });
    c.bench_function("build_document/medium", |b| {
        let src = render_doc(0, 16);
        b.iter(|| {
            let _ = build_document(
                black_box(SourceId::new("x")),
                black_box("d.md"),
                black_box(&src),
            )
            .unwrap();
        })
    });
    c.bench_function("build_document/large", |b| {
        let src = render_doc(0, 64);
        b.iter(|| {
            let _ = build_document(
                black_box(SourceId::new("x")),
                black_box("d.md"),
                black_box(&src),
            )
            .unwrap();
        })
    });
}

fn bench_rebuild_indices(c: &mut Criterion) {
    // Pre-parse all docs once; measure only the `push + rebuild_indices` cost.
    let prebuilt_docs: Vec<_> = (0..200)
        .map(|di| {
            let src = render_doc(di, 8);
            build_document(SourceId::new("b"), format!("doc-{di:04}.md"), &src).unwrap()
        })
        .collect();

    c.bench_function("rebuild_indices/200_docs_x_24_nodes", |b| {
        b.iter_batched(
            || {
                let mut corp = CorpusIndex::new(SourceId::new("b"), PathBuf::from("/tmp"));
                for d in &prebuilt_docs {
                    corp.push_document(d.clone());
                }
                corp
            },
            |mut corp| {
                corp.rebuild_indices();
                black_box(corp);
            },
            BatchSize::SmallInput,
        )
    });
}

fn bench_full_corpus(c: &mut Criterion) {
    let mut group = c.benchmark_group("corpus_from_strings");
    // Steal-the-ball throughput across three corpus sizes so we can see
    // scaling behavior (ideally linear in total nodes).
    for &(docs, hd) in &[(50usize, 6usize), (200, 8), (1000, 4)] {
        let label = format!("{docs}_docs_x_{hd}_headings");
        group.bench_function(label, |b| {
            b.iter(|| {
                let corpus = synthetic_corpus(black_box(docs), black_box(hd));
                black_box(corpus);
            })
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_build_document,
    bench_rebuild_indices,
    bench_full_corpus
);
criterion_main!(benches);
