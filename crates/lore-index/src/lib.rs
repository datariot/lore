//! Heading-tree index for markdown corpora.
//!
//! This crate converts the flat events from `lore-parse` into a hierarchical
//! `DocumentIndex`, groups many documents into a `CorpusIndex`, and provides
//! the serialization, lookup, and traversal primitives the MCP server needs.

#![deny(unsafe_op_in_unsafe_fn)]

mod access;
mod builder;
mod corpus;
mod model;
mod query;
mod serialize;

pub use access::AccessCounter;
pub use builder::build_document;
pub use corpus::{CorpusIndex, DocId, Field, FieldLengths, Posting, canonical_link_keys, tokenize};
pub use model::{DocumentIndex, HeadingNode};
pub use query::{NodeRef, Traversal};
pub use serialize::{load_index, write_index};
