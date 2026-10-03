# Lore architecture

The map a fresh session reads before it starts: what the components are, what
each file defines, and where the tests and the decisions live. This is the map
escapement ADR-009 §1 asks every repo on the kernel to carry.

Two companions, not repeated here:

- [`../CLAUDE.md`](../CLAUDE.md) — conventions, the five design invariants,
  the MCP wire-field naming rules, and the gotcha list. Read it for *how to
  change* this code.
- [`daily-driver.md`](daily-driver.md) — how the author runs lore in
  production against a live Obsidian vault (install, LaunchAgent, reindex,
  how to verify a rebuild reached the daemon).

Lore indexes a markdown corpus by its heading hierarchy and serves retrieval
tools to agents over MCP. No vectors, no LLM at query time, no web
dependency — see [`../README.md`](../README.md) for the pitch.

## 1. Components

`Cargo.toml` at the repo root is the workspace manifest: `resolver = "2"`,
`edition = "2024"` in `[workspace.package]`, six members, and every third-party
version pinned once in `[workspace.dependencies]`. Formatting rules are in
`rustfmt.toml`.

| Directory | Crate | What it is |
|---|---|---|
| `crates/lore-core` | `lore-core` | Shared ids, value types, and the error enum. The leaf of the dependency graph: every other crate depends on it, it depends on none of them. |
| `crates/lore-parse` | `lore-parse` | `pulldown-cmark` event extraction: frontmatter peeling, heading events, inline + wiki links, Dataview blocks, first-sentence summaries. Emits a flat `ParsedDoc`; builds no tree. |
| `crates/lore-index` | `lore-index` | The heading tree and everything derived from it: `DocumentIndex`, `CorpusIndex`, the derived lookup tables, OKF projection, and the on-disk format. |
| `crates/lore-search` | `lore-search` | The BM25 ranker over three scored fields (title, heading-path segments, summary) plus the coverage verdict. |
| `crates/lore-watch` | `lore-watch` | A `notify::RecommendedWatcher` behind a tokio `mpsc` channel of coarse `WatchEvent`s, with a debouncer (250 ms by default) collapsing an editor's rapid-fire modify/create bursts. |
| `services/lore` | `lore` (bin) + `lore_service` (lib) | The only binary: the clap CLI and the `rmcp` MCP server over Streamable HTTP. |

**The library crates do zero I/O.** `services/lore` is the only crate that
touches the filesystem, HTTP, or the tokio runtime — the parsers, the builder,
the ranker, and the decay math all take data in and hand data back, which is
what lets the property tests in `crates/lore-index/tests/properties.rs` drive
them on generated input with no fixture directory. The one qualification:
`crates/lore-watch` owns the OS watch handle and the channel, because the
platform-specific surface is better kept in one small crate than smeared
across the service; it receives *paths*, never opens or reads a file, and the
service does every read.

`services/lore` exposes itself as a library (`services/lore/src/lib.rs`:
`cli`, `config`, `eval`, `export`, `mcp`, `walker`, `watch`) so the
integration tests drive the same code paths the CLI does without shelling out.

## 2. Main types and entry points

### Types

| File | Defines |
|---|---|
| `crates/lore-core/src/lib.rs` | `SourceId` (corpus identifier, usually the root's basename), `NodeId` (dense `u32` into a document's node arena), `HeadingPath` (root-to-leaf heading ancestry), `ByteRange` (half-open `[start, end)` into the source), `Link` + `LinkKind` (`Inline` / `Wiki`), `Error` and the `Result<T>` alias |
| `crates/lore-parse/src/lib.rs` | `ParsedDoc` and `parse_document` — the flat parse output: optional frontmatter, heading events, link events |
| `crates/lore-index/src/model.rs` | `HeadingNode` (title, level, `byte_range`, `content_range`, `summary`, `outbound_links`, children/parent, `access_count`) and `DocumentIndex` (`rel_path`, `file_hash`, `frontmatter`, `modified_at`, the `nodes` arena, `roots`, `body_offset`) |
| `crates/lore-index/src/corpus.rs` | `CorpusIndex` plus `DocId`, `Field`, `Posting`, `FieldLengths` — and `rebuild_indices`, which clears and repopulates every derived table: `heading_lookup`, `title_trigrams`, `path_to_doc`, `backlinks`, `section_backlinks`, `doc_key_lookup`, `inverted`, `field_lengths`. All of them are `#[serde(skip)]`; they are rebuilt on load, never persisted |
| `crates/lore-index/src/access.rs` | `AccessCounter` — the in-memory, never-serialized per-node tally that feeds the BM25 boost and resets on restart |
| `crates/lore-index/src/hotstore.rs` | `AccessStore` and `AccessRecord` — the *persisted* hotness signal, keyed by `(rel_path, heading_path)` so counts survive a reindex that renumbers nodes, with `DEFAULT_HALF_LIFE_SECS` of two weeks. Pure decay math; the service owns the file |
| `crates/lore-index/src/okf.rs` | `TrustTier` and the Open Knowledge Format accessors `concept_type`, `status`, `stale_after`, `is_declared_stale`, `trust_tier` — a pure projection over decoded frontmatter, safe at query time |
| `crates/lore-index/src/query.rs` | `NodeRef`, `Traversal` — tree-walking primitives over a `DocumentIndex` |
| `crates/lore-search/src/bm25.rs` | `Ranker`, `SearchHit`, `GroupedSearchHit`, `Coverage`, `CoverageReport` |
| `crates/lore-watch/src/lib.rs` | `WatchEvent` (`Upsert` / `Remove`), `WatchHandle`, and `watch` |
| `services/lore/src/mcp/registry.rs` | `CorpusRegistry` — every loaded corpus as `CorpusHandle = Arc<RwLock<CorpusIndex>>` in a `DashMap`, the registered roots, the per-corpus `AccessStore`, and the mmap cache |
| `services/lore/src/mcp/server.rs` | `LoreServer` — the `#[tool_router]` impl holding every MCP tool handler, plus its `ServerHandler` impl |
| `services/lore/src/cli.rs` | `IndexOptions`, `IndexReport` |
| `services/lore/src/walker.rs` | `WalkOptions`, `PathFilter`, `IGNORE_FILES` |
| `services/lore/src/eval.rs` | `EvalQuery`, `QueryResult`, `EvalSummary` |
| `services/lore/src/mcp/transport.rs` | `ServeOptions` (bind address, mount path) |
| `services/lore/src/mcp/tools.rs` | Every MCP request/response type — `SearchRequest`/`SearchResponse`, `GetSectionRequest`/`SectionResponse`, `TocRequest`/`TocResponse`, `CorpusMapRequest`/`CorpusMapResponse`, and the rest |

### Entry points

| Function | File | Role |
|---|---|---|
| `build_document` | `crates/lore-index/src/builder.rs` | `ParsedDoc` → `DocumentIndex`. One linear walk of the heading stream over a stack of open ancestors; byte ranges close as each node is popped. Pure — it cannot stat, so the service stamps `modified_at` afterwards |
| `index_command` | `services/lore/src/cli.rs` | Executes `lore index`: canonicalize the root, walk, build each document, `rebuild_indices`, write, return an `IndexReport` |
| `write_index` / `load_index` | `crates/lore-index/src/serialize.rs` | The on-disk format. `load_index` rejects a mismatched magic, then calls `rebuild_indices` |
| `serve_http` | `services/lore/src/mcp/transport.rs` | Mounts an `rmcp` `StreamableHttpService` wrapping a fresh `LoreServer` per session under an axum `Router`, binds the `TcpListener`, and blocks |
| `run_watcher` | `services/lore/src/watch.rs` | Consumes debounced `WatchEvent`s, maps each path back to `(source_id, rel_path)`, re-checks it against the corpus's `PathFilter`, and routes to `reindex_document` or `remove_document` |
| `run_eval` / `eval_command` | `services/lore/src/eval.rs` | Scores a labeled JSONL query set: Success@1/3/10, MRR, coverage-verdict accuracy |
| `render_llms_txt` / `export_command` | `services/lore/src/export.rs` | Projects the index into `llms.txt` (and `llms-full.txt` with `--full`) for agents that do not speak MCP |
| `main` | `services/lore/src/main.rs` | clap parsing, tracing setup, and the 30-second dirty-gated access-store flush task |

### The indexer path, traced once

`walk_markdown` (`services/lore/src/walker.rs`, the `ignore` crate's traversal
plus extension filtering and a `.lore` exclusion) → `parse_document`
(`crates/lore-parse/src/lib.rs`) → `build_document`
(`crates/lore-index/src/builder.rs`) → `CorpusIndex::push_document` then
`CorpusIndex::rebuild_indices` (`crates/lore-index/src/corpus.rs`) →
`write_index` (`crates/lore-index/src/serialize.rs`).

The walk and the watcher are two admission paths into the same corpus and must
agree on what counts as a document — `PathFilter` in
`services/lore/src/walker.rs` reproduces the traversal's rules for a single
path. `../CLAUDE.md` has the failure that motivated it.

### CLI

Five subcommands, defined in `services/lore/src/main.rs`:

| Command | What it does |
|---|---|
| `lore index` | Build a corpus index from a directory of markdown files |
| `lore serve` | Serve already-indexed corpora over MCP Streamable HTTP (default bind `127.0.0.1:7331`, mount `/mcp`) |
| `lore watch` | `serve` plus a debounced filesystem watcher that re-indexes changed files in place |
| `lore eval` | Run a labeled query set through the real ranker and report retrieval effectiveness |
| `lore export` | Write `llms.txt` (and `llms-full.txt` with `--full`) for the corpus |

### MCP surface

Eleven tools, all routed by the `#[tool_router]` impl in
`services/lore/src/mcp/server.rs`. Each takes `Parameters<Req>` and returns
`Json<Resp>`; the request and response types live in
`services/lore/src/mcp/tools.rs`, and the ranker behind `search` is
`crates/lore-search/src/bm25.rs`.

| Tool | What it returns |
|---|---|
| `list_sources` | Every loaded corpus: source id, root directory, document and heading counts |
| `list_documents` | Documents in a corpus, filterable by `path_prefix` and frontmatter equality |
| `table_of_contents` | The heading tree, nested as `roots[].children[]`, optionally one document and capped by `max_depth` |
| `corpus_map` | Folder hierarchy → documents, each with title, description, and a heading preview — the orient-yourself call for a large source |
| `get_section` | A section (or a whole document) as an O(1) byte-range slice of a cached memory map |
| `search` | BM25 hits over title, path segments, and summary, with a `full`/`partial`/`none` coverage verdict and per-hit `age_days` / staleness / OKF fields |
| `backlinks` | Every section linking *to* a target, optionally narrowed by `target_anchor`; precomputed at index time |
| `recent_hot` | Top-N sections by time-decayed access score, from the persisted `AccessStore` |
| `neighbors` | A node's parent, previous sibling, next sibling, and children — one navigation hop |
| `get_by_path` | `get_section` addressed by one qualified-path string of the form `rel_path#Heading > Sub` |
| `add_source` | Index a directory and register it as a new corpus at runtime |

The wire-field naming rules (`heading_path`, `rel_path`, never a bare `path`)
and the accepted request-shape aliases are specified in `../CLAUDE.md`.

### Storage

Path constants live in `services/lore/src/config.rs`. `LORE_DIR`,
`INDEX_FILE`, and `ACCESS_FILE` spell `.lore/index.json` and
`.lore/access.json`; `index_path` and `access_path` join them onto a corpus
root. `MARKDOWN_EXTENSIONS` is the set the walker admits.

- **The index** is one `serde_json` file per corpus at `.lore/index.json`
  inside the corpus root, written and read by
  `crates/lore-index/src/serialize.rs`. It carries a format tag from that
  file's `MAGIC` constant, currently `"lore-index-v3"`; `load_index` refuses a
  file whose magic does not match rather than deserializing garbage. Only the
  documents and the heading trees are persisted — every derived table is
  `#[serde(skip)]` and rebuilt on load.
- **The usage sidecar** is `.lore/access.json`, the serialized `AccessStore`
  from `crates/lore-index/src/hotstore.rs`. It is deliberately a separate file
  so usage data accumulating never bumps the index format, and it is flushed
  dirty-gated every 30 seconds by a task in `services/lore/src/main.rs`.
- **Section reads** never re-parse markdown. `get_section` slices a
  `memmap2::Mmap` of the *original* file; the mmap cache lives in
  `CorpusRegistry` (`services/lore/src/mcp/registry.rs`) keyed by
  `(SourceId, rel_path)` and is invalidated when a corpus is reloaded or a
  single document is re-indexed.

## 3. Tests, benches, and the gate

| Where | What |
|---|---|
| `crates/lore-index/tests/properties.rs` | Property tests over generated markdown: byte-range contiguity, sibling non-overlap, tree depth equals heading-path length, serde round-trip, every body byte covered exactly once |
| inline `#[cfg(test)]` modules | Unit tests next to the code, in every file doing non-trivial work |
| `services/lore/tests/mcp_server.rs` | The end-to-end wire test: a real server on a loopback port, raw JSON-RPC over Streamable HTTP, every tool exercised against the fixture corpus. Deliberately does not use the `rmcp` client |
| `services/lore/tests/caller_shapes.rs` | The request shapes lifted from real transcripts that the server used to refuse, driven over the same wire |
| `services/lore/tests/watch.rs` | Start a watched server, change a file on disk, assert the MCP surface reflects it within a bounded window |
| `services/lore/tests/index_roundtrip.rs` | `lore index` the fixture, load the index back, assert structural properties |
| `services/lore/tests/eval_fixture.rs` | The retrieval-quality gate: scores `eval/mini-kb.jsonl` against the fixture corpus and fails on a ranking or coverage-verdict regression |
| `services/lore/tests/fixtures/mini-kb` | The shared fixture corpus the integration tests and the eval gate index |
| `eval/mini-kb.jsonl`, `eval/knowledge-base.jsonl` | Labeled query sets. The first is CI-gated; the second is the larger vault set, run by hand |
| `crates/lore-search/benches/search.rs` | Criterion bench for BM25 query latency |
| `crates/lore-index/benches/build.rs` | Criterion bench for index build throughput |
| `crates/lore-index/examples/dump_tree.rs` | A debugging example: pass it a markdown file and it prints the heading tree the builder produced |

`scripts/gate.sh` is the single merge gate — fmt, clippy with
`-D warnings`, `cargo test --workspace --all-targets`, and a bench-compile
pass. CI (`.github/workflows/ci.yml`) and the escapement kernel both run it,
so one script defines what GREEN means. Exit 0 is GREEN; anything else failed.

## 4. Decision records

`docs/decisions/` holds the running log of design decisions, each one dated
with a `status` in its frontmatter and chained by a `follows:` field.

| File | Covers |
|---|---|
| `docs/decisions/0001-dogfood-knowledge-base.md` | First end-to-end run against a real 1000-file Obsidian vault — index size, build and query timings, every tool exercised over HTTP from `curl` |
| `docs/decisions/0002-effectiveness-notes.md` | Second pass on the same corpus, scored on *agent* effectiveness rather than server correctness: latency budget, what an agent gets right on the first call, what it gives up on |
| `docs/decisions/0003-roadmap-revision.md` | Post-validation roadmap revision — the core bet held, so priority moves to corpus-level structure, honest negatives, and living with the tool |
| `docs/decisions/0004-effectiveness-harness.md` | Why `lore eval` exists and how it is computed: Success@k, MRR, coverage-verdict accuracy, the query-set format, and the baseline numbers the CI floors came from |
| `docs/decisions/0005-okf-alignment.md` | Google Cloud's Open Knowledge Format, why it does not compete with lore, and which OKF frontmatter fields `crates/lore-index/src/okf.rs` projects onto responses |
