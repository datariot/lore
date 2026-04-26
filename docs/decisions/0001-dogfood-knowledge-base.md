---
date: 2026-04-26
status: notes
corpus: knowledge-base (Obsidian Faceted Entity Graph vault)
---

# Dogfooding against a real Obsidian vault

First end-to-end run against `~/Workspace/knowledge-base` (1043 .md files on
disk, 985 indexed, 14,126 heading nodes, 6.4 MB index, 24 MB corpus). All
nine MCP tools exercised over Streamable HTTP from `curl`.

## Numbers (release build, M-series Mac, cold cache)

- Walk + parse + index + serialize: **278 ms build, 8 ms write** (~290 ms total)
- `search "kafka"`: ~14 ms wall (server side: well under 1 ms BM25)
- `table_of_contents` for one document: ~10 ms
- `get_section` by heading_path: ~13 ms
- `neighbors` and `get_by_path`: <10 ms
- Index size: 6.4 MB JSON for 14,126 nodes (≈ 460 B/node)

These are end-to-end numbers including curl + JSON-RPC framing + SSE event
parsing. The actual Lore work is small enough that the network and shell are
the dominant cost.

## What worked

- **Gitignore-aware walker skipped exactly the right files.** 1043 .md files
  on disk minus 58 in `.claude/`, `.backup/`, plus the gitignored `CLAUDE.md`
  → 985 indexed. Confirmed by diffing `find` against the index's
  `corpus.documents[].rel_path` set. Backups and command templates would
  have been pure noise to an agent.
- **`recent_hot` tracking works.** The single section we read with
  `get_section` showed `access_count: 1` immediately. Usage signal is live.
- **`get_by_path` qualified-path syntax (`file.md#H1 > H2`) is ergonomic.**
  Returned the right section in one hop without an intermediate TOC call.
- **Wire protocol is honest MCP Streamable HTTP.** Initialize → session id →
  notifications/initialized → tools/list → tools/call all worked from raw
  curl. No `rmcp` client required.

## Friction points (in priority order)

### 1. Field-name inconsistency between input and output schemas

Output of `search`/`backlinks`/`recent_hot` returns `path` (the heading
path). But `get_section`, `neighbors`, and `table_of_contents` take
`heading_path` as input. An agent that pipes search results into
`get_section` has to rename the field. Pick one — `heading_path`
everywhere is clearer than the bare `path`, which collides conceptually
with `rel_path`.

### 2. TOC wire format is flat, but README and CLAUDE.md call it a "heading tree"

`table_of_contents` returns `documents[].entries[]` — a flat list with
`level` and `path`. The agent has to reconstruct the tree from `level`
runs. Either:
- Rename to `outline` / `headings` and update prose, or
- Actually return nested children (the on-disk `DocumentIndex` already has
  this shape — `roots` + `children` per node).

Flat-with-levels is fine for a UI render, but the project explicitly
positions itself as "the structure IS the index." Tree-shaped output
would honor that.

### 3. Phantom setext headings from Obsidian daily-note templates

Found in the wild — daily notes use this pattern:

```markdown
## 🚧 Progress Made

**[[01_Projects/Klaviyo/Klaviyo|Klaviyo]]:**
-

**[[01_Projects/Bloom & Grow Farm/Bloom & Grow Farm|Bloom & Grow Farm]]:**
-
```

The bare `-` on its own line is parsed as a **CommonMark setext H2
underline**, promoting the bold-text line above to a level-2 heading.
Lore picks this up faithfully — pulldown-cmark is correct — but the
result is dozens of headings titled `[[01_Projects/.../Klaviyo|Klaviyo]]:`
in the TOC.

Possibilities:
- **Document it** as a corpus-quality gotcha. Cheap.
- **Heuristic:** treat `-` as a setext underline only if the previous
  line is *not* an "Obsidian-style empty bullet" pattern (i.e., the
  previous heading-section already contains list items). Slippery.
- **Strict mode:** require setext underlines to be ≥ 2 chars. CommonMark
  technically allows `-` alone, but a 2-char minimum would resolve almost
  every real-world false positive. This is a one-line patch in a
  pre-pass.

I'd start with documenting and a strict-mode flag (`--no-setext-empty`).
The corpus author should fix templates, but Lore can be charitable.

### 4. Wiki-link backlink keys aren't canonicalized

`backlinks target="Klaviyo"` and `backlinks target="01_Projects/Klaviyo/Klaviyo"`
both return 5 different hits, against the *same* destination file.
Agents can't reliably ask "what links here?" — they have to know which
form the source files use.

Fix: resolve every wiki-link target to its canonical file path at index
time, and key the inverted backlink table on canonical path. A
display-time `target_text` field can preserve the original form for the
agent's benefit.

### 5. Search summary lacks a heading→body separator

```
"summary": "OverviewThis document outlines the key metrics..."
```

The first child heading title (`Overview`) is concatenated with its body
without a delimiter, so the agent reads `OverviewThis...`. Insert `". "`
or `"\n\n"` between heading and body when building the summary.

### 6. `path` field on backlinks is the *containing section*, not the link site

For an agent that wants to read the actual sentence containing a wiki-link,
the response says "this section contains a link to Klaviyo" but doesn't
include the byte offset of the link itself. May not matter — the agent can
re-search the section text — but worth noting if we ever expose
"link-aware excerpts."

## Substack-relevant

- The "no LLM at retrieval time" claim held: every tool returned in <15 ms.
  Compare with PageIndex's per-query LLM tree-walks.
- The structure-aware index successfully resisted the temptation to
  "improve" the heading tree with embeddings or summarization. The TOC of
  a 985-doc Obsidian vault is just *there*, no model required.
- The setext footgun is a great anecdote: "structure-aware retrieval is
  only as honest as the structure the author wrote." It's the dual of the
  RAG-chunking critique — chunks lie because they ignore structure;
  Lore's failure mode is the opposite, faithfully reflecting structural
  ambiguity that the rendering tool (Obsidian) was hiding.
