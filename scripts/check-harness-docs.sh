#!/usr/bin/env bash
# Comment-budget gate: the doc-comment lines sitting on non-`pub`
# items under `crates/elysper-harness/src` must stay at or under
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
# A `pub trait` body is public surface even though its members are
# declared without their own visibility (`fn foo(..)`), so runs
# inside one are not charged: while the brace depth sits inside a
# `pub [unsafe] trait` body nothing is charged, and the normal rule
# resumes once the depth returns to the body's opening level. A
# non-`pub` trait still charges — its members are private.
#
# Everything else (plain `//` comments, block comments, doc on
# `pub` items) is out of scope: this gate is about `///` on private
# items, which is the only category that grew without review.
#
# Exit 0 with an `OK:` line, or exit 1 with the per-file breakdown.

set -euo pipefail

cd "$(dirname "$0")/.."

SRC_ROOT="crates/elysper-harness/src"
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
# Three things this classifier must get right, all learned the hard
# way: gawk does not treat `\b` as a word boundary inside a regex
# constant (`/^pub\b/` silently matches nothing, which would charge
# every `pub` run to the private budget), so the visibility test
# is an explicit character class. The declaration is read from
# the record that *ends* a doc run, not from a `getline` probe —
# a probe lands on the run's own second line for a multi-line doc
# comment and files the whole run against a fake declaration. And
# the trait-body exemption rides on brace depth, so the counter
# must close to zero in every file — a file where it does not
# (a raw string full of braces, say) fails the gate here instead
# of silently mis-charging every run after the drift.
if ! breakdown=$(
  find "$SRC_ROOT" -name '*.rs' -print0 |
    xargs -0 awk '
      function trim(s) { sub(/^[ \t]+/, "", s); sub(/[ \t]+$/, "", s); return s }
      function charge(n) { priv[charged_to] += n; total += n }
      function opens(s,  copy, n) { copy = s; n = gsub(/{/, "", copy); return n }
      function closes(s,  copy, n) { copy = s; n = gsub(/}/, "", copy); return n }
      FNR == 1 {
        # Files start fresh: depth is per-file, and a run the
        # previous file left open is charged to that file, not
        # filed against this one s first declaration.
        if (inrun) charge(runlen)
        if (depth != 0) {
          printf "FAIL: brace depth ended at %d in %s; the trait-body accounting cannot be trusted\n", depth, charged_to > "/dev/stderr"
          bad_depth = 1
        }
        inrun = 0; runlen = 0; depth = 0
        in_pub_trait = 0; trait_open = 0
        charged_to = FILENAME
      }
      # A doc line: open the run on the first, extend on the rest —
      # unless the run sits inside a `pub trait` body, whose members
      # are public surface documented for docs.rs.
      /^[ \t]*\/\/\// {
        if (!in_pub_trait) { inrun = 1; runlen++ }
        next
      }
      {
        t = trim($0)
        o = opens($0); c = closes($0)
        if (!in_pub_trait) {
          # A run just ended: classify the declaration it documented.
          if (inrun) {
            # Attributes, `impl` blocks and bare braces are not the
            # declaration; keep looking at the next line.
            if (t == "" || t ~ /^#\[/ || t ~ /^impl[ \t{]/ || t ~ /^[])}]/ || t ~ /^[{([][ \t]*$/) {
              # not the declaration; fall through to the accounting
            } else {
              if (t !~ /^pub[ \t(]/) charge(runlen)
              inrun = 0; runlen = 0
            }
          }
          # Entering a `pub [unsafe] trait` body exempts everything
          # in it until the depth returns to the body s opening
          # level. The brace may sit on the declaration line or
          # after a where clause; either way `body` is the depth of
          # the body once open, and `pub trait Foo {}` (an empty
          # body on one line) disarms itself immediately.
          if (t ~ /^pub([ \t]+unsafe)?[ \t]+trait([^[:alnum:]_]|$)/) {
            in_pub_trait = 1; trait_open = 0
            if (o > 0) {
              trait_open = 1
              body = depth + o
              if (depth + o - c < body) in_pub_trait = 0
            }
          }
        } else if (!trait_open && o > 0) {
          trait_open = 1
          body = depth + o
          if (depth + o - c < body) in_pub_trait = 0
        }
        depth += o - c
        if (in_pub_trait && trait_open && depth < body) in_pub_trait = 0
        next
      }
      END {
        # A run that runs to end-of-file documents nothing we can
        # see; charge it rather than silently dropping it.
        if (inrun) charge(runlen)
        if (depth != 0) {
          printf "FAIL: brace depth ended at %d in %s; the trait-body accounting cannot be trusted\n", depth, charged_to > "/dev/stderr"
          bad_depth = 1
        }
        if (bad_depth) exit 1
        for (f in priv) printf "%6d  %s\n", priv[f], f
      }
    ' | sort -rn
); then
  echo "FAIL: the private-doc classifier could not account for every file under $SRC_ROOT (see the lines above)" >&2
  exit 1
fi

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