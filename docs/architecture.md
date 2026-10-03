# Lore architecture — the map

The map a session reads before it starts work, per escapement ADR-009 §1: what
the components are, where the main types and entry points live, where the tests
and the merge gate are, and which decision record covers what.

This file answers *where does X live*. It does not restate the rules:

- [`../CLAUDE.md`](../CLAUDE.md) — conventions, the five design invariants,
  the MCP wire-field naming rules, and the gotcha list. Read it for *why you
  must not* change something.
- [`daily-driver.md`](daily-driver.md) — how the author runs Lore continuously
  against a live vault (install, LaunchAgent, Claude Code registration,
  verification).
- [`../README.md`](../README.md) — the user-facing pitch.

Lore is a Rust MCP server that indexes a markdown corpus by its heading
hierarchy and serves retrieval tools to agents. No vectors, no LLM at query
time, no web dependency.

## 1. Components

[`../Cargo.toml`](../Cargo.toml) is the workspace manifest: six members, one
`[workspace.package]` block pinning `edition = "2024"`, and all third-party
versions pinned in `[workspace.dependencies]`.

| Directory | Crate | What it owns |
|---|---|---|
| [`../crates/lore-core`](../crates/lore-core) | `lore-core` | Shared ids, the byte-range primitive, the link model, and the one error enum every crate returns. Depends on nothing internal. |
| [`../crates/lore-parse`](../crates/lore-parse) | `lore-parse` | `pulldown-cmark` event extraction: frontmatter peeling, heading events, inline + wiki links, Dataview blocks, first-sentence summaries, and the shared parser option set. |
| [`../crates/lore-index`](../crates/lore-index) | `lore-index` | The heading tree: flat parse events → `DocumentIndex`, many documents → `CorpusIndex`, the derived lookup tables, the JSON index format, traversal, the OKF frontmatter projection, and the access signals. |
| [`../crates/lore-search`](../crates/lore-search) | `lore-search` | The BM25 ranker, query parsing (negated terms), doc-grouped hits, and the coverage verdict. |
| [`../crates/lore-watch`](../crates/lore-watch) | `lore-watch` | A `notify::RecommendedWatcher` behind a tokio `mpsc` channel of coarse `WatchEvent`s, with a 250 ms debouncer that collapses editors' rapid-fire modify/create bursts. |
| [`../services/lore`](../services/lore) | `lore` (bin) + `lore_service` (lib) | The only binary: a clap CLI and the rmcp MCP server over Streamable HTTP, plus the walker, the watcher bridge, eval, and export. |

**The library crates do zero I/O, and `services/lore` is the only crate that
touches the filesystem, HTTP, or the tokio runtime.** Property tests depend on
that: the builder is a pure function of `&str`, so `proptest` can hammer it
without a tempdir. Two narrow, deliberate exceptions are documented in the
source: `crates/lore-index/src/serialize.rs` opens the index file itself (it is
the format's owner), and `crates/lore-watch` wraps `notify`, which is "the one
piece of Lore that does real I/O outside the service binary" and is kept small
so OS-specific behaviour stays concentrated. Everything else — parsing,
building, ranking, decay math — takes bytes in and returns values. The access
store is the pattern to copy: `AccessStore` is pure decay math plus serde, and
`CorpusRegistry` owns reading and writing its sidecar file.

## 2. Main types and entry points

### Types

| File | Defines |
|---|---|
| [`../crates/lore-core/src/lib.rs`](../crates/lore-core/src/lib.rs) | `SourceId` (corpus identifier), `NodeId` (index into a document's node arena), `HeadingPath` (the heading-segment ancestry), `ByteRange` (`start`/`end` into the original file, with `slice`), `Link` + `LinkKind` (`Inline` / `Wiki`), `Error` (`Parse` / `Io` / `NotFound` / `Serialize`) and `pub type Result<T>`. |
| [`../crates/lore-index/src/model.rs`](../crates/lore-index/src/model.rs) | `HeadingNode` — level, title, `path`, `byte_range` (heading line through next sibling), `content_range` (body only), `summary`, `outbound_links`, `children`/`parent`, optional `kind`, and the `#[serde(skip)]` `access_count`. `DocumentIndex` — `rel_path`, `file_hash` (xxh3), decoded `frontmatter`, `modified_at` (mtime, persisted), the `nodes` arena, `roots`, plus the OKF accessors (`okf_type`, `okf_status`, `trust_tier`, `is_declared_stale`) and `age_days`. |
| [`../crates/lore-index/src/corpus.rs`](../crates/lore-index/src/corpus.rs) | `CorpusIndex` — `source`, `root_dir`, `documents`, and every derived table, all `#[serde(skip)]`: `heading_lookup`, `title_trigrams`, `path_to_doc`, `backlinks`, `section_backlinks`, `doc_key_lookup`, `inverted`, `field_lengths`. `rebuild_indices` is the one place they are populated (it clears each at the top); `DocId`, `Field`, `Posting`, `FieldLengths`, `tokenize`, `canonical_link_keys`, and `trigrams_of` live here too. |
| [`../crates/lore-index/src/hotstore.rs`](../crates/lore-index/src/hotstore.rs) | `AccessStore` and `AccessRecord` — the *persisted* hotness signal. Keyed by `(rel_path, heading_path)` so counts survive a reindex that renumbers nodes; `bump` decays to now and adds 1; `decayed` and `ranked` read it back. `DEFAULT_HALF_LIFE_SECS` is a two-week half-life. Distinct from `AccessCounter` in [`../crates/lore-index/src/access.rs`](../crates/lore-index/src/access.rs), the in-memory per-node atomic that feeds the BM25 boost and resets on restart. |
| [`../services/lore/src/mcp/registry.rs`](../services/lore/src/mcp/registry.rs) | `CorpusRegistry` — a `DashMap<SourceId, CorpusHandle>` where `CorpusHandle = Arc<parking_lot::RwLock<CorpusIndex>>`, plus the mmap cache, the registered roots (for mapping a changed path back to a source), and the per-corpus `AccessEntry` with its dirty flag. `locate`, `reindex_document`, `remove_document`, `mmap_document`, `bump_access`, `flush_access`. |
| [`../services/lore/src/mcp/server.rs`](../services/lore/src/mcp/server.rs) | `LoreServer` — holds the registry and a `ToolRouter` built once in `new()`; every MCP tool handler is a method on it. |

### Entry points

| File | Function |
|---|---|
| [`../crates/lore-index/src/builder.rs`](../crates/lore-index/src/builder.rs) | `build_document(source, rel_path, src) -> Result<DocumentIndex>` — parse events to tree, byte-range finalization, link and Dataview attachment. Pure: it cannot stat, so the service stamps `modified_at` afterwards. |
| [`../services/lore/src/cli.rs`](../services/lore/src/cli.rs) | `index_command(IndexOptions) -> Result<IndexReport>` — the whole `lore index` run; `read_and_build` is the per-file step that calls `build_document` and stamps the mtime. |
| [`../crates/lore-index/src/serialize.rs`](../crates/lore-index/src/serialize.rs) | `write_index(path, &CorpusIndex)` (creates the parent dir, writes the magic-stamped envelope) and `load_index(path) -> Result<CorpusIndex>` (rejects a foreign magic, then calls `rebuild_indices`). |
| [`../services/lore/src/mcp/transport.rs`](../services/lore/src/mcp/transport.rs) | `serve_http(registry, ServeOptions)` — mounts rmcp's `StreamableHttpService` under `ServeOptions::path` on an axum `Router` and serves until the listener errors. Defaults: `127.0.0.1:7331`, `/mcp`. |
| [`../services/lore/src/watch.rs`](../services/lore/src/watch.rs) | `run_watcher(registry, debounce)` — maps each `WatchEvent` back to `(source_id, rel_path)` and routes it to `reindex_document` or `remove_document`. `DEFAULT_DEBOUNCE` is 250 ms. |
| [`../services/lore/src/walker.rs`](../services/lore/src/walker.rs) | `walk_markdown(root, &WalkOptions) -> Vec<PathBuf>` (the traversal) and `PathFilter::accepts` (the same admission rules for a single path, which is all the watcher ever has). These two must agree — see the gotcha in [`../CLAUDE.md`](../CLAUDE.md). |
| [`../services/lore/src/eval.rs`](../services/lore/src/eval.rs) | `eval_command(root, queries_path, limit)`, `run_eval(&CorpusIndex, &[EvalQuery], limit) -> EvalSummary`, `parse_query_set`. |
| [`../services/lore/src/export.rs`](../services/lore/src/export.rs) | `export_command(root, out_dir, full)`, `render_llms_txt`, `render_llms_full`. |
| [`../services/lore/src/main.rs`](../services/lore/src/main.rs) | `main` — tracing init, clap parse, dispatch. `lore serve` and `lore watch` share `run_serve`, which also spawns the 30-second dirty-gated `flush_access` task. |

The indexer path, worth tracing once — `index_command` in
`services/lore/src/cli.rs` is the spine, and it runs:

1. `walk_markdown` (`services/lore/src/walker.rs`) — the corpus root in,
   markdown paths out, with hidden/gitignore handling from the `ignore` crate.
2. read the file, then parse it with `lore-parse` (`parse_document` peels
   frontmatter and emits heading, link, and Dataview events with original-source
   offsets).
3. `build_document` (`crates/lore-index/src/builder.rs`) — events to tree;
   `cli.rs` stamps `modified_at` on the result.
4. `CorpusIndex::push_document`, then one `CorpusIndex::rebuild_indices` at the
   end (`crates/lore-index/src/corpus.rs`) — every derived table, once.
5. `write_index` (`crates/lore-index/src/serialize.rs`) — `.lore/index.json`.

The query path is the second half run backwards: `load_index` (which rebuilds
the derived tables), then a read lock on the handle and, for `get_section`, a
byte-range slice of a cached mmap. No parsing, no index building, no LLM.

### CLI

Five subcommands, defined as the `Command` enum in
[`../services/lore/src/main.rs`](../services/lore/src/main.rs):

| Command | Does |
|---|---|
| `lore index` | Build `.lore/index.json` for a corpus root. `--source-id` overrides the id (default: the root's basename); `--json` prints the `IndexReport`. |
| `lore serve` | Load one or more `-r/--root`s and serve MCP over Streamable HTTP on `--bind` under `--path`. Each root must already be indexed. |
| `lore watch` | `serve` plus a debounced watcher over every root, re-indexing affected files in place. `--debounce-ms` (default 250). |
| `lore eval` | Score a labeled `-q/--queries` JSONL set: Success@1/3/10, MRR, coverage-verdict accuracy. Indexes on the fly if the index is absent. |
| `lore export` | Write `llms.txt` into `--out` (plus `llms-full.txt` with `--full`), or print `llms.txt` to stdout when `--out` is omitted — a link-first map for agents that don't speak MCP. Indexes on the fly if the index is absent. |

### MCP surface

Eleven tools, all routed by the `#[tool_router]` impl in
[`../services/lore/src/mcp/server.rs`](../services/lore/src/mcp/server.rs).
Request/response types live in
[`../services/lore/src/mcp/tools.rs`](../services/lore/src/mcp/tools.rs) (each
`#[derive(Serialize, Deserialize, JsonSchema)]`, plus the `caller_shapes`
module pinning the payloads callers actually send); the ranker behind `search`
is [`../crates/lore-search/src/bm25.rs`](../crates/lore-search/src/bm25.rs).

| Tool | Returns |
|---|---|
| `list_sources` | Every loaded corpus: id, root dir, document and heading counts. |
| `list_documents` | Documents in a corpus, filterable by `path_prefix` and by frontmatter equality. |
| `table_of_contents` | The heading tree, nested `roots[].children[]`, optionally one document and capped by `max_depth`. |
| `corpus_map` | Folder hierarchy → documents, each with title, description, and a heading preview. The orient-yourself call for a large corpus. |
| `get_section` | A section, or a whole document when no section is named — an O(1) byte-range slice of a cached mmap. Bumps both access signals. |
| `search` | Ranked BM25 hits over heading titles, path segments, and summaries, plus a `coverage` verdict (`full`/`partial`/`none`) and per-hit freshness/OKF fields. Spans every corpus when `source_id` is omitted. |
| `backlinks` | Every section linking *to* a target, from the precomputed table; `target_anchor` narrows to `[[target#anchor]]` links. |
| `recent_hot` | Top-N sections by decayed access score, from the persisted `AccessStore`. |
| `neighbors` | A node's parent, previous sibling, next sibling, and children — one navigation hop. |
| `get_by_path` | A section named by one qualified string, `path/to/file.md#Heading > Subheading`. Wraps `get_section`. |
| `add_source` | Index a directory and register it as a new corpus (or load its existing index when `rebuild` is false). |

### Storage

| What | Where | Notes |
|---|---|---|
| Corpus index | `.lore/index.json` | One `serde_json` file per corpus, written and read by [`../crates/lore-index/src/serialize.rs`](../crates/lore-index/src/serialize.rs). The envelope carries the format magic from that file's `MAGIC` constant, currently `"lore-index-v3"`; `load_index` rejects anything else rather than mis-ranking against an older tokenizer or reporting every document as unknown-age. |
| Usage sidecar | `.lore/access.json` | The serialized `AccessStore`. Deliberately separate from the index so usage data accumulating never bumps the index format — and a corrupt sidecar falls back to an empty store rather than blocking serving. |
| Document reads | mmap of the original file | `CorpusRegistry::mmap_document` keyed by `(SourceId, rel_path)`, invalidated on reindex and on remove. |
| Path constants | [`../services/lore/src/config.rs`](../services/lore/src/config.rs) | `LORE_DIR` (`.lore`), `INDEX_FILE` (`index.json`), `ACCESS_FILE` (`access.json`), `MARKDOWN_EXTENSIONS`, and the `index_path` / `access_path` / `rel_path` / `default_source_id` / `file_mtime_secs` / `now_unix_secs` helpers. |

## 3. Tests, benches, and the gate

| Path | What it covers |
|---|---|
| [`../crates/lore-index/tests/properties.rs`](../crates/lore-index/tests/properties.rs) | The structural invariants that must hold for *any* markdown: byte-range contiguity, sibling non-overlap, depth equals path length, serde round-trip, every body byte covered exactly once. |
| inline `#[cfg(test)]` modules | Unit tests, in every file that does non-trivial work — including `walker.rs` (where `filter_agrees_with_walk_on_a_mixed_tree` lives), `corpus.rs`, `bm25.rs`, and the `caller_shapes` module at the foot of `tools.rs`. |
| [`../services/lore/tests/mcp_server.rs`](../services/lore/tests/mcp_server.rs) | The real server on a loopback port, driven with raw JSON-RPC over Streamable HTTP — every tool against the `services/lore/tests/fixtures/mini-kb` corpus. Deliberately not via the rmcp client, so the wire shape is what's asserted. |
| [`../services/lore/tests/caller_shapes.rs`](../services/lore/tests/caller_shapes.rs) | The request shapes lifted from real transcripts, driven over the wire. A newly-observed refused shape goes here first. |
| [`../services/lore/tests/watch.rs`](../services/lore/tests/watch.rs) | Watched server, file changed on disk, MCP surface reflects it within a bounded window. Needs real filesystem-event delivery — see the sandboxing gotcha in [`../CLAUDE.md`](../CLAUDE.md). |
| [`../services/lore/tests/index_roundtrip.rs`](../services/lore/tests/index_roundtrip.rs) | `lore index` the fixture corpus, load it back, assert structural properties. |
| [`../services/lore/tests/eval_fixture.rs`](../services/lore/tests/eval_fixture.rs) | **The retrieval-quality gate.** Runs [`../eval/mini-kb.jsonl`](../eval/mini-kb.jsonl) against the mini-kb fixture and asserts the metric floors. A ranking or coverage-verdict regression fails here. The larger vault set is [`../eval/knowledge-base.jsonl`](../eval/knowledge-base.jsonl), not CI-gated. |
| [`../crates/lore-search/benches/search.rs`](../crates/lore-search/benches/search.rs) | Criterion: `search_bm25` latency on a synthetic 2,000-node corpus across five query shapes (frequent term, rare term, multi-word, typo, empty). |
| [`../crates/lore-index/benches/build.rs`](../crates/lore-index/benches/build.rs) | Criterion: index-build throughput in three independent phases — `build_document` for one doc, `push` + `rebuild_indices` over N docs, and the full corpus build from raw strings (what `lore index` does). The synthetic corpus shape is calibrated against the author's real vault, so CI never clones one. |
| [`../crates/lore-index/examples/dump_tree.rs`](../crates/lore-index/examples/dump_tree.rs) | Not a test — the debug tool for eyeballing one file's heading tree. |

[`../scripts/gate.sh`](../scripts/gate.sh) is the single merge gate, running
four stages in order — `fmt` (`--check`), `clippy` (`--workspace
--all-targets -D warnings`), `test` (`--workspace --all-targets`, which
includes the eval fixture), and `bench-compile` (`cargo bench --no-run`).
Exit 0 is GREEN. It exists so a kernel ship and
[`../.github/workflows/ci.yml`](../.github/workflows/ci.yml) agree on what
GREEN means; a phase argument, if passed, is ignored, because lore's gate is
one pass.

## 4. Decision records

In [`../docs/decisions`](../docs/decisions), oldest first. Each one's
frontmatter carries its `date`, its `status`, and the record it `follows`.

| File | Covers |
|---|---|
| [`0001-dogfood-knowledge-base.md`](decisions/0001-dogfood-knowledge-base.md) | The first end-to-end run against a real Obsidian vault: build/write timings, per-tool latencies, index size per node, what worked, and six friction points in priority order (schema field-name inconsistency, the then-flat TOC wire format, phantom setext headings from daily-note templates, uncanonicalized backlink keys, and two response-shape problems). Status: notes. |
| [`0002-effectiveness-notes.md`](decisions/0002-effectiveness-notes.md) | Second pass on the same corpus, aimed at agent *effectiveness* rather than server correctness — what an agent gets right on the first call versus gives up on: the latency budget per operation, what BM25 already gets right, seven ranked effectiveness gaps (no stemming, same-doc flooding, no frontmatter filtering, …), the capability gaps, and a closing resolution. Status: notes. |
| [`0003-roadmap-revision.md`](decisions/0003-roadmap-revision.md) | The post-validation roadmap: what moved in the field (agentic search over pipelines became consensus), then revised priorities, what was deprioritized, and what was kept as-is — the shift away from the dogfood tail toward corpus-level structure, honest negatives, and actually living with the tool. Status: adopted. |
| [`0004-effectiveness-harness.md`](decisions/0004-effectiveness-harness.md) | `lore eval` — the JSONL query-set format, the metrics (Success@k, MRR, coverage accuracy), the methodology, and the baseline numbers. Read this before changing anything on the retrieval path. Status: adopted. |
| [`0005-okf-alignment.md`](decisions/0005-okf-alignment.md) | Google's Open Knowledge Format: what it is, why it is an authoring format rather than a competing runtime, and the one slice Lore adopted — the `type`/`status`/`verified`/`stale_after` projection in [`../crates/lore-index/src/okf.rs`](../crates/lore-index/src/okf.rs). Status: adopted. |
