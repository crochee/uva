#!/usr/bin/env bash
# Comment-budget gate: the doc-comment lines sitting on non-`pub`
# items under `crates/synthia-harness/src` must stay at or under
# the integer in `scripts/harness-docs/baseline`.
#
# Why this exists: the crate's *public* docs are the docs.rs
# surface and are worth every line they spend. The comments on
# private items are a different thing — when they restate the
# signature or narrate the next three lines they are pure reading
# tax, and they make the 100-line / 4-nesting shape budget from
# `check-harness-shape` harder to locate a real violation in. This
# gate ratchets the private-item total down so a later round
# cannot quietly put it back.
#
# The classifier, in order:
#   1. a run of `///` lines (inner `//!` doc is module-level and
#      always public-facing, so it is not counted);
#   2. skip forward over attribute lines (`#[...]`), `impl` blocks
#      and closing braces to the declaration the run documents;
#   3. count the run against the private budget unless that
#      declaration starts with `pub`.
#
# Everything else (plain `//` comments, block comments, doc on
# `pub` items) is out of scope: this gate is about `///` on private
# items, which is the only category that grew without review.
#
# Exit 0 with an `OK:` line, or exit 1 with the per-file breakdown.

set -euo pipefail

cd "$(dirname "$0")/.."

SRC_ROOT="crates/synthia-harness/src"
BASELINE_FILE="scripts/harness-docs/baseline"

if [[ ! -f "$BASELINE_FILE" ]]; then
  echo "FAIL: $BASELINE_FILE not found — the comment budget has no source of truth" >&2
  exit 1
fi
baseline=$(awk '/^[0-9]+$/ { print; exit }' "$BASELINE_FILE")
if [[ -z "$baseline" ]]; then
  echo "FAIL: $BASELINE_FILE must contain a single integer" >&2
  exit 1
fi

if [[ ! -d "$SRC_ROOT" ]]; then
  echo "FAIL: $SRC_ROOT not found" >&2
  exit 1
fi

# Per-file private-item `///` line counts, highest first. A file's
# own number is the granularity a reviewer can act on, so print the
# breakdown rather than only the verdict.
#
# Two things this classifier must get right, both learned the hard
# way: gawk does not treat `\b` as a word boundary inside a regex
# constant (`/^pub\b/` silently matches nothing, which would charge
# every `pub` run to the private budget), so the visibility test
# is an explicit character class. And the declaration is read from
# the record that *ends* a doc run, not from a `getline` probe —
# a probe lands on the run's own second line for a multi-line doc
# comment and files the whole run against a fake declaration.
breakdown=$(
  find "$SRC_ROOT" -name '*.rs' -print0 |
    xargs -0 awk '
      function trim(s) { sub(/^[ \t]+/, "", s); sub(/[ \t]+$/, "", s); return s }
      function charge(n) { priv[FILENAME] += n; total += n }
      # A doc line: open the run on the first, extend on the rest.
      /^[ \t]*\/\/\// { inrun = 1; runlen++; next }
      # A run just ended: classify the declaration it documented.
      inrun {
        t = trim($0)
        # Attributes, `impl` blocks and bare braces are not the
        # declaration; keep looking at the next line.
        if (t == "" || t ~ /^#\[/ || t ~ /^impl[ \t{]/ || t ~ /^[])}]/ || t ~ /^[{([][ \t]*$/)
          next
        if (t !~ /^pub[ \t(]/) charge(runlen)
        inrun = 0; runlen = 0
        next
      }
      END {
        # A run that runs to end-of-file documents nothing we can
        # see; charge it rather than silently dropping it.
        if (inrun) { charge(runlen); inrun = 0 }
        for (f in priv) printf "%6d  %s\n", priv[f], f
      }
    ' | sort -rn
)

total=$(awk '{ sum += $1 } END { print sum + 0 }' <<< "$breakdown")

if [[ "$total" -gt "$baseline" ]]; then
  echo "FAIL: private-item doc comments in $SRC_ROOT total $total lines, over the $baseline budget"
  echo "Per-file (largest first):"
  printf '%s\n' "$breakdown" | awk '$1 > 0'
  echo
  echo "Delete the ones that restate the signature or narrate the next"
  echo "three lines; keep the ones recording a non-obvious constraint"
  echo "(why this borrow, why this channel, why sharing beats cloning)."
  echo "After a deliberate cleanup, lower $BASELINE_FILE to the new total."
  exit 1
fi

echo "OK: $total private-item doc lines in $SRC_ROOT, at or under the $baseline budget"