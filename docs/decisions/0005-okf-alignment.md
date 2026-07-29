---
date: 2026-07-29
status: adopted
follows: 0003-roadmap-revision.md
---

# Aligning with Google's Open Knowledge Format (OKF)

On 2026-06-12 Google Cloud published the [Open Knowledge Format
(OKF)](https://github.com/GoogleCloudPlatform/knowledge-catalog/tree/main/okf) —
a vendor-neutral spec for packaging organizational knowledge as a directory of
markdown files with YAML frontmatter, cross-linked with ordinary markdown
links, versioned in git. It is, almost line for line, the shape Lore already
consumes. This note records what OKF is, why it does *not* compete with Lore,
and the one slice we adopted from it.

## What OKF is

OKF v0.2 is an **authoring / interchange format**, not a runtime. Its entire
spec is conventions for how a producer *writes and packages* a knowledge
bundle:

- **Required:** `type` — a free string naming the concept kind. Not centrally
  registered; consumers must tolerate unknown values. This is the *only* hard
  requirement.
- **Recommended:** `title`, `description` (one sentence), `resource` (a URI to
  the underlying asset), `tags`.
- **Provenance (`sources`):** per-source `author`, `usage_count`,
  `last_modified`, footnote-keyed per-claim attribution.
- **Trust (`generated` / `verified`):** `generated: {by, at}` records who
  produced the content; `verified: [{by, at}]` records review events. Actors
  are written `human:<id>`, `<producer>/<version>`, or `process:<id>`. From the
  `verified` list a consumer derives a **trust tier**: no `verified` key →
  unverified; only non-human verifiers → machine-confirmed; any `human:` actor
  → human-reviewed.
- **Lifecycle:** `status` (`draft` | `stable` | `deprecated`) and `stale_after`
  (a `YYYY-MM-DD` date; the concept is stale once `today >= stale_after`).
- **Structure:** concept ID = file path minus `.md`; `index.md` gives
  progressive disclosure; `log.md` is an optional changelog. `Attested
  Computation` concepts carry runnable, verifiable SQL/dbt/python.

Consumption is **explicitly unspecified**. The spec says an agent can "parse
the files, index the frontmatter, follow the links, *potentially with
embeddings*." Google ships a static HTML visualizer as the reference consumer.
That is the whole of it.

## Why it doesn't compete with Lore

OKF and Lore sit at opposite ends of the same pipeline:

| | OKF | Lore |
|---|---|---|
| Concern | how you *write* a corpus (producer) | how you *serve* a corpus (consumer) |
| Artifact | a spec + a static viewer | an index + retrieval runtime |
| Retrieval | left as an exercise ("potentially with embeddings") | BM25 over the heading tree, no vectors |

Google standardized the fuel; Lore is the engine the spec deliberately leaves
open. Its own consumption note reaches for embeddings by default — Lore is the
standing counter-argument that the heading tree is enough.

This is the **second independent convergence** on Lore's thesis in a week: the
Claude Code memory system (see `0003`'s memory notes) and now an official
Google spec both landed on *markdown + frontmatter + cross-links, curated for
agents*. Nobody coordinated. That convergence is worth more than a benchmark.

## What we adopted: OKF-aware ingestion

OKF ratifies the frontmatter roadmap `0003` already set in motion —
`description` as a retrieval hook (shipped, KB-646), mtime staleness (shipped,
KB-647). It adds two signals worth surfacing, and one date rule worth
honoring:

1. **`type` and `status`** — surfaced as `concept_type` and `status` on
   search hits, `list_documents`, `get_section`, and `corpus_map` documents.
   Cheap projection over frontmatter already loaded; no new derived index.
2. **Trust tier** — derived from `verified` actors, surfaced as `trust`
   (`human-reviewed` | `machine-confirmed`; omitted when unverified). An agent
   can prefer a human-reviewed concept over a machine-generated one.
3. **Declared staleness beats inferred staleness.** Lore already computes a
   `stale` boolean from file mtime against a request threshold. When a document
   declares `stale_after` and that date has passed, the hit now reports
   `stale: true` *regardless of any request threshold* — an author who says
   "this expires on date X" is a stronger signal than "the file is N days old."

The payoff: **point Lore at any OKF bundle and it just works** — a far better
consumer than a static HTML viewer, and still with zero vectors.

## What we deliberately did not do

- **`Attested Computation`** — that is Google's BigQuery-verification agenda,
  orthogonal to retrieval. Out of scope.
- **Vectors** — the no-vector invariant (design invariant #1) holds. OKF's
  "potentially with embeddings" is the assumption Lore exists to refute.
- **Trust *ranking*** — surfacing `trust` is cheap; letting it move BM25 scores
  needs `verified` data to exist in a real vault first. Deferred until there is
  something to tune against (the same "needs data" tail as the access-decay
  work).

## Follow-ups (filed, not in this slice)

- `lore export --okf` — emit a conformant OKF bundle from a corpus, alongside
  the existing `llms.txt` export. Makes Lore a *producer* too, interoperable
  both directions.
- Resolve OKF `/`-absolute (bundle-relative) links in backlink resolution;
  today Lore resolves wiki-links by basename.
