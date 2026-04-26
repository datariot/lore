---
date: 2026-04-26
status: notes
follows: 0001-dogfood-knowledge-base.md
---

# Lore effectiveness — what works, what to improve

Second pass on the same `knowledge-base` corpus (985 docs, 14k nodes).
Focus: agent effectiveness, not server correctness. What does an agent get
right on the first call vs. give up on?

## Latency budget

| Operation | Wall (curl + SSE) |
|---|---|
| `search "kafka connect dashboard"` | ~14 ms |
| `table_of_contents` (one doc) | ~10 ms |
| `get_section` by node id | ~13 ms |
| `neighbors` | <10 ms |
| `backlinks` | <10 ms |
| **Full 5-call agent workflow** | **38 ms** |
| Corpus-wide TOC depth 1 (985 docs → 862 entries) | ~16 ms |

These include curl + JSON-RPC framing. Real MCP clients with kept-alive
connections will be faster. Agents pay ~10 ms per hop, which means a
plan-and-fetch workflow can do dozens of hops inside a single second.

## What BM25 gets right

- **Multi-word phrase ranking is strong.** "kafka connect dashboard" puts
  triple-match hits cleanly above doubles. "managed streaming kafka"
  scored 52 vs 32 for the next hit — full-phrase coverage rises sharply.
- **Long natural-language queries work.** "what are the critical alarms
  for kafka connect workers" → top hit scored 79, perfectly relevant.
  Agents do not need to learn a query DSL; whole questions work.
- **Three-field weighting (`title 3.0 / path 2.0 / summary 1.0`) is
  sensible.** Nodes whose path *segments* match the query (e.g.,
  "Validation Checklist > Alerts" for query "alerts") rank high even
  when the term isn't a leaf heading.
- **Body content IS searchable, via the per-node `summary` field.**
  CLAUDE.md misstates this — it claims "titles and path segments only."
  The summary is the first sentence (≤ 240 chars) of body text. Found
  "MLOps" in `Atlas/Home.md > Facets` whose body says "Teaching data
  science and MLOps online" — heading text alone wouldn't have matched.

## Effectiveness gaps (priority order)

### 1. No stemming or synonym handling

Most painful gap in the real corpus. The vault uses both "alarms" and
"alerts" interchangeably across ~50 docs:

- `search "alerts"` → 0 hits in `02-kafka-connect-observability.md`
  (which has 5 sections about alarms).
- `search "alarms"` → 0 hits in `09-critical-missing-alerts.md` (the
  *file is literally named "alerts"*).

The agent has to guess which term the author used or run both queries
and merge. A 3-line stem table for the most common doubles
(`alert/alarm`, `metric/measurement`, `deploy/deployment/deployed`)
would buy a lot. Full Porter stemming is overkill but tempting.

### 2. Query side of backlinks doesn't canonicalize

`canonical_link_keys` in `corpus.rs:295` produces multiple keys per
*indexed* link. But `backlinks` in `services/lore/src/mcp/server.rs:213`
just lowercases `req.target` and reads one key:

```rust
let key = req.target.to_lowercase();
let hits = corpus.backlinks.get(&key).cloned().unwrap_or_default();
```

Result asymmetry on the same logical target:
- `target: "Klaviyo"` → 50 hits / 40 unique files (basename key catches both forms)
- `target: "01_Projects/Klaviyo/Klaviyo"` → 45 hits / 36 unique files (only path-form links)

Fix: run `canonical_link_keys(&req.target)` on the query, union postings
across all keys, dedupe by `(doc_id, node_id)`.

### 3. Same-doc result flooding for narrow queries

`search "Andrew"` → 5/5 hits all from `Entities/People/Andrew Bialecki.md`
(root + 4 sub-sections). The agent has to dedupe by `rel_path` and pick
its preferred granularity.

Two options:
- Add a `group_by: "doc"` flag that returns the top-scoring section per
  document, with secondary sections nested under it.
- Add a per-doc score penalty so a narrow corpus produces a more diverse
  result set.

### 4. No corpus-wide frontmatter filtering

To find "all docs with `type: moc`" or "all projects where
`status: active`", an agent must:
1. Call `table_of_contents` with `include_frontmatter: true` and no
   `rel_path` (returns every document's frontmatter).
2. Filter client-side.

For 985 docs that's a 16 ms call returning ~860 entries. Fine.
At 50K docs it would not be fine. A `list_documents(filters)` tool with
indexed frontmatter would scale better and read more naturally to an
agent.

### 5. No folder-prefix filter for navigation

Same shape: to list "all Atlas/Facets/* docs," the agent fetches the
entire corpus TOC and filters by `rel_path.startswith()`. A
`path_prefix` parameter on `table_of_contents` would let the agent
ask "what's under Atlas/Facets?" in a server-bounded query.

### 6. Heading→body concatenation in summaries (real bug)

Found in `crates/lore-parse/src/summary.rs:60` — `flatten_text` doesn't
emit whitespace on `Event::End(TagEnd::Heading(_))`. Result: a parent
node's summary reads "OverviewThis document outlines..." (child heading
title concatenated with its body, no separator).

One-line fix:

```rust
Event::End(TagEnd::Heading(_)) => {
    if !out.ends_with(' ') {
        out.push(' ');
    }
}
```

### 7. Tokenizer drops anything shorter than 2 chars

`k`, `c`, `R` → 0 hits. `ML` (2 chars) is on the boundary, would tokenize.
Defensible — single letters are noise — but worth a one-line note in
the search tool description so agents don't waste a turn.

## What an agent can't do today (capability gaps)

- **Search by frontmatter values** (project status, doc type, tags).
- **List documents in a folder** without pulling the whole corpus.
- **Find sections that link to a *section* of another doc** (only
  per-doc backlinks; `[[Page#Heading]]` fragments are stored but not
  separately queryable as far as I saw).
- **Phrase queries.** Quoted `"high priority"` is just split into two
  BM25 terms; there's no way to require adjacency.
- **Negation.** No `kafka -lambda` style filtering.

## Substack hooks

- The 38 ms 5-call workflow is the headline number. PageIndex burns
  multiple seconds per query because it does LLM tree-walks.
- The `alerts/alarms` failure is a great honest counter-example to
  "structure is enough." It isn't, *quite*. The corpus author's word
  choices leak through. A tiny stem table closes the gap without
  sliding into vector RAG.
- The asymmetric backlinks bug is a microcosm of the larger thesis:
  small canonicalization wins are worth a lot. ByteRover's hot-path
  retrieval banks on the same idea.

## Resolution

Same-session follow-up commits closed gaps #2 and #6 and added
three of the five capabilities listed above. Kept to general
markdown / Obsidian conventions — no corpus-specific tables.

| Item | Status | Commit |
|---|---|---|
| #6 Heading→body summary concat | fixed | `21e8aaf` |
| #2 Backlinks query canonicalization | fixed | `21e8aaf` |
| Capability: list documents in a folder | added (`path_prefix` on TOC) | `b820d0f` |
| Capability: search by frontmatter | added (new `list_documents` tool) | `b820d0f` |
| Capability: search negation | added (`-term` syntax) | `7eeb305` |
| #7 Tokenizer min-length note | done (search tool description) | `7eeb305` |

Still open:

- **#1 Stemming/synonyms** (`alert/alarm`). The hand-table version
  would over-fit. Porter stemming is a separate session's work.
- **#3 Same-doc result flooding** (`group_by: "doc"` knob).
- **#4 / #5 partially closed** by `list_documents` + `path_prefix`,
  but a true frontmatter index for large corpora is still future work.
- **Phrase queries** — needs positional postings (index format change).
- **Section-anchor backlinks** — `[[Page#Heading]]` granularity needs
  link-parse changes.
