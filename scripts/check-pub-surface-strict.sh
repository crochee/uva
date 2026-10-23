#!/usr/bin/env bash
# Strict paradigm-crate pub-surface gate: per-crate ratchet on the
# number of explicit `pub use ...` re-export lines at the root of each
# non-facade lib crate's `lib.rs`. Glob re-exports are already caught
# by `check-pub-surface`; this gate catches the subtler drift where
# each new public name gets listed individually and the curated
# surface grows until it's indistinguishable from a glob.
#
# Why this exists: AGENTS.md §3.7 says the lib.rs of each
# `synthia-*` crate is the single place that decides what is public.
# A count-only ratchet pins that decision so a future "let's also
# export this" doesn't sneak past review — the same way
# `check-plugin-shape` ratchets plugin-crate file length.
#
# Scope: same as `check-pub-surface` minus the facade — `crates/synthia/`
# is exempt (it propagates curated surfaces wholesale), `synthia-server`
# and `synthia-mcp-server` are app / binary crates and their lib.rs is
# a wiring entry, not a curated surface.
#
# Per-crate baselines live in `scripts/paradigm-surface/baselines/<crate>`.
# Each baseline file contains a single integer (the current `pub use`
# count for that crate). Adding a new public item to a crate's lib.rs
# goes in the same commit as the baseline bump.
#
# Exit 0 with an `OK:` line, or exit 1 with the offending crates.

set -euo pipefail

cd "$(dirname "$0")/.."

# Crates whose lib.rs participates in the ratchet. Kept explicit so
# a new crate's lib.rs doesn't silently get budgeted at 0.
PARADIGM_CRATES=(
  crates/synthia-tool
  crates/synthia-harness
  crates/synthia-provider
  crates/synthia-core
  crates/synthia-context
  crates/synthia-steering
  crates/synthia-session
)

baseline_dir="scripts/paradigm-surface/baselines"
if [[ ! -d "$baseline_dir" ]]; then
  echo "FAIL: $baseline_dir/ not found — the per-crate budgets have no source of truth" >&2
  exit 1
fi

over=()
checked=0
for crate in "${PARADIGM_CRATES[@]}"; do
  lib_rs="$crate/src/lib.rs"
  if [[ ! -f "$lib_rs" ]]; then
    continue
  fi
  baseline_file="$baseline_dir/$crate"
  if [[ ! -f "$baseline_file" ]]; then
    echo "FAIL: $baseline_file not found — every paradigm crate needs a baseline file" >&2
    exit 1
  fi
  baseline=$(awk '/^[0-9]+$/ { print; exit }' "$baseline_file")
  if [[ -z "$baseline" ]]; then
    echo "FAIL: $baseline_file must contain a single integer line count" >&2
    exit 1
  fi
  current=$(grep -cE '^[[:space:]]*pub use ' "$lib_rs" 2>/dev/null || echo 0)
  checked=$((checked + 1))
  if [[ "$current" -gt "$baseline" ]]; then
    over+=("$current (was $baseline): $lib_rs  -> baseline: $baseline_file")
  fi
done

if [[ "$checked" -eq 0 ]]; then
  echo "FAIL: no paradigm-crate lib.rs files found under the configured roots" >&2
  exit 1
fi

if [[ ${#over[@]} -gt 0 ]]; then
  echo "FAIL: paradigm crate(s) grew past their per-crate pub-use budget"
  printf '  %s\n' "${over[@]}"
  echo "Either remove the new pub use or bump the baseline in the matching"
  echo "scripts/paradigm-surface/baselines/crates/<crate> file in the same"
  echo "commit (mirrors check-plugin-shape)."
  exit 1
fi

echo "OK: all $checked paradigm crates are at or under their per-crate pub-use baselines"
