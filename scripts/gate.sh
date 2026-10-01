#!/usr/bin/env bash
# The merge gate the escapement kernel runs for lore (escapement ADR-005):
# the same checks as .github/workflows/ci.yml, in one place, so a kernel
# ship and CI agree on what GREEN means. Exit 0 is GREEN; anything else
# FAILED. A phase argument, if given, is ignored: lore's gate is one pass.
set -euo pipefail
cd "$(dirname "$0")/.."

stage() { echo "==> gate: $1"; }

stage fmt
cargo fmt --all --check

stage clippy
cargo clippy --workspace --all-targets -- -D warnings

stage test
# Includes the retrieval-effectiveness fixture (tests/eval_fixture.rs),
# gated at 1.0 on eval/mini-kb.jsonl.
cargo test --workspace --all-targets

stage bench-compile
cargo bench --workspace --no-run

echo "==> gate: GREEN"
