#!/usr/bin/env bash
# List-recipe gate: a list envelope is built by the recipe, not at a call site.
#
# Why this exists: every cursor-paginated list route answers the same
# six questions — which sort keys it accepts, how each orders rows,
# which sort a caller who names none gets, which way a `-` prefix turns
# it, which row a page resumes after, and what the envelope holds.
# Written per route, six copies drifted and every divergence was
# silent: a route promised a key it never sorted by, a route accepted a
# direction it never applied, a route's declared default never reached
# its ordering, two routes had no tiebreaker. `routes/list/` is the one
# implementation now (ADR 0027), and the way it stays the one
# implementation is that no route builds a page envelope itself.
#
# What counts as building one: a `List { … }` struct literal.
# `List::new` / `List::default` / `From<Vec<_>>` are not page
# construction (they carry no cursor and no total), and a projection
# goes through `List::map`.
#
# Allowed: the recipe module (it must build one) and the envelope's own
# module (its constructors may). Every other production file is red.
#
# Test code is skipped — an inline `#[cfg(test)] mod` block for its
# whole extent, plus `tests.rs` files and `tests/` directories. A test
# may build an envelope to compare against, which is assertion, not
# a second construction path.
#
# A line that is entirely a comment is skipped; a trailing comment that
# mentions `List {` would be reported, and the fix is to reword it.
#
# Exit 0 with an `OK:` line, or exit 1 with every offending site.

set -euo pipefail

cd "$(dirname "$0")/.."

# The one place a page envelope is built.
ALLOWED_DIR="crates/uva-app-server/src/routes/list"
ALLOWED_FILE="crates/uva-app-server/src/api/v1/list.rs"

mapfile -t files < <(
  find crates -name '*.rs' \( -path '*/src/*' -o -path '*/examples/*' \) \
    ! -name 'tests.rs' ! -path '*/tests/*' \
    | grep -v "^$ALLOWED_DIR/" \
    | grep -v "^$ALLOWED_FILE$" \
    | sort
)

if [[ ${#files[@]} -eq 0 ]]; then
  echo "FAIL: the list-recipe gate found no Rust files to scan" >&2
  exit 1
fi

report=$(printf '%s\n' "${files[@]}" | xargs awk '
# Brace delta of one line. Strings and comments are not modeled; a brace
# inside a string would skew this, and the repo has no such line in the
# files this gate scans.
function braces(text,   opens, closes, i, c, len) {
  opens = 0; closes = 0
  len = length(text)
  for (i = 1; i <= len; i++) {
    c = substr(text, i, 1)
    if (c == "{") opens++
    else if (c == "}") closes++
  }
  return opens - closes
}

FNR == 1 {
  depth = 0
  skip_at = -1
  pending = 0
}

{
  line = $0

  # Inside an inline test module: count braces until the block closes.
  if (skip_at >= 0) {
    depth += braces(line)
    if (depth <= skip_at) { skip_at = -1 }
    next
  }

  # A line that is only a comment builds nothing.
  if (line ~ /^[ \t]*\/\// || line ~ /^[ \t]*\*/) { next }

  opened = 0
  if (line ~ /#\[cfg\(test\)\]/) {
    if (line ~ /mod[ \t]+[A-Za-z0-9_]+[ \t]*\{/) {
      skip_at = depth
      opened = 1
    } else {
      pending = 1
    }
  } else if (pending) {
    if (line ~ /mod[ \t]+[A-Za-z0-9_]+[ \t]*\{/) {
      skip_at = depth
      opened = 1
      pending = 0
    } else if (line ~ /mod[ \t]+[A-Za-z0-9_]+[ \t]*;/) {
      pending = 0
    } else if (line !~ /^[ \t]*$/) {
      pending = 0
    }
  }

  if (!opened && line ~ /(^|[^A-Za-z0-9_])List[ \t]*\{/) {
    printf "%s:%d: list envelope built outside the recipe\n", FILENAME, FNR
  }

  depth += braces(line)
}
')

if [[ -n "$report" ]]; then
  echo "FAIL: a list envelope is built outside the list recipe:" >&2
  printf '%s\n' "$report" >&2
  echo "" >&2
  echo "Build the page through the route's sort key:" >&2
  echo "    RouteSort::sort_and_page(rows, &page, |row| row.id.as_str())" >&2
  echo "A route whose sort key is not on the wire row projects with" >&2
  echo "    List::map." >&2
  echo "Only $ALLOWED_DIR/ and $ALLOWED_FILE may build one (ADR 0027)." >&2
  exit 1
fi

echo "OK: every list envelope is built in $ALLOWED_DIR or $ALLOWED_FILE"
