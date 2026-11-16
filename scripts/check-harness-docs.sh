#!/usr/bin/env bash
# Comment-budget gate: the doc-comment lines sitting on non-`pub`
# items under `crates/uva-harness/src` must stay at or under
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
# The attribute skipper counts brackets, and a bracket inside a
# string is not a bracket. `#[doc = "the [start, end) range"]` opens
# one bracket and closes one, but a counter that reads the line as
# text sees two opens and one close, decides the attribute is still
# open, swallows the declaration under it as an "attribute
# continuation", and files a *public* item's doc against the private
# budget — the exact mis-charge this gate exists to kill. So the
# bracket count is taken over the characters of the line that are
# outside string literals, char literals and line comments; the
# scanner is the one `check-frozen-api.sh` already uses for its own
# extractor, so the repo has one idiom, not three.
#
# The brace count that decides where a `pub trait` body ends is
# taken over the same characters, for the same reason: a `{` inside a
# string is text, not a brace. A doc comment that shows a `{` in an
# example used to move the depth, and the file that followed no
# longer closed — the gate failed with a confusing "brace depth
# ended at N", and a `{` in a string could just as well hide a real
# imbalance. One scan per line yields the opens, the closes and the
# bracket balance together, so the shared `in_string` state advances
# once per line; the opens and the closes stay separate counts,
# because "this line opens a brace" is what the empty `pub trait Foo
# {}` body case turns on.
#
# Block comments are text too, and they nest: the scanner keeps a
# depth counter, so an inner `*/` does not close an outer comment and
# a `{` between them never moves the depth. The line comment is
# checked first, so a `/*` after a `//` on the same line opens
# nothing.
#
# `--self-test` runs the classifier's own fixtures — a multi-line
# `#[derive(...)]` before a `pub` item and before a private one, a
# plain private item, the string-literal bracket case, the
# string-literal / feature-name / line-comment brace cases, and the
# block-comment cases (a brace inside a block comment, a nested block
# comment, a `/*` inside a line comment) — so the gate behaviour is
# pinned by a test, not by a reviewer's memory of it.
#
# Exit 0 with an `OK:` line, or exit 1 with the per-file breakdown.

set -euo pipefail

cd "$(dirname "$0")/.."

SRC_ROOT="crates/uva-harness/src"
BASELINE_FILE="scripts/harness-docs/baseline"

# The classifier, as one awk program. It lives in a variable so the
# production scan and the `--self-test` fixtures run the *same*
# program: a fixture that passes because it runs different code is
# not a pin.
CLASSIFIER_AWK=$(cat <<'AWK'
function trim(s) { sub(/^[ \t]+/, "", s); sub(/[ \t]+$/, "", s); return s }
function charge(n) { priv[charged_to] += n; total += n }
# One scan of one line: the braces it opens (`o`), the braces it
# closes (`c`) and its bracket balance (`b`) — the `[` it opens minus
# the `]` it closes — counting only the characters that are *code*.
#
# A `{` inside a string literal is text, not a brace, and so is a `{`
# inside a char literal, a line comment or a block comment. That
# matters as much for the braces as it does for the brackets: the
# trait-body exemption rides on the brace depth, so a `{` in a doc
# comment used to move the depth and the accounting after it went
# wrong — silently, because a doc comment is exactly where an
# example with a brace lives.
#
# The scan is per character, with an `in_string` flag and a
# `block_depth` counter that survive across lines so a string or a
# block comment that spans lines keeps its state: the same idiom
# `check-frozen-api.sh` uses for its own extractor, so the repo has
# one scanner, not three.
#
# `block_depth` is a COUNTER and not a flag, because Rust block
# comments nest: `/* outer /* inner */ still outer */` is one
# comment, and only the last `*/` closes it. A flag would end the
# comment at the inner `*/` and read the rest as code.
#
# The order of the two comment forms matters: a `//` inside a block
# comment is comment text, and a `/*` inside a line comment opens
# nothing. The line comment is therefore checked first and ends the
# line before a later `/*` on the same line is ever seen.
#
# ONE scan per line, not two: the flag and the counter are shared
# state, so scanning the same line twice would advance them twice
# and corrupt every line after it. The scan therefore yields all
# three counts at once, and the attribute logic reads the bracket
# balance out of it.
#
# `o` and `c` stay SEPARATE counts, not a net delta: `o > 0` means
# "this line opens a brace", and the `pub trait Foo {}` case — an
# empty body on one line, which must disarm itself immediately —
# depends on that reading.
function scan(s,  i, n, ch, nxt, j) {
  o = 0; c = 0; b = 0
  n = length(s)
  for (i = 1; i <= n; i++) {
    ch = substr(s, i, 1)
    # Inside a block comment every character is text: a brace, a
    # bracket or a quote in it is not code.
    if (block_depth > 0) {
      if (ch == "/" && substr(s, i + 1, 1) == "*") {
        block_depth++
        i++
      } else if (ch == "*" && substr(s, i + 1, 1) == "/") {
        block_depth--
        i++
      }
      continue
    }
    if (in_string) {
      if (ch == "\\") i++
      else if (ch == "\"") in_string = 0
      continue
    }
    if (ch == "\"") { in_string = 1; continue }
    if (ch == "/") {
      # A line comment: the rest of the line is not code, and the
      # `/*` further along it opens nothing.
      if (substr(s, i + 1, 1) == "/") break
      # A block comment: everything up to its closing `*/` is not
      # code, and the comment may nest.
      if (substr(s, i + 1, 1) == "*") {
        block_depth = 1
        i++
      }
      continue
    }
    if (ch == "'") {
      nxt = substr(s, i + 1, 1)
      if (nxt == "\\") {
        # An escaped char literal: skip the escape and the closing
        # quote, so a brace in it cannot move the depth.
        j = i + 3
        while (j <= n && substr(s, j, 1) != "'") j++
        i = j
        continue
      }
      if (nxt != "" && substr(s, i + 2, 1) == "'") {
        # A one-character literal: 'x'
        i = i + 2
        continue
      }
      # A lifetime ('a), not a char literal: an ordinary character.
      continue
    }
    if (ch == "{") o++
    else if (ch == "}") c++
    else if (ch == "[") b++
    else if (ch == "]") b--
  }
}
FNR == 1 {
  # Files start fresh: depth is per-file, and a run the
  # previous file left open is charged to that file, not
  # filed against that file s first declaration.
  if (inrun) charge(runlen)
  if (depth != 0) {
    printf "FAIL: brace depth ended at %d in %s; the trait-body accounting cannot be trusted\n", depth, charged_to > "/dev/stderr"
    bad_depth = 1
  }
  if (block_depth != 0) {
    printf "FAIL: %d block comment(s) were still open in %s; the comment text was never closed\n", block_depth, charged_to > "/dev/stderr"
    bad_depth = 1
  }
  inrun = 0; runlen = 0; depth = 0
  attr_depth = 0
  in_pub_trait = 0; trait_open = 0
  charged_to = FILENAME
  in_string = 0
  block_depth = 0
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
  # One scan per line: `o` / `c` are the braces, `b` the bracket
  # balance. The attribute logic below reads `b` out of this same
  # scan — a second scan of the line would advance the shared
  # `in_string` state twice.
  scan($0)
  if (!in_pub_trait) {
    # A run just ended: classify the declaration it documented.
    if (inrun) {
      # Attributes, `impl` blocks and bare braces are not the
      # declaration; keep looking at the next line.
      if (attr_depth > 0) {
        # Inside a multi-line attribute: its continuation lines
        # are part of the attribute, not a declaration.
        attr_depth += b
        if (attr_depth < 0) attr_depth = 0
      } else if (t ~ /^#\[/) {
        # An attribute: skip the whole of it, not just its
        # first line — a `#[derive(` that continues on the
        # next lines must not be mistaken for a declaration.
        attr_depth = b
        if (attr_depth < 0) attr_depth = 0
      } else if (t == "" || t ~ /^impl[ \t{]/ || t ~ /^[])}]/ || t ~ /^[{([][ \t]*$/) {
        # not the declaration; fall through to the accounting
      } else {
        if (t !~ /^pub[ \t(]/) charge(runlen)
        inrun = 0; runlen = 0
      }
    }

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
  if (block_depth != 0) {
    printf "FAIL: %d block comment(s) were still open in %s; the comment text was never closed\n", block_depth, charged_to > "/dev/stderr"
    bad_depth = 1
  }
  if (bad_depth) exit 1
  for (f in priv) printf "%6d  %s\n", priv[f], f
}
AWK
)

# The classifier's verdict on one file: the number of `///` lines it
# charges against the private budget. A file the classifier could not
# account for (a brace depth that does not close) is a failure, not a
# zero — a silent zero would let a broken classifier pass.
classify_file() {
  local file="$1" out
  if ! out=$(awk "$CLASSIFIER_AWK" "$file"); then
    echo "FAIL: the classifier could not account for $file" >&2
    return 1
  fi
  awk '{ sum += $1 } END { print sum + 0 }' <<<"$out"
}

# One fixture, one expected charge: the number the classifier must
# file against the private budget.
check_count() {
  local name="$1" expected="$2" file="$3" got
  if ! got=$(classify_file "$file"); then
    printf 'FAIL: %s — the classifier could not account for %s\n' "$name" "$file" >&2
    return 1
  fi
  if [[ "$got" != "$expected" ]]; then
    printf 'FAIL: %s — %s charged %s private-doc lines, expected %s\n' \
      "$name" "$file" "$got" "$expected" >&2
    return 1
  fi
  printf 'ok: %s — charged %s private-doc lines\n' "$name" "$got"
  return 0
}

# ---- The classifier's own fixtures --------------------------------

# The gate behaviour is pinned by a test, not by a reviewer's memory
# of it: the cases below are the ones a line-shaped classifier gets
# wrong.
self_test() {
  local tmp failures=0
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' RETURN

  # A multi-line `#[derive(...)]` before a `pub` item: the attribute
  # is skipped whole, so the public item's doc is not charged.
  cat >"$tmp/derive_pub.rs" <<'FIXTURE'
/// Doc on a public struct.
#[derive(
    Debug,
    Clone,
)]
pub struct PublicOne;
FIXTURE

  # The same attribute before a private item: the whole doc run is
  # charged against the private budget.
  cat >"$tmp/derive_private.rs" <<'FIXTURE'
/// Doc line one on a private struct.
/// Doc line two on a private struct.
#[derive(
    Debug,
    Clone,
)]
struct PrivateOne;
FIXTURE

  # A plain private item: its doc run is charged.
  cat >"$tmp/private_fn.rs" <<'FIXTURE'
/// Doc on a private function.
fn later() {}
FIXTURE

  # A bracket inside a string is not a bracket: the attribute closes
  # on its own line, and the public item's doc stays public.
  cat >"$tmp/doc_bracket_pub.rs" <<'FIXTURE'
/// Doc on a public struct.
#[doc = "an unbalanced [ bracket"]
pub struct T;
FIXTURE

  # ... and the public item's doc must not be charged to the private
  # item that follows it either.
  cat >"$tmp/doc_bracket_later.rs" <<'FIXTURE'
/// Doc on a public struct.
#[doc = "an unbalanced [ bracket"]
pub struct T;

/// Doc on a private fn.
fn later() {}
FIXTURE

  # A bracket inside a `cfg` feature name is the same case.
  cat >"$tmp/cfg_feature_bracket.rs" <<'FIXTURE'
/// Doc on a public struct.
#[cfg(feature = "x[")]
pub struct T;
FIXTURE

  # A brace inside a doc string is not a brace: the trait-body
  # accounting rides on the brace depth, so a `{` in a doc comment
  # used to move the depth and the file no longer closed.
  cat >"$tmp/doc_brace_pub.rs" <<'FIXTURE'
/// Doc on a public struct.
#[doc = "an unbalanced { brace"]
pub struct T;
FIXTURE

  # A brace inside a `cfg` feature name is the same case.
  cat >"$tmp/cfg_feature_brace.rs" <<'FIXTURE'
/// Doc on a public struct.
#[cfg(feature = "x{")]
pub struct T;
FIXTURE

  # A brace inside a line comment is not a brace either: the comment
  # is not code, so the depth the body opened is the depth the body
  # closes.
  cat >"$tmp/comment_brace.rs" <<'FIXTURE'
/// Doc on a private function.
fn later() {
    // a { in a line comment is not a brace
}
FIXTURE

  # A public item with no attribute at all: its doc is public.
  cat >"$tmp/plain_pub.rs" <<'FIXTURE'
/// Doc on a public struct.
pub struct PublicPlain;
FIXTURE

  # A brace inside a block comment is not a brace: the trait-body
  # accounting rides on the brace depth, so a `{` in a block comment
  # moves the depth and the file no longer closes.
  cat >"$tmp/block_comment_brace.rs" <<'FIXTURE'
/// Doc on a private function.
fn later() {
    /* a { in a block comment is not a brace */
}
FIXTURE

  # Rust block comments nest, so the scanner needs a depth counter
  # and not a flag: an inner `*/` does not close an outer comment,
  # and the text between them is still comment.
  cat >"$tmp/nested_block_comment.rs" <<'FIXTURE'
/// Doc on a private function.
fn later() {
    /* outer {
       /* inner */
       still inside the outer comment {
    */
}
FIXTURE

  # A `/*` inside a line comment opens nothing: the line comment ends
  # first, so the rest of the file is still code.
  cat >"$tmp/line_comment_block_opener.rs" <<'FIXTURE'
/// Doc on a private function.
fn later() {
    // /* a block comment opener inside a line comment opens nothing
    let value = 1;
}
FIXTURE

  check_count "multi-line derive before a pub item" 0 \
    "$tmp/derive_pub.rs" || failures=$((failures + 1))
  check_count "multi-line derive before a private item" 2 \
    "$tmp/derive_private.rs" || failures=$((failures + 1))
  check_count "a plain private item" 1 \
    "$tmp/private_fn.rs" || failures=$((failures + 1))
  check_count "a bracket in a doc string before a pub item" 0 \
    "$tmp/doc_bracket_pub.rs" || failures=$((failures + 1))
  check_count "a bracket in a doc string before a later private item" 1 \
    "$tmp/doc_bracket_later.rs" || failures=$((failures + 1))
  check_count "a bracket in a cfg feature name" 0 \
    "$tmp/cfg_feature_bracket.rs" || failures=$((failures + 1))
  check_count "a brace in a doc string before a pub item" 0 \
    "$tmp/doc_brace_pub.rs" || failures=$((failures + 1))
  check_count "a brace in a cfg feature name" 0 \
    "$tmp/cfg_feature_brace.rs" || failures=$((failures + 1))
  check_count "a brace in a line comment" 1 \
    "$tmp/comment_brace.rs" || failures=$((failures + 1))
  check_count "a plain pub item" 0 \
    "$tmp/plain_pub.rs" || failures=$((failures + 1))
  check_count "a brace in a block comment" 1 \
    "$tmp/block_comment_brace.rs" || failures=$((failures + 1))
  check_count "a nested block comment" 1 \
    "$tmp/nested_block_comment.rs" || failures=$((failures + 1))
  check_count "a block comment opener inside a line comment" 1 \
    "$tmp/line_comment_block_opener.rs" || failures=$((failures + 1))

  if [[ $failures -ne 0 ]]; then
    printf 'FAIL: %s self-test case(s) failed\n' "$failures" >&2
    return 1
  fi
  echo "OK: the harness-docs classifier passes its own fixtures"
}

case "${1:-}" in
  --self-test)
    self_test
    exit 0
    ;;
  "")
    ;;
  *)
    echo "usage: $0 [--self-test]" >&2
    exit 2
    ;;
esac

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
# Six things this classifier must get right, all learned the hard
# way: gawk does not treat `\b` as a word boundary inside a regex
# constant (`/^pub\b/` silently matches nothing, which would charge
# every `pub` run to the private budget), so the visibility test
# is an explicit character class. The declaration is read from
# the record that *ends* a doc run, not from a `getline` probe —
# a probe lands on the run's own second line for a multi-line doc
# comment and files the whole run against a fake declaration. An
# attribute is skipped whole, not line by line: a multi-line
# `#[derive(` … `)]` continues on lines that look like a
# declaration, and charging the run against one of them files a
# *public* item's doc against the private budget. The brackets that
# decide where an attribute ends are counted over the code
# characters only, so a `[` inside a string literal cannot leave the
# attribute open and hand a public item's doc to the private budget.
# The braces that decide where a trait body ends are counted over
# the same characters, for the same reason: a `{` inside a string or
# a line comment is text, and a counter that reads it as a brace
# moves the depth of a file that closes perfectly well. And the
# trait-body exemption rides on brace depth, so the counter must
# close to zero in every file — a file where it does not (a raw
# string full of braces, say) fails the gate here instead of
# silently mis-charging every run after the drift.
if ! breakdown=$(
  find "$SRC_ROOT" -name '*.rs' -print0 |
    xargs -0 awk "$CLASSIFIER_AWK" | sort -rn
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
