---
date: 2026-07-19
status: adopted
follows: 0002-effectiveness-notes.md
---

# Roadmap revision — post-validation priorities

Six months of field movement reviewed (2026-07-19), roadmap revised
accordingly. Short version: the core bet is validated, one competitor moved
onto our turf with an LLM-shaped answer, and the highest-value work is no
longer the dogfood tail — it's corpus-level structure, honest negatives,
and actually living with the tool.

## What moved in the field

- **Agentic search over pipelines is now consensus.** The story told across
  the industry is that Claude Code dropped its early local-vector-DB design
  because agentic search was simpler and better; 2026 SWE-bench leaders do
  not use vector retrieval over the target repo. Lore's three "no"s (no
  vectors, no LLM at query time, no web) held up.
- **PageIndex shipped "File System" (May 2026)**: tree search scaled from
  one document to millions, synthesizing *virtual nodes* where no folder
  structure exists — built per-query, with LLM reasoning at query time.
  They answered the corpus-structure question with inference; we should
  answer it with the structure authors already wrote.
- **ByteRover became a paper** (arXiv 2604.01599): importance scoring with
  recency decay and maturity tiers, plus *out-of-domain detection* — the
  retrieval layer explicitly says "this isn't in my knowledge" instead of
  returning plausible-but-weak hits. Zero infra, markdown on disk. Closest
  research to our roadmap.
- **BookRAG** (arXiv 2512.03413): academic validation of hierarchical
  structure-aware indexing; pairs the heading tree with an entity graph
  mapped to tree nodes — structurally our heading tree + wiki-link graph,
  built there by an LLM, here for free.
- **Progressive disclosure standardized**: SKILL.md (40+ platforms) and
  llms.txt (~10% domain adoption; IDE agents fetch it routinely). Lore's
  TOC→section surface already has this shape; we can also *emit* the
  standard artifact.

## Revised priorities

1. **Corpus-level tree** — extend the nested `roots[].children[]`
   projection one level up: folders → documents → headings in a single
   tree. Folder hierarchy is author-written structure, same argument as
   headings; no virtual-node synthesis, no LLM. Our answer to PageIndex
   File System.
2. **Out-of-domain / weak-match signal on `search`** — a `coverage`
   verdict computed from BM25 score distribution ("top hit far below this
   corpus's typical match"), so agents stop grinding on corpora that don't
   contain the answer. Honest negatives beat plausible junk.
3. **Daily-driver ops** — lore installed as a LaunchAgent serving the
   knowledge-base vault, registered user-scope in Claude Code. Every
   hypothesis (access counts, coverage, corpus tree) needs lived usage
   data; commit messages and decision docs feed the Substack piece.
4. **Recency-decayed, persisted access counts** — all-time in-memory
   counters go stale and die on restart; decay + a sidecar file (not the
   index — format stays stable) make `recent_hot` mean something.
5. **`lore export llms-txt`** — serialize the index as `llms.txt` /
   `llms-full.txt`. Rides the adopted standard; useful to agents that
   never speak MCP.

## Deprioritized

- **Phrase queries (KB-229)** — positional postings are a real index
  format change for a need BM25 + structure mostly covers; nothing in the
  research suggests adjacency matters for agent workflows. Parked.
- **Any embedding fallback tier** — semtools shows the tempting middle
  path (static embeddings, no infra). Porter stemming closed the observed
  synonym gap; the no-vector line holds until lived usage proves a gap
  stemming can't close.

## Kept as-is

- KB-233 (setext strict mode) — real Obsidian paper cut, waiting on
  daily-driver friction to justify itself.
- KB-640 (watch-test FSEvents flake) — test hygiene, independent of
  direction.
