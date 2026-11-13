#!/usr/bin/env bash
# Frozen curated-surface gate: pins the *names* a frozen crate's
# lib.rs re-exports, not just how many `pub use` lines it has.
# `check-pub-surface-strict` ratchets the statement count; this gate
# ratchets the identity of every exported name, because a count that
# holds while `ReActAgent` quietly becomes `ReactAgent` holds nothing.
#
# What counts as a curated name: every identifier explicitly listed
# in a **crate-root** `pub use` item of the crate's `src/lib.rs` —
# including names nested in sub-group lists such as
# `pub use agent::{..., host::{Gate, Hook, ...}}`.
#
# Scope matters. A `pub use` inside a module — an inline
# `#[cfg(test)] mod tests { ... }`, a `pub mod foo { ... }` — names
# something *that module* re-exports, not something the crate root
# publishes, so it is not a curated name and the gate does not count
# it. The extractor walks the file's **top-level items**, so a nested
# `pub use` is never seen as a crate-root one: `pub use a::Alpha;`
# beside `mod internal { pub use b::Beta; }` publishes `Alpha` and
# not `Beta`.
#
# `self` resolves to the path in front of it: `pub use agent::{self,
# host}` publishes `agent` (and `host`).
#
# Glob re-exports (`pub use module::*;`, `pub use module::{a, *};`)
# are **refused** by this gate, in every form: a glob publishes an
# unbounded set of names, so the frozen surface could not be pinned.
# The repo-wide style ban lives in `make check-pub-surface`, which
# matches the single-segment lowercase form (`pub use foo::*;`) and
# exempts the facade, which propagates each crate's curated surface.
# This gate rule is the freeze, not the style ban — so it refuses
# every glob form in a frozen crate, and says which crate and which
# glob.
#
# Both directions are violations:
#   NEW:     crate::Name     — a name the baseline never promised
#   REMOVED: crate::Name     — a promised name that disappeared
#
# The baseline lives in `scripts/frozen-api/baseline`, one
# `crate-name::Name` per line, sorted. Changing the curated surface
# means changing the baseline and `docs/api-frozen.md` in the same
# commit, so the promise and the gate never disagree. The gate
# compares the **live surface against the baseline**; the doc is the
# human-readable half of the same promise, and its co-change is the
# review convention, not something this script can check.
#
# `--print` lists the current surface in baseline format (read-only;
# it never touches the file) so a maintainer can regenerate with:
#   scripts/check-frozen-api.sh --print > scripts/frozen-api/baseline
#
# `--self-test` runs the extractor's own fixtures — the scope cases
# (a `pub use` inside a module, a `#[cfg(test)] mod`), the same-line
# cases (two `pub use` on one line, a `pub use` behind an
# attribute), the glob refusal, and the NEW/REMOVED diff — so the
# gate behaviour is pinned by a test, not by a reviewer's memory of
# it.
#
# Exit 0 with an `OK:` line, or exit 1 naming every violation.

set -euo pipefail

cd "$(dirname "$0")/.."

# The frozen crates: the seven paradigm crates whose lib.rs is a
# curated surface. Kept explicit so a new crate never joins silently.
FROZEN_CRATES=(
  crates/uva-core
  crates/uva-context
  crates/uva-harness
  crates/uva-provider
  crates/uva-session
  crates/uva-steering
  crates/uva-tool
)

baseline_file="scripts/frozen-api/baseline"

# Print one curated name per line for a single lib.rs. Pure awk: the
# statement accumulator joins multi-line `pub use ... ;` blocks, and
# the recursive group parser walks nested brace lists, so a name is
# never confused with the module path in front of it.
#
# The scan is **scope-aware**: it walks the file top-level items —
# the text of one item ends at a `;` at brace depth 0 or at the
# closing brace of a block item — so a `pub use` inside a module
# block is part of that module item, never a crate-root name of its
# own. `mode` selects the output:
#
#   names  — the curated names, one per line
#   globs  — the `pub use` items that carry a glob, one per line
extract_names() {
  awk -v mode="${2:-names}" -v qchar="'" '
    function trim(s) {
      sub(/^[ \t\r\n]+/, "", s); sub(/[ \t\r\n]+$/, "", s); return s
    }
    function last_seg(p,  n, a) {
      sub(/::[ \t]*$/, "", p)   # a brace prefix ends in `::`
      n = split(p, a, "::"); return trim(a[n])
    }
    # Strip the leading attributes of an item: `#[cfg(test)] pub use …`.
    # An attribute ends at its first `]`, and Rust attributes do not
    # nest.
    function strip_attributes(s) {
      while (substr(s, 1, 2) == "#[") {
        bracket = index(s, "]")
        if (bracket == 0) break
        s = substr(s, bracket + 1)
      }
      return s
    }
    function print_name(name) {
      if (mode == "names") print name
    }
    # A glob publishes an unbounded set of names: the frozen surface
    # could not be pinned, so the gate refuses it and names it.
    function report_glob() {
      if (mode == "globs") print use_text
    }
    function emit_item(item, prefix) {
      item = trim(item)
      if (item == "") return
      if (item == "self") { print_name(last_seg(prefix)); return }
      if (match(item, /[ \t]+as[ \t]+/)) item = substr(item, RSTART + RLENGTH)
      if (index(item, "*") > 0) { report_glob(); return }
      print_name(last_seg(item))
    }
    # s (the use tree) and pos (the scan offset) are shared with the
    # recursive caller so nested groups resume where they ended.
    function parse_group(prefix,  start, c, saved) {
      start = pos
      while (pos <= length(s)) {
        c = substr(s, pos, 1)
        if (c == "}") {
          emit_item(substr(s, start, pos - start), prefix)
          pos++
          return
        } else if (c == "{") {
          # The nested group has its own prefix; the outer one is
          # restored afterwards, so a `self` after the group still
          # resolves against the outer path.
          saved = prefix
          prefix = trim(substr(s, start, pos - start))
          pos++
          parse_group(prefix)
          prefix = saved
          start = pos
        } else if (c == ",") {
          emit_item(substr(s, start, pos - start), prefix)
          pos++
          start = pos
        } else {
          pos++
        }
      }
    }
    function parse_stmt(stmt,  b) {
      s = trim(stmt); sub(/^pub use /, "", s)
      use_text = "pub use " s
      b = index(s, "{")
      if (b == 0) { emit_item(s, ""); return }
      pos = b + 1
      parse_group(trim(substr(s, 1, b - 1)))
    }
    # One top-level item. A `pub use` inside a module block is part
    # of that module item, so it never becomes a crate-root name of
    # its own.
    function handle_item(raw,  s) {
      s = trim(strip_attributes(trim(raw)))
      if (s !~ /^pub use /) return
      parse_stmt(s)
    }
    {
      line = $0
      sub(/\/\/.*$/, "", line)   # pub use lines carry no string literals
      n = length(line)
      for (i = 1; i <= n; i++) {
        ch = substr(line, i, 1)
        if (in_string) {
          if (ch == "\\") { i++ }
          else if (ch == "\"") { in_string = 0 }
          item = item ch
          continue
        }
        if (ch == "\"") { in_string = 1; item = item ch; continue }
        if (ch == qchar && substr(line, i + 1, 1) == "\\") {
          # A char literal: skip to its closing quote, so a brace in
          # it cannot move the depth.
          j = i + 2
          while (j <= n && substr(line, j, 1) != qchar) j++
          item = item substr(line, i, j - i + 1)
          i = j
          continue
        }
        if (ch == "{") {
          depth++
          item = item ch
          continue
        }
        if (ch == "}") {
          depth--
          item = item ch
          if (depth <= 0) {
            # A block item ends with its closing brace — a `mod x {
            # … }` has no trailing `;`, and a `pub use a::{…}`
            # group ends the same way. Either way the item is over.
            depth = 0
            handle_item(item)
            item = ""
          }
          continue
        }
        if (ch == ";" && depth == 0) {
          handle_item(item)
          item = ""
          continue
        }
        item = item ch
      }
    }
    END {
      if (item ~ /[^ \t\r\n]/)
        print "unterminated top-level item:" item > "/dev/stderr"
    }
  ' "$1"
}

# The curated names of every frozen crate, in baseline format.
current_names() {
  local crate lib_rs
  for crate in "${FROZEN_CRATES[@]}"; do
    lib_rs="$crate/src/lib.rs"
    if [[ ! -f "$lib_rs" ]]; then
      echo "FAIL: $lib_rs not found — a frozen crate cannot go missing silently" >&2
      exit 1
    fi
    extract_names "$lib_rs" names | LC_ALL=C sort -u | sed "s|^|${crate##*/}::|"
  done | LC_ALL=C sort -u
}

# Every glob re-export in a frozen crate, one per line, prefixed with
# the crate it sits in.
current_globs() {
  local crate lib_rs
  for crate in "${FROZEN_CRATES[@]}"; do
    lib_rs="$crate/src/lib.rs"
    if [[ ! -f "$lib_rs" ]]; then
      echo "FAIL: $lib_rs not found — a frozen crate cannot go missing silently" >&2
      exit 1
    fi
    extract_names "$lib_rs" globs | sed "s|^|${crate##*/}: |"
  done
}

# The diff between the live surface and the baseline, as the gate
# reports it: one `NEW:` and one `REMOVED:` line per difference.
# Both inputs must be sorted — `comm` says so itself, and an
# unsorted input would report a wrong diff.
compare_names() {
  local current="$1" baseline="$2"
  LC_ALL=C comm -23 "$current" "$baseline" | sed 's/^/  NEW: /'
  LC_ALL=C comm -13 "$current" "$baseline" | sed 's/^/  REMOVED: /'
}

# ---- The extractor's own fixtures --------------------------------

# The gate behaviour is pinned by a test, not by a reviewer's memory
# of it: the cases below are the ones a line-shaped extractor gets
# wrong.
self_test() {
  local tmp failures=0
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' RETURN

  # A crate root with names on both sides of the scope line.
  cat >"$tmp/lib.rs" <<'FIXTURE'
//! A fixture crate.
pub use a::Alpha;
pub use b::Beta as Renamed;
pub use c::{self, Gamma};
pub use d::{Delta, nested::{Epsilon, Zeta}};
mod internal {
    pub use e::NestedBeta;
}
#[cfg(test)]
mod tests {
    pub use f::NestedGamma;
    pub use g::NestedDelta;
}
#[cfg(feature = "x")]
pub use h::Alpha;
pub use i::Zeta; pub use j::Eta;
FIXTURE

  local got expected
  got=$(extract_names "$tmp/lib.rs" names | LC_ALL=C sort -u | tr '\n' ' ')
  expected="Alpha Delta Epsilon Eta Gamma Renamed Zeta c "
  if [[ "$got" != "$expected" ]]; then
    echo "FAIL: the extractor returned [$got], expected [$expected]"
    failures=$((failures + 1))
  fi

  # The nested names are the point of the fixture: they live in
  # modules, so they are not crate-root names.
  for nested in NestedBeta NestedGamma NestedDelta; do
    if extract_names "$tmp/lib.rs" names | grep -qx "$nested"; then
      echo "FAIL: a name from inside a module was counted: $nested"
      failures=$((failures + 1))
    fi
  done

  # A glob in a frozen crate is refused, and the refusal names it.
  cat >"$tmp/glob.rs" <<'FIXTURE'
pub use m::*;
pub use n::{O, *};
FIXTURE
  local globs
  globs=$(extract_names "$tmp/glob.rs" globs)
  if [[ "$(printf '%s\n' "$globs" | grep -c '\*')" != "2" ]]; then
    echo "FAIL: the glob fixture reported [$globs], expected two globs"
    failures=$((failures + 1))
  fi

  # The diff reports a new name and a removed name, and nothing else.
  printf 'crate::Fresh\ncrate::Kept\n' >"$tmp/live"
  printf 'crate::Gone\ncrate::Kept\n' >"$tmp/base"
  local diff
  diff=$(compare_names "$tmp/live" "$tmp/base")
  if [[ "$diff" != *"NEW: crate::Fresh"* || "$diff" != *"REMOVED: crate::Gone"* ]]; then
    echo "FAIL: the diff did not report the new and the removed name: $diff"
    failures=$((failures + 1))
  fi

  if [[ $failures -ne 0 ]]; then
    echo "FAIL: $failures self-test case(s) failed"
    return 1
  fi
  echo "OK: the frozen-surface extractor passes its own fixtures"
}

case "${1:-}" in
  --print)
    current_names
    exit 0
    ;;
  --self-test)
    self_test
    exit 0
    ;;
  "")
    ;;
  *)
    echo "usage: $0 [--print|--self-test]" >&2
    exit 2
    ;;
esac

if [[ ! -f "$baseline_file" ]]; then
  echo "FAIL: $baseline_file not found — the frozen surface has no source of truth" >&2
  exit 1
fi

# A glob in a frozen crate is refused before anything else: the
# surface could not be pinned, so there is nothing to compare.
globs=$(current_globs)
if [[ -n "$globs" ]]; then
  echo "FAIL: a frozen crate re-exports a glob; the curated surface cannot be pinned"
  while IFS= read -r glob; do
    [[ -n "$glob" ]] && echo "  GLOB: $glob"
  done <<<"$globs"
  echo "List the names explicitly (the crate's lib.rs is the single place"
  echo "that decides what is public), then regenerate the baseline with:"
  echo "  scripts/check-frozen-api.sh --print > scripts/frozen-api/baseline"
  exit 1
fi

tmp_cur=$(mktemp)
tmp_base=$(mktemp)
trap 'rm -f "$tmp_cur" "$tmp_base"' EXIT
current_names >"$tmp_cur"

# `comm` needs both inputs sorted, and the baseline is a file a
# human edits: an unsorted one would report a wrong diff — or make
# `comm` complain instead of naming the difference. Validate it
# first, so the failure says what is wrong.
LC_ALL=C sort "$baseline_file" >"$tmp_base"
if ! cmp -s "$baseline_file" "$tmp_base"; then
  echo "FAIL: $baseline_file is not sorted (or holds a duplicate)"
  echo "  The diff needs both inputs sorted; an unsorted baseline would"
  echo "  report the wrong difference. Fix it with:"
  echo "    LC_ALL=C sort -u -o $baseline_file $baseline_file"
  exit 1
fi

new_names=$(LC_ALL=C comm -23 "$tmp_cur" "$tmp_base")
removed_names=$(LC_ALL=C comm -13 "$tmp_cur" "$tmp_base")

if [[ -n "$new_names" || -n "$removed_names" ]]; then
  echo "FAIL: the curated public surface moved without a promise update"
  while IFS= read -r n; do [[ -n "$n" ]] && echo "  NEW: $n"; done <<<"$new_names"
  while IFS= read -r n; do [[ -n "$n" ]] && echo "  REMOVED: $n"; done <<<"$removed_names"
  echo "docs/api-frozen.md and scripts/frozen-api/baseline change together,"
  echo "in one reviewed commit — regenerate the list with:"
  echo "  scripts/check-frozen-api.sh --print > scripts/frozen-api/baseline"
  exit 1
fi

count=$(wc -l <"$baseline_file" | tr -d " ")
echo "OK: frozen curated surface unchanged — $count names across ${#FROZEN_CRATES[@]} crates match scripts/frozen-api/baseline"
