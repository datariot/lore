---
date: 2026-07-24
status: adopted
follows: 0003-roadmap-revision.md
---

# Measuring retrieval effectiveness

Until now every retrieval change (stemming, coverage, description weighting,
section backlinks, corpus_map) was justified by unit/integration tests
(*correctness*) and anecdotal live queries. Those prove the code does what it
says; they do not prove retrieval got *better*. `lore eval` closes that gap —
it's the quality counterpart to the Criterion latency bench.

## Method

A query set is JSONL, one labeled query per line:

```json
{"query": "kafka connect alerts", "relevant": ["docs/obs.md#Alarms"], "expected_coverage": "full"}
{"query": "helm chart rollout",   "relevant": [], "expected_coverage": "none"}
```

- **`relevant`** — acceptable answers as `rel_path` (any section of the
  document counts) or `rel_path#Heading > Sub` (that exact section). A query
  with no targets is an *out-of-domain probe*: it scores coverage only.
- **`expected_coverage`** — the KB-642 verdict under test (`full`/`partial`/`none`).

`lore eval -r <corpus> -q <set>` runs each query through the real ranker and
reports:

- **Success@1 / @3 / @10** — fraction of judged queries whose answer landed in
  the top k.
- **MRR** — mean reciprocal rank of the first relevant hit.
- **Coverage accuracy** — fraction of coverage-scored queries whose verdict
  matched ground truth.

Metric logic is pure (`run_eval`) and unit-tested; the CLI wraps it with
index-load + JSONL parsing.

## Two query sets

- **`eval/mini-kb.jsonl`** — deterministic, against the test fixture, gated in
  CI (`tests/eval_fixture.rs`) at Success@1 = 1.0, MRR = 1.0, coverage
  accuracy = 1.0. Small and fully understood: any regression in ranking or the
  coverage verdict fails the build. This is the guard.
- **`eval/knowledge-base.candidate.jsonl`** — a bootstrapped candidate set
  against the 1,136-doc vault, auto-filled with search's own top hits for a
  human (David) to correct into real ground truth. Not CI-stable (the vault
  drifts); it's the real-world signal.

## What the harness scores, by feature

- **Coverage (KB-642)** — the `none`/`partial`/`full` verdict is checked on
  every query. On the fixture it's 8/8. See the precision note below.
- **Stemming (KB-228)** — the alert/alarm recall gap from `0002` becomes a
  query whose ground-truth doc uses the opposite surface form.
- **Description weighting (KB-646)** — the `zephyrine` fixture query: the
  answer lives *only* in README's frontmatter description, and it ranks #1.

## Findings from the first run

1. **The harness caught a mislabel on its first run.** I labeled
   `"components parser indexer server"` as `full`, but it's `partial`:
   `parser/indexer/server` live in body bullet items, and only the
   first-sentence *summary* is indexed, not full body text. The `partial`
   verdict was right; my assumption was wrong. Ground truth corrected.
2. **Coverage precision is real, not lucky.** An out-of-domain probe
   `"quokka photosynthesis nonsense"` returned `partial`, not `none` — because
   "nonsense" stems to a word genuinely present in the vault. Coverage flagged
   *exactly* that one term of three (`matched: ["nonsens"]`,
   `unmatched: ["quokka", "photosynthesi"]`). That's the feature working: a
   true out-of-domain probe needs genuinely-absent vocabulary, which the vault
   set now uses.
3. **A real ranking miss surfaced.** `"vpc flow log reduction"` ranks
   `Calendar/Weekly/*` notes above the actual `vpcflow_task_reduction_analysis.md`.
   Flagged in the candidate set; once David sets the right answer, the harness
   will quantify the miss and any future fix.

## How to use it going forward

Before/after any retrieval change: `lore eval` on both sets, diff the numbers.
The fixture floor is the hard gate; the vault set is the judgment call. When a
legitimate change moves a metric, update the floor *and the commit message* —
the numbers are the running effectiveness log now.
