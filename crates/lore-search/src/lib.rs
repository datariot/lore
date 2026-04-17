//! Search over Lore's heading index.
//!
//! This is a tiny BM25 ranker with three scored fields per node:
//! **title**, **heading path segments**, and **first-sentence summary**.
//! At ~15K nodes per corpus, brute-force scoring is perfectly fine — no
//! inverted index and no candidate filter. We keep `lore-index`'s trigram
//! map for prefix/typo work in future phases; today it is unused at query
//! time.
//!
//! The ranker is deliberately minimal (~200 LoC) so we can test it
//! deterministically and avoid a 20-crate tantivy dependency subtree.

#![deny(unsafe_op_in_unsafe_fn)]

mod bm25;

pub use bm25::{Ranker, SearchHit, search, search_bm25};

// Phase 3 called this `search_naive`; keep the alias so service code
// doesn't have to switch in lock-step with the ranker rewrite.
pub use bm25::search as search_naive;
