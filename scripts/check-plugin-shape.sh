#!/usr/bin/env bash
# Plugin-shape gate: no plugin tool crate's production source file may
# grow past the line budget in `scripts/plugin-shape/limit`. The
# baseline is the current max line count across the same file set,
# captured at the time this gate was introduced; the gate fails the
# moment any single file grows past `baseline`, so the budget ratchets
# only on intentional cleanup, never on drift.
#
# Why this exists: `check-harness-shape` already pins the
# harness crate's function length / nesting budget; plugin crates
# (`elysper-tool-plugins` / `-policy-plugins` / `-plugin-runtime` plus
# `elysper-mcp` / `-skill` / `-attachment`) ship one plugin per module
# directory, and the same long-file drift that motivated the harness
# budget shows up there too. The gate is intentionally
# file-grained (not function-grained): plugins are small and the
# natural remediation is splitting the file, not extracting helpers
# inline.
#
# Scoped to plugin crates on purpose: `elysper-harness` is governed
# by `check-harness-shape`; `elysper-app-server` and `elysper-mcp-server`
# are app / binary crates and their file layout is deployment-driven.
#
# Exit 0 with an `OK:` line, or exit 1 with the offending files.

set -euo pipefail

cd "$(dirname "$0")/.."

# Plugin-crate roots whose production source files participate in
# the gate. Each root is the leaf crate directory under `crates/`.
PLUGIN_CRATE_ROOTS=(
  crates/elysper-tool-plugins
  crates/elysper-policy-plugins
  crates/elysper-plugin-runtime
  crates/elysper-mcp
  crates/elysper-skill
  crates/elysper-attachment
)

# Baseline cap: highest current line count across the scanned file
# set. Update via `make plugin-shape-audit` (re-emits the right
# number after a deliberate split); the gate fails on any growth.
BASELINE_FILE="scripts/plugin-shape/baseline"
if [[ ! -f "$BASELINE_FILE" ]]; then
  echo "FAIL: $BASELINE_FILE not found — the plugin-shape budget has no source of truth" >&2
  exit 1
fi
baseline=$(awk '/^[0-9]+$/ { print; exit }' "$BASELINE_FILE")
if [[ -z "$baseline" ]]; then
  echo "FAIL: $BASELINE_FILE must contain a single integer line count" >&2
  exit 1
fi

# Walk each plugin crate's `src/` directory **recursively**: a plugin
# family keeps one plugin per module directory (`src/<plugin>/*.rs`),
# so a depth-1 walk would scan nothing and the gate would pass
# vacuously. Exclude `tests.rs` (inline tests; the `check-test-layout`
# gate owns that) and the sibling `tests/` integration-test dirs
# (layout gate covers those).
files=()
for root in "${PLUGIN_CRATE_ROOTS[@]}"; do
  [[ -d "$root/src" ]] || continue
  while IFS= read -r -d '' f; do
    files+=("$f")
  done < <(find "$root/src" -name '*.rs' ! -name 'tests.rs' -print0)
done

if [[ ${#files[@]} -eq 0 ]]; then
  echo "FAIL: no plugin-crate source files found under the configured roots" >&2
  exit 1
fi

over=()
for f in "${files[@]}"; do
  lines=$(wc -l < "$f")
  if [[ "$lines" -gt "$baseline" ]]; then
    over+=("$lines $f")
  fi
done

if [[ ${#over[@]} -gt 0 ]]; then
  echo "FAIL: plugin-crate production file(s) grew past the $baseline-line budget"
  printf '  %s\n' "${over[@]}"
  echo "Split the offending file (the lib.rs + focused siblings pattern is the"
  echo "intended shape). After the split, update $BASELINE_FILE to the new max"
  echo "and commit it as part of the same change."
  exit 1
fi

echo "OK: all ${#files[@]} plugin-crate production files are at or under $baseline lines (baseline)"
