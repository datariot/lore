# CLAUDE.md

Guidance for Claude Code when working in this repo.

## What Lore is

A Rust MCP server that indexes a markdown corpus by its heading hierarchy and serves retrieval tools to agents. No vectors, no LLM at query time, no web dependency. Read `README.md` for the user-facing pitch.

## Workspace layout

```
lore/
├── Cargo.toml                 workspace manifest, edition = 2024
├── crates/
│   ├── lore-core/             SourceId, NodeId, HeadingPath, ByteRange, Link, Error
│   ├── lore-parse/            pulldown-cmark events, frontmatter, wiki-links, Dataview
│   ├── lore-index/            HeadingNode model, builder, corpus indices, serialization
│   ├── lore-search/           BM25 ranker, Criterion bench
│   └── lore-watch/            notify-rs wrapper with 250 ms debouncer
└── services/
    └── lore/                  single binary: clap CLI + rmcp server over Streamable HTTP
```

**Library crates have zero I/O.** The `lore` service is the only place that touches the filesystem, HTTP, or the tokio runtime. Keep it that way — property tests depend on it.

## Commands

```bash
cargo check --workspace                        # fast compile check
cargo test --workspace                         # all unit, integration, property tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo bench -p lore-search --bench search      # BM25 latency
cargo run -p lore-index --example dump_tree -- file.md   # debug heading tree

cargo run -p lore -- index /path/to/vault
cargo run -p lore -- serve -r /path/to/vault
cargo run -p lore -- watch -r /path/to/vault --debounce-ms 250
```

## Design invariants

1. **Markdown's heading hierarchy IS the index.** Do not add LLM summarization or vector embeddings to the retrieval path.
2. **Byte ranges reference the *original* source** — after frontmatter is peeled, offsets are shifted back. `get_section` is a pure mmap slice; never re-parse at query time.
3. **No indexing work at query time.** Everything derived (inverted index, backlinks, trigrams, field lengths, path_to_doc) is built in `CorpusIndex::rebuild_indices`. If you add a new derived structure, populate it there and clear it at the top.
4. **Registry uses `Arc<RwLock<CorpusIndex>>`** — queries take read locks, the watcher takes a write lock during re-index. Don't hold a read lock across an `.await`.
5. **Mmap cache is keyed by `(SourceId, rel_path)`** and invalidated on reindex/remove.

## Test strategy

- **Property tests** live in `crates/lore-index/tests/properties.rs` — byte-range contiguity, sibling non-overlap, tree depth equals path length, serde round-trip, every-body-byte-covered-exactly-once. 128 cases each.
- **Unit tests** are inline `#[cfg(test)]` modules in every file that does non-trivial work.
- **Integration tests** in `services/lore/tests/` drive the real MCP server over HTTP with `reqwest`. Don't depend on the `rmcp` client — the wire-protocol test is more valuable.
- **Criterion benches** live under `crates/*/benches/`.

If you add a new derived index or a new MCP tool, write:
1. a unit test in the crate that owns it,
2. an MCP integration test in `services/lore/tests/mcp_server.rs`,
3. a property test *if* the behavior is an invariant rather than a specific output.

## Rust conventions

- Edition 2024, `max_width = 100` in `rustfmt.toml`.
- `thiserror` for crate errors with a per-crate `pub type Result<T>` alias.
- `#[deny(unsafe_op_in_unsafe_fn)]` at every `lib.rs`.
- `hashbrown::HashMap` in hot paths (imported as `HashMap`).
- `parking_lot::RwLock` over `std::sync::RwLock`.
- No `unwrap()` outside tests or `main`.

## MCP surface

Nine tools in `services/lore/src/mcp/server.rs`, all routed by `#[tool_router]`. Request/response types live in `mcp/tools.rs` so the server file stays readable. Tools take `Parameters<Req>` and return `Json<Resp>`.

When adding a tool:

1. Define `FooRequest` / `FooResponse` in `mcp/tools.rs` with `#[derive(Serialize, Deserialize, JsonSchema)]`.
2. Add a `#[tool(description = "...")]` method on `LoreServer`. Descriptions are consumed by agents — write them for an LLM reader, not a human one.
3. Extend the end-to-end test in `services/lore/tests/mcp_server.rs` to exercise the new tool.

## File paths worth knowing

- `crates/lore-core/src/lib.rs` — shared types
- `crates/lore-parse/src/obsidian.rs` — Dataview detection (Phase 6)
- `crates/lore-index/src/model.rs` — `HeadingNode`, `DocumentIndex`
- `crates/lore-index/src/builder.rs` — AST → tree + range finalization + link/Dataview attachment
- `crates/lore-index/src/corpus.rs` — `rebuild_indices`, inverted index, canonical link keys
- `crates/lore-search/src/bm25.rs` — ranker
- `services/lore/src/mcp/registry.rs` — `CorpusRegistry`, mmap cache, reindex
- `services/lore/src/mcp/server.rs` — all nine MCP tool handlers
- `services/lore/src/watch.rs` — watcher → registry bridge

## Common gotchas

- **macOS** canonicalizes `/tmp` to `/private/tmp`. `CorpusRegistry::install` canonicalizes the root so `locate()` matches notify's canonical paths. Don't revert that.
- **macOS FSEvents** sometimes delivers deletes as Modify-then-nothing. `watch.rs` normalizes by checking `path.exists()` on Upsert events.
- **`pulldown-cmark` offset iteration** gives byte positions into the *body* passed to the parser. We always add `body_offset` to shift back into the original source.
- **Wiki-links in code fences** must be excluded — see `build_code_mask` in `lore-parse/src/links.rs`.
- **`access_count` is `#[serde(skip)]`** so the on-disk index is stable. Counters live only in memory.

## Substack article

David plans to write about this. Decisions worth capturing as we work:

- Why no vectors: the heading tree is the semantic structure authors produced.
- Why no LLM at query time: the agent IS the planner.
- Why `pulldown-cmark` over `comrak`: zero-copy offset iteration vs AST allocation.
- Why a single JSON file per corpus: mmap + serde_json is fast enough that fancier formats are premature.
- Why BM25 over tantivy: 15K headings fit in L2; a 200-line ranker beats a 20-crate subtree.
- The ByteRover access-count trick: agent usage IS the importance signal.

Commit messages are the running log — write them with that in mind.
