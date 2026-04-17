//! MCP server surface for Lore.
//!
//! The server wraps a registry of loaded corpus indices and exposes tools
//! that let agents retrieve structured content — heading trees, sections by
//! path, and keyword search — without grepping, reading full files, or
//! invoking an LLM at retrieval time.

pub mod registry;
pub mod server;
pub mod tools;
pub mod transport;

pub use registry::CorpusRegistry;
pub use server::LoreServer;
pub use transport::{ServeOptions, serve_http};
