//! Library surface for the `lore` service binary.
//!
//! Exposed as a library so integration tests and (eventually) benchmarks can
//! drive the same code paths the CLI does without shelling out.

#![deny(unsafe_op_in_unsafe_fn)]

pub mod cli;
pub mod config;
pub mod mcp;
pub mod walker;
pub mod watch;

pub use cli::{IndexOptions, IndexReport, index_command};
pub use mcp::{CorpusRegistry, LoreServer, ServeOptions, serve_http};
pub use watch::run_watcher;
