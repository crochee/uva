#!/usr/bin/env bash
# Front-end method gate: every method name the browser's domain
# clients hard-code must be a method the server actually serves.
#
# Why this exists: `uva-web/src/api/*.ts` is a set of per-domain
# clients, and each one carries a `METHOD` table — the RPC method
# names it calls, written as plain strings. A typo in one of those
# strings compiles: `tsc` only knows the types, and
# `check-web-contract` only diffs `types.ts`. The method simply does
# not exist on the wire, and the browser finds out at runtime, as a
# JSON-RPC "method not found", with nothing in the build to say so.
# The only defence today is the manual MCP smoke pass.
#
# The server's method table is the oracle, and it is one table:
#
#   - the RPC methods the router registers —
#     `router.register("…")` in `crates/uva-app-server/src/rpc/methods/`;
#   - the notification methods the server pushes —
#     `crates/uva-app-server/src/rpc/notify.rs` and the Anthropic
#     compatibility event (`crates/uva-app-server/src/routes/messages/`).
#     The browser subscribes to those by name, so a name it spells
#     wrongly is the same bug as a misspelt request method.
#
# The direction of the comparison is one-way on purpose: the server
# serves 64 methods and the browser uses a subset of them, so a
# server method the front end never calls is a fact about the
# product, not a defect. Only the other direction is a defect — the
# front end calling a method that does not exist.
#
# The contract catalog (`crates/uva-app-server/src/api/contract.rs`)
# is checked too: every concrete method it names must be a method the
# server serves, so the generated `types.ts` cannot claim a surface
# that is gone. Its wildcard rows (`*/list`, `schedules/*`) name a
# *family* of methods rather than one, and are not method names.
#
# # A text gate must be loud about what it cannot read
#
# This gate reads text, so it can only vouch for text it understands.
# Its blind spot used to be silent: the parser knew exactly one shape
# of table and one shape of row, and everything else — a table
# declared with a type annotation, a method name written as a template
# literal — was skipped without a word. The other tables still parsed,
# so the gate stayed green while a whole table's methods went
# unchecked. That is the worst failure a gate can have: it says "OK"
# about a table it never read.
#
# Silence is therefore not acceptable here, and the gate now asserts
# that it read everything it was asked to read:
#
#   - every file under `uva-web/src/api` that declares a `METHOD`
#     table must have contributed at least one record. A table
#     the parser could not read at all fails the gate, naming the
#     file.
#   - every row of a `METHOD` table must be a row the extractor can
#     read. A row whose value is computed — or a template literal
#     carrying an interpolation — fails the gate, naming the file
#     and the key: a table with five literal rows and one computed
#     row used to pass in silence, the computed row checked by
#     nobody. A row whose **key** is computed (`[computeKey()]:`)
#     and a row whose value sits on the line after its key fail the
#     same way: the reader reads the table's members, so a member
#     it cannot name is a member it cannot vouch for.
#   - every row of the contract catalog must be a row the extractor
#     reads. A row built by a callee the extractor does not know
#     fails the gate, naming the file and the callee.
#
# Scope: the `METHOD` tables in `uva-web/src/api/`. The gate is blind
# to the request envelopes and the streaming frame shapes the catalog
# deliberately leaves out (see `contract.rs`), and to a method string
# written inline outside a `METHOD` table — the same blindness
# `check-web-contract` has, for the same reason.
#
# Exit 0 with an `OK:` line, or exit 1 naming the file, the table key
# and the method the server does not serve — or the file, the table
# key and the reason the row could not be read.

set -euo pipefail

cd "$(dirname "$0")/.."

METHODS_GLOB="crates/uva-app-server/src/rpc/methods/*.rs"
NOTIFY_FILES=(
  crates/uva-app-server/src/rpc/notify.rs
  crates/uva-app-server/src/routes/messages/mod.rs
)
CONTRACT="crates/uva-app-server/src/api/contract.rs"
WEB_API_DIR="uva-web/src/api"

# The callees the contract catalog's rows are built by. A row built by
# any other callee is a row the extractor below does not read, and a
# row the extractor does not read is a row this gate cannot vouch for.
CATALOG_CALLEES="response request generic_response"

# The shape of a catalogue row the extractor reads: a turbofish-shaped
# call whose first argument is a string literal. One pattern, used by
# the extractor and by the row count it is compared against — the two
# must not drift apart, or the comparison compares nothing.
CATALOG_ROW_PATTERN='(response|request|generic_response)::<[^(]*>\([[:space:]]*"[^"]+"'

if [[ ! -d "$WEB_API_DIR" ]]; then
  echo "FAIL: $WEB_API_DIR does not exist — the front-end domain clients live there" >&2
  exit 1
fi

if [[ ! -f "$CONTRACT" ]]; then
  echo "FAIL: $CONTRACT does not exist — the wire contract catalog is the server's" >&2
  exit 1
fi

# Every method the server serves.
#
# The router's registrations are the requests; the notification
# constants are the pushes. Both are string literals in the sources
# above, and the router's own `method_names()` reads the same
# literals, so the two cannot disagree about a spelling.
server_methods() {
  {
    grep -ho 'router\.register("[^"]*"' $METHODS_GLOB \
      | sed 's/router\.register("//; s/"$//'
    grep -ho 'pub const [A-Z_]*: &str = "[^"]*"' "${NOTIFY_FILES[@]}" \
      | sed 's/.*= "//; s/"$//'
  } | sort -u
}

# Every method the contract catalog names, one per line. The catalog's
# rows are `response::<T>("method", …)` / `request::<T>(…)` /
# `generic_response::<T>(…)`, and the method is the first argument —
# which may sit on the next line, so the file is folded onto one line
# before the rows are picked out.
catalog_methods() {
  tr '\n' ' ' <"$CONTRACT" \
    | grep -oE "$CATALOG_ROW_PATTERN" \
    | sed 's/.*"\([^"]*\)"$/\1/' \
    | sort -u
}

# Every row the contract catalog has, whichever callee builds it: a
# turbofish-shaped call whose first argument is a string literal. One
# row per line:
#
#   <callee>|<method>
#
# The file is folded onto one line first, because the method name may
# sit on the line after the callee. The callee is the identifier in
# front of the turbofish — the name the extractor has to know to read
# the row at all.
catalog_rows() {
  tr '\n' ' ' <"$CONTRACT" \
    | grep -oE '[A-Za-z_][A-Za-z0-9_]*::<[^(]*>\([[:space:]]*"[^"]+"' \
    | sed -E 's/^([A-Za-z_][A-Za-z0-9_]*)::<[^(]*>\([[:space:]]*"([^"]*)"$/\1|\2/' \
    || true
}

# The front end's `METHOD` tables: every `key: 'value'` pair inside a
# `const METHOD = { … }` block, one line each:
#
#   <file>|<key>|<value>
#
# The declaration may carry a type annotation
# (`const METHOD: Record<string, string> = {`), and a row's value may
# be written with single quotes, double quotes or backticks — the
# three ways a method name is spelled here.
#
# The parser is **table-shaped**: it takes the table's body — every
# line between the `const METHOD … = {` header and the closing brace
# of the literal — and reads the members out of it. A line-shaped
# reader cannot see a member whose key it does not recognise, and it
# drops that member in silence: a computed key
# (`[computeKey()]: 'value'`) matched no key pattern at all, so a
# table with five literal rows and one computed row still yielded
# five rows, the per-file coverage check passed, and the gate said
# `OK` about a row nobody checked. Reading the table's members
# instead of guessing them from the line shape means every member
# produces a record — a readable one or an unreadable one — and
# nothing is ever dropped in silence.
#
# A row whose value is NOT a plain string literal is a row the gate
# cannot read, and it is reported as one:
#
#   <file>|<key>|#unreadable|<why>
#
# Four shapes land there. A computed value (`key: computeMethod(…)`)
# is not a hard-coded method name at all — the gate cannot know what
# it will be. A backtick template carrying an interpolation
# (`` `agents/${id}/x` ``) is the same thing in disguise: the
# backticks make it look like a literal, but the interpolation makes
# it computed. A value that opens a quote and never closes it is
# unreadable for the same reason. And a **computed key** is not a key
# the gate can name: the report names the key, so a key the gate
# cannot name is a row it cannot vouch for.
#
# Comparing such a row against the server's method list would report
# the wrong thing ("the server serves no such method") about a row
# nobody checked, so the row is reported as unreadable instead.
#
# The gate reads a member whose key and value sit on the same line.
# A value on the line *after* its key (`key:` then `'value',`) is
# reported as unreadable for the same reason: the reader reads one
# member at a time, and a member whose value is on another line is
# a member it cannot read without guessing where the member ends.
#
# The body ends at the first line that starts with `}` — the closing
# brace of the table literal — so a table written across several lines
# is read whole and the rest of the module is not.
#
# A `<file>|#tables|<n>` line closes every file that contributed at
# least one row, so the gate can report how many tables it read.
frontend_methods() {
  local dir="${1:-$WEB_API_DIR}"
  find "$dir" -name '*.ts' -type f | sort | while IFS= read -r file; do
    awk -v file="$file" '
      BEGIN {
        sq = sprintf("%c", 39)
        dq = sprintf("%c", 34)
        bq = sprintf("%c", 96)
        tables = 0
        rows = 0
        unreadable = 0
        in_table = 0
        n = 0
      }
      # The table header: `const METHOD = {`, with or without a type
      # annotation.
      /const METHOD([[:space:]]*:[[:space:]]*[^=]*)?[[:space:]]*=[[:space:]]*\{/ {
        in_table = 1
        tables++
        n = 0
        next
      }
      # The closing brace of the table literal: the body is read
      # whole, member by member.
      in_table && /^\}/ {
        read_table()
        in_table = 0
        next
      }
      in_table {
        body[++n] = $0
        next
      }
      END {
        if (rows > 0 || unreadable > 0) printf "%s|#tables|%d\n", file, tables
      }
      # The bracketed expression at the head of a line — a computed
      # key. The nesting is counted, so `[a[0]]` is one key.
      function bracketed(text,  j, depth, ch) {
        depth = 0
        for (j = 1; j <= length(text); j++) {
          ch = substr(text, j, 1)
          if (ch == "[") depth++
          else if (ch == "]") {
            depth--
            if (depth == 0) return substr(text, 1, j)
          }
        }
        return ""
      }
      # One member of the table: its key, whether the key is computed,
      # and the text that follows the colon.
      function emit_member(key, computed, rest,  quote, text, stop, value) {
        if (computed) {
          # A computed key: the report names the key, and this one
          # has no name the gate can read.
          unreadable++
          printf "%s|%s|#unreadable|the key is computed, not a plain identifier\n", file, key
          return
        }
        sub(/^[ \t]+/, "", rest)
        if (rest == "") {
          # The value is on the next line: the reader reads one
          # member at a time, and it cannot tell where this one
          # ends.
          unreadable++
          printf "%s|%s|#unreadable|the value is not on the same line as its key\n", file, key
          return
        }
        quote = substr(rest, 1, 1)
        if (quote != sq && quote != dq && quote != bq) {
          # A computed value: not a method name this gate can read.
          unreadable++
          printf "%s|%s|#unreadable|the value is computed, not a plain string literal\n", file, key
          return
        }
        text = substr(rest, 2)
        stop = index(text, quote)
        if (stop == 0) {
          # An unterminated literal: the gate cannot tell where the
          # value ends, so it cannot read it.
          unreadable++
          printf "%s|%s|#unreadable|the value is not a closed string literal\n", file, key
          return
        }
        value = substr(text, 1, stop - 1)
        if (quote == bq && index(value, "${") > 0) {
          # A template with an interpolation: computed, not a literal.
          unreadable++
          printf "%s|%s|#unreadable|the value is a template with an interpolation, not a plain string literal\n", file, key
          return
        }
        rows++
        printf "%s|%s|%s\n", file, key, value
      }
      # Read the members out of the table body.
      function read_table(  i, line, trimmed, key, rest) {
        for (i = 1; i <= n; i++) {
          trimmed = body[i]
          sub(/^[ \t]+/, "", trimmed)
          sub(/[ \t]+$/, "", trimmed)
          if (trimmed == "" || trimmed ~ /^\/\//) continue
          if (match(trimmed, /^[A-Za-z_$][A-Za-z0-9_$]*:/)) {
            key = substr(trimmed, 1, RLENGTH - 1)
            rest = substr(trimmed, RLENGTH + 1)
            emit_member(key, 0, rest)
            continue
          }
          if (substr(trimmed, 1, 1) == "[") {
            key = bracketed(trimmed)
            if (key == "") {
              # An unterminated bracket: the gate cannot tell where
              # the key ends, so it cannot name it.
              unreadable++
              printf "%s|[%s|#unreadable|the key is not a closed bracket expression\n", file, substr(trimmed, 2)
              continue
            }
            rest = substr(trimmed, length(key) + 1)
            sub(/^[ \t]*:/, "", rest)
            emit_member(key, 1, rest)
            continue
          }
          # Any other line is the continuation of a member whose
          # value sits on its own line; that member is already
          # reported as unreadable, so the continuation is not a
          # member of its own.
        }
      }
    ' "$file"
  done
}

# ---- The parser's own fixtures ------------------------------------
#
# The parser reads text, so the only defence against a member shape
# it mis-reads is a fixture that pins what it reports. The blind spot
# this closes used to be silent: a member the reader did not
# recognise was dropped, and the gate said `OK` about a row nobody
# checked. Every fixture below is a shape the reader can meet, and
# every one of them must produce a record — a readable one or an
# unreadable one — never nothing.
self_test() {
  local tmp failures=0 actual expected
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' RETURN

  # A member whose key and value are plain: three ways of spelling a
  # string literal, all readable.
  cat >"$tmp/readable.ts" <<'FIXTURE'
const METHOD = {
  plain: 'agents/list',
  double: "agents/get",
  template: `agents/default`,
} as const;
FIXTURE

  # A computed key: the report names the key, and this one has no
  # name the gate can read.
  cat >"$tmp/computed_key.ts" <<'FIXTURE'
const METHOD = {
  list: 'agents/list',
  [computeKey()]: 'agents/detail',
} as const;
FIXTURE

  # A value on the line after its key: the reader reads one member at
  # a time, and it cannot tell where this one ends.
  cat >"$tmp/split_value.ts" <<'FIXTURE'
const METHOD = {
  detail:
    'agents/get',
} as const;
FIXTURE

  # A computed value: not a method name the gate can read.
  cat >"$tmp/computed_value.ts" <<'FIXTURE'
const METHOD = {
  detail: computeMethod('agents/get'),
} as const;
FIXTURE

  # A template carrying an interpolation: computed in disguise.
  cat >"$tmp/interpolated.ts" <<'FIXTURE'
const METHOD = {
  detail: `agents/${id}`,
} as const;
FIXTURE

  # A value that opens a quote and never closes it.
  cat >"$tmp/unterminated.ts" <<'FIXTURE'
const METHOD = {
  detail: 'agents/get
} as const;
FIXTURE

  actual="$(frontend_methods "$tmp")"
  expected="$tmp/computed_key.ts|list|agents/list
$tmp/computed_key.ts|[computeKey()]|#unreadable|the key is computed, not a plain identifier
$tmp/computed_key.ts|#tables|1
$tmp/computed_value.ts|detail|#unreadable|the value is computed, not a plain string literal
$tmp/computed_value.ts|#tables|1
$tmp/interpolated.ts|detail|#unreadable|the value is a template with an interpolation, not a plain string literal
$tmp/interpolated.ts|#tables|1
$tmp/readable.ts|plain|agents/list
$tmp/readable.ts|double|agents/get
$tmp/readable.ts|template|agents/default
$tmp/readable.ts|#tables|1
$tmp/split_value.ts|detail|#unreadable|the value is not on the same line as its key
$tmp/split_value.ts|#tables|1
$tmp/unterminated.ts|detail|#unreadable|the value is not a closed string literal
$tmp/unterminated.ts|#tables|1"

  if [[ "$actual" != "$expected" ]]; then
    printf 'FAIL: the METHOD table parser did not report what the fixtures pin\n' >&2
    printf -- '--- expected ---\n%s\n--- actual ---\n%s\n' "$expected" "$actual" >&2
    failures=$((failures + 1))
  else
    printf 'ok: the METHOD table parser reports every member shape\n'
  fi

  # A file with a table the reader could not read at all still has to
  # produce a record: the per-file coverage check is what turns a
  # silently unread table into a failure.
  local covered
  covered="$(printf '%s\n' "$actual" | cut -d'|' -f1 | sort -u)"
  if [[ "$(printf '%s\n' "$covered" | wc -l)" -ne 6 ]]; then
    printf 'FAIL: the parser did not produce a record for every fixture file\n' >&2
    failures=$((failures + 1))
  else
    printf 'ok: every fixture file produced a record\n'
  fi

  if [[ $failures -ne 0 ]]; then
    printf 'FAIL: %s self-test case(s) failed\n' "$failures" >&2
    return 1
  fi
  echo "OK: the METHOD table parser passes its own fixtures"
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

server="$(server_methods)"
if [[ -z "$server" ]]; then
  echo "FAIL: no server methods found — the router registrations moved" >&2
  exit 1
fi

failures=0

# What the front end declares, and what the parser could read of it.
frontend_rows="$(frontend_methods)"
parsed_files="$(printf '%s\n' "$frontend_rows" | cut -d'|' -f1 | sort -u)"

# 1. Every file that declares a `METHOD` table must have contributed a
#    record — a parsed row, or an unreadable one. A table the parser
#    could not read at all is a table whose methods nobody checked,
#    and that has to fail the gate rather than pass it in silence. A
#    table the parser read but whose values it could not read is
#    named by check 2 below.
while IFS= read -r file; do
  [[ -z "$file" ]] && continue
  if ! printf '%s\n' "$parsed_files" | grep -qxF "$file"; then
    echo "FAIL: $file — its METHOD table yielded no parsed row; the gate could not read the table and will not pass it in silence" >&2
    failures=$((failures + 1))
  fi
done <<< "$(grep -rl --include='*.ts' 'const METHOD' "$WEB_API_DIR" | sort || true)"

# 2. Every row the front end declares must be a row the gate could
#    read, and every method it names must be one the server serves.
#
#    A row the gate could not read — a computed value, a template
#    with an interpolation — is not compared against the server's
#    method list: the comparison would report "the server serves no
#    such method" about a row nobody checked. The row is named
#    instead, because a row the gate cannot read must never pass in
#    silence.
checked=0
tables=0
while IFS='|' read -r file key method why; do
  if [[ "$key" == '#tables' ]]; then
    tables=$((tables + method))
    continue
  fi
  if [[ "$method" == '#unreadable' ]]; then
    echo "FAIL: $file: METHOD.$key — $why; the gate could not read this row and will not pass it in silence" >&2
    failures=$((failures + 1))
    continue
  fi
  [[ -z "$method" ]] && continue
  checked=$((checked + 1))
  if ! printf '%s\n' "$server" | grep -qxF "$method"; then
    echo "FAIL: $file: METHOD.$key = '$method' — the server serves no such method" >&2
    failures=$((failures + 1))
  fi
done <<< "$frontend_rows"

if [[ "$checked" -eq 0 ]]; then
  echo "FAIL: no front-end METHOD tables found under $WEB_API_DIR — the gate has nothing to check" >&2
  exit 1
fi

# 3. Every row of the contract catalog must be a row the extractor
#    reads. A row built by a callee the extractor does not know would
#    be skipped in silence, so the rows in the file are counted and
#    compared with the rows the extractor reads, and an unknown callee
#    is named.
catalog_all="$(catalog_rows)"
catalog_total=$(printf '%s\n' "$catalog_all" | awk -F'|' 'NF == 2 { c++ } END { print c + 0 }')
catalog_known=$(printf '%s\n' "$catalog_all" | awk -F'|' -v known="$CATALOG_CALLEES" '
  BEGIN { n = split(known, k, " "); for (i = 1; i <= n; i++) { is_known[k[i]] = 1 } }
  NF == 2 && ($1 in is_known) { c++ }
  END { print c + 0 }
')
catalog_unknown=$(printf '%s\n' "$catalog_all" | awk -F'|' -v known="$CATALOG_CALLEES" '
  BEGIN { n = split(known, k, " "); for (i = 1; i <= n; i++) { is_known[k[i]] = 1 } }
  NF == 2 && !($1 in is_known) { print $1 }
' | sort -u)
catalog_extracted=$(tr '\n' ' ' <"$CONTRACT" \
  | grep -oE "$CATALOG_ROW_PATTERN" \
  | grep -c . || true)

if [[ "$catalog_known" != "$catalog_total" ]]; then
  echo "FAIL: $CONTRACT — $((catalog_total - catalog_known)) catalogue row(s) are built by a callee the extractor does not read (unknown: $(printf '%s' "$catalog_unknown" | tr '\n' ' ')); a row the extractor cannot see must not pass in silence" >&2
  failures=$((failures + 1))
fi

if [[ "$catalog_extracted" != "$catalog_total" ]]; then
  echo "FAIL: $CONTRACT — the catalogue has $catalog_total row(s) but the extractor reads $catalog_extracted of them; a row the extractor cannot see must not pass in silence" >&2
  failures=$((failures + 1))
fi

# 4. Every concrete method the contract catalog names must be one the
#    server serves. A wildcard row names a family of methods, not one,
#    so it is not a method name and is skipped.
while IFS= read -r method; do
  [[ -z "$method" ]] && continue
  [[ "$method" == *'*'* ]] && continue
  if ! printf '%s\n' "$server" | grep -qxF "$method"; then
    echo "FAIL: $CONTRACT names the method '$method' — the server serves no such method" >&2
    failures=$((failures + 1))
  fi
done < <(catalog_methods)

if (( failures > 0 )); then
  echo "FAIL: the front-end method gate found $failures problem(s)" >&2
  exit 1
fi

echo "OK: $checked front-end method names are all served by the server; the gate read $tables METHOD tables and $catalog_total catalogue rows"
