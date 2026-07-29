# Lore

**Structure-aware markdown retrieval for AI agents.**

Lore is an MCP server that indexes a markdown corpus by its heading hierarchy and serves structured retrieval tools to agents over Streamable HTTP. No vector database. No LLM at retrieval time. No web dependency. Just markdown's own heading tree, used as the index it already is.

Think of it as a reference librarian who knows the table of contents for every document you point them at. Agents that don't speak MCP aren't left out — `lore export` emits a standard [`llms.txt`](https://llmstxt.org/) map of any corpus.

## Why

Every major coding agent solves documentation retrieval the same wrong way: grep and read. They treat markdown — which already has explicit structure — the same as flat source code. Lore takes the most direct path: your headings *are* the index.

| Approach | Who | Cost |
|---|---|---|
| Grep + read | Claude Code, Gemini CLI | Token-expensive iteration |
| Vector RAG | Cursor, MCP-Markdown-RAG | Embedding infra, chunking destroys structure |
| LLM-navigated trees | PageIndex | LLM inference per query |
| **Heading-tree index** | **Lore** | **None at query time** |

## Install

```bash
cargo install --path services/lore
```

Or build from source:

```bash
cargo build --release -p lore
./target/release/lore --help
```

## Quick start

```bash
# 1. Index a directory of markdown.
lore index /path/to/your/vault

# 2. Serve it over MCP Streamable HTTP.
lore serve -r /path/to/your/vault

# 3. Or combine — serve and watch for changes:
lore watch -r /path/to/your/vault

# 4. Measure retrieval quality against a labeled query set.
lore eval -r /path/to/your/vault -q eval/mini-kb.jsonl

# 5. Export an llms.txt map (add --full for llms-full.txt).
lore export -r /path/to/your/vault --out ./site
```

The MCP endpoint is at `http://127.0.0.1:7331/mcp` by default. Point any MCP-compatible client at it — for Claude Code:

```bash
claude mcp add --scope user --transport http lore http://127.0.0.1:7331/mcp
```

To run it continuously against a live vault, see [docs/daily-driver.md](docs/daily-driver.md).

> **Upgrading from a pre-stemming build?** The on-disk index format moved from `lore-index-v1` to `lore-index-v2` when Porter stemming landed. Old `.lore/index.json` files refuse to load with a clear message; re-run `lore index <root>` to rebuild.

> **Wire-format break (2026-07):** every tool response now names the heading-segment list `heading_path` (previously `path`), matching the request parameter of the same name, and `table_of_contents` returns a nested `roots[].children[]` tree instead of a flat `entries[]` array.

> **Index format v3 (2026-07):** the on-disk index now persists each document's modification time so search results can report `age_days`. v2 indexes are rejected with a clear message; re-run `lore index <root>` to rebuild.

## MCP tools

| Tool | What it does |
|---|---|
| `list_sources` | Every corpus Lore has loaded, with document and heading counts. |
| `table_of_contents` | Heading tree for a corpus or a single document, as nested `roots[].children[]` — the wire format is the tree. Supports `max_depth` and optional frontmatter. |
| `corpus_map` | Navigation map of a whole corpus: the folder hierarchy (from document paths) nested folders → documents, each with its title, description, and a heading preview. The orient-yourself call for a large source. `path_prefix` maps one subtree. |
| `get_section` | Retrieve a section by heading path or node id. O(1) byte-range slice via mmap. |
| `get_by_path` | Convenience form: `file.md#Heading > Subheading`. |
| `search` | BM25 ranking over titles, path segments, and summaries. Porter-stemmed (so `alarm` finds `alarms`). `-term` excludes. `group_by: "doc"` collapses same-document hits. Access-count boost. Returns a `coverage` verdict (`full`/`partial`/`none`) so an agent can tell "not in this corpus" from "no good match" and stop instead of retrying. Each hit reports `age_days`; pass `stale_after_days` for a per-hit `stale` boolean. OKF documents also carry `concept_type`, `status`, and `trust` on each hit. |
| `backlinks` | Every section that links *to* a target — precomputed at index time. `target_anchor` narrows to links aimed at one section (`[[Page#Heading]]`). |
| `recent_hot` | Top-N sections by a time-decayed access score (two-week half-life) — recent use outranks stale heavy use. Persisted to `.lore/access.json`, so it survives restarts and reindexes. Returns raw `access_count` and `decayed_score`. |
| `neighbors` | Parent, prev/next sibling, children of a node. Navigate one hop at a time. |
| `add_source` | Register a new directory as a corpus. |

## Architecture

```
lore/
├── crates/
│   ├── lore-core      Types: SourceId, NodeId, HeadingPath, ByteRange, Link.
│   ├── lore-parse     pulldown-cmark events + frontmatter + wiki-links + Dataview.
│   ├── lore-index     Heading tree, corpus-level indices, serialization.
│   ├── lore-search    BM25 ranker over title/path/summary/description with access boost.
│   └── lore-watch     Debounced notify-rs wrapper.
└── services/
    └── lore           Single binary: clap CLI + rmcp server over Streamable HTTP.
```

Library crates have **zero I/O**. The `lore` service owns the filesystem, HTTP transport, and on-disk index file.

## Performance

Measured on the author's personal knowledge-base: **985 markdown files, 14,126 headings**.

| Operation | Time |
|---|---|
| Full index build (cold) | 450 ms |
| Index file size (JSON) | 6.4 MB |
| `get_section` (mmap slice) | < 1 ms |
| BM25 search | 21 µs (multi-word) / 102 µs (common single term) |
| Incremental re-index on file change | ~200 ms (full rebuild of derived indices) |

Plan target was **<10 ms p99 for search at 15K headings** — the inverted index puts us two orders of magnitude under that.

## Obsidian

Lore handles Obsidian-flavoured markdown natively:

- **Frontmatter** is parsed as YAML and surfaced in `table_of_contents` when requested. A frontmatter `description` is treated as an author-written retrieval hook: it's indexed as a top-weight BM25 field on the document's root heading (so a doc is findable by words that appear only in its description) and returned on search hits and `list_documents`.
- **Wiki-links** (`[[Page]]`, `[[Page|alias]]`, `[[folder/Page#Heading]]`) are extracted and indexed. Backlinks match by basename, so `[[arch]]` and `[[docs/arch.md#Caching]]` both find the same target. `#Heading` fragments are resolved to their heading at index time, so backlinks can be queried at section granularity.
- **Dataview blocks** are tagged with `kind: "dataview"` on the owning heading so agents know they're query results, not prose.
- **Code-fenced wiki-links** are excluded — `[[example]]` inside a code block doesn't create a spurious link.

## Open Knowledge Format

Lore is a native consumer of Google Cloud's [Open Knowledge Format
(OKF)](https://github.com/GoogleCloudPlatform/knowledge-catalog/tree/main/okf) —
a vendor-neutral spec (markdown + YAML frontmatter) for packaging knowledge for
agents. OKF standardizes the *corpus*; Lore is the retrieval runtime the spec
leaves open. Point Lore at any OKF bundle and it just works — with no vectors,
the thing OKF's own consumption note assumes you'll reach for.

When a document carries OKF frontmatter, Lore surfaces it on results:

- **`type`** → `concept_type` on search hits, `list_documents`, `get_section`, and `corpus_map`.
- **`status`** (`draft`/`stable`/`deprecated`) → `status`, same places.
- **`verified`** → a derived `trust` tier (`human-reviewed` or `machine-confirmed`); absent when unverified.
- **`stale_after`** → when the author's expiry date has passed, hits report `stale: true` *without* any `stale_after_days` request — an explicit expiry beats an mtime guess.

See [docs/decisions/0005-okf-alignment.md](docs/decisions/0005-okf-alignment.md) for the full analysis.

## Local-first

Your corpus lives on your disk. Lore reads it, indexes it to `.lore/index.json` inside the corpus root, and serves queries from memory. No telemetry, no uploads, no external services.

## Design principles

- **Optimize for agents, not humans.** The UX target is the LLM calling the tool, not the human reading help text.
- **No LLM at retrieval time.** The agent *is* the LLM. Duplicating planning in the server wastes tokens.
- **No vectors.** Vectors recreate structure that authors already wrote. Reuse the structure instead.
- **Property-based tests for invariants, Criterion for latency.** Coverage follows the shape of likely bugs.
- **Library/service split.** Pure logic crates make property tests trivial to write.

## License

MIT or Apache-2.0, at your option.

## Contributing

Issues, PRs, and design critique welcome. This project is actively being built — see the [Linear project](https://linear.app/datariot/project/lore-a9e29923ed13) for roadmap.
