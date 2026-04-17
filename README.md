# Lore

**Structure-aware markdown retrieval for AI agents.**

Lore is an MCP server that indexes a markdown corpus by its heading hierarchy and serves structured retrieval tools to agents over Streamable HTTP. No vector database. No LLM at retrieval time. No web dependency. Just markdown's own heading tree, used as the index it already is.

Think of it as a reference librarian who knows the table of contents for every document you point them at.

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
```

The MCP endpoint is at `http://127.0.0.1:7331/mcp` by default. Point any MCP-compatible client at it.

## MCP tools

| Tool | What it does |
|---|---|
| `list_sources` | Every corpus Lore has loaded, with document and heading counts. |
| `table_of_contents` | Heading tree for a corpus or a single document. Supports `max_depth` and optional frontmatter. |
| `get_section` | Retrieve a section by heading path or node id. O(1) byte-range slice via mmap. |
| `get_by_path` | Convenience form: `file.md#Heading > Subheading`. |
| `search` | BM25 ranking over titles, path segments, and summaries. Access-count boost. |
| `backlinks` | Every section that links *to* a target — precomputed at index time. |
| `recent_hot` | Top-N sections by access count. Agent usage as the importance signal. |
| `neighbors` | Parent, prev/next sibling, children of a node. Navigate one hop at a time. |
| `add_source` | Register a new directory as a corpus. |

## Architecture

```
lore/
├── crates/
│   ├── lore-core      Types: SourceId, NodeId, HeadingPath, ByteRange, Link.
│   ├── lore-parse     pulldown-cmark events + frontmatter + wiki-links + Dataview.
│   ├── lore-index     Heading tree, corpus-level indices, serialization.
│   ├── lore-search    BM25 ranker over title/path/summary with access boost.
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

- **Frontmatter** is parsed as YAML and surfaced in `table_of_contents` when requested.
- **Wiki-links** (`[[Page]]`, `[[Page|alias]]`, `[[folder/Page#Heading]]`) are extracted and indexed. Backlinks match by basename, so `[[arch]]` and `[[docs/arch.md#Caching]]` both find the same target.
- **Dataview blocks** are tagged with `kind: "dataview"` on the owning heading so agents know they're query results, not prose.
- **Code-fenced wiki-links** are excluded — `[[example]]` inside a code block doesn't create a spurious link.

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
