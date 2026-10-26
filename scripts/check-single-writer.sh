#!/usr/bin/env bash
# Single-writer gate: a session's rows go through its one writer.
#
# Why this exists: the invariant this branch is named after is "one
# writer per session, one ordinal source". The writer lives in
# `synthia-session`, and every row a session gains — the model's
# turns, the tool results, the user's prompts, the feedback, the
# compaction rows, a fork's inherited rows — must go through it. A
# direct call to `SessionSink::append` / `append_many` outside the
# writer is a second write path: it draws an ordinal from the same
# counter but lands the row outside the writer's batch, so the row can
# miss the log's flush and the writer's ledger — and the next person
# who needs a row written outside the writer would add one and CI
# would stay green.
#
# What counts as a direct call:
#
# - `append_many` — the writer has no such method (a batch is the
#   writer's own flush), so any `.append_many(` outside the writer
#   writes a session's log behind its back.
# - `append` on a session sink — a value whose name ends in `sink`
#   (the repo's convention: `sink`, `session_sink`, `child_sink`) or
#   is `session_store`. The writer's own `append` is a different
#   method on a different type, and stays allowed.
#
# The scan skips the test code: an inline `#[cfg(test)] mod` block is
# skipped for its whole extent — not just its marker line, which is
# what a line-based grep can see — and the files a
# `#[cfg(test)] mod NAME;` declaration pulls in (`NAME.rs`, or the
# `NAME/` directory) are skipped as well.
#
# Exit 0 with an `OK:` line, or exit 1 with every offending call site.

set -euo pipefail

cd "$(dirname "$0")/.."

# The writer itself: the one place a session sink is written.
WRITER_DIR="crates/synthia-session/src/writer"

# The production files: a crate's `src/` and its `examples/`. The
# integration tests under `crates/*/tests/` are not production code.
mapfile -t all_files < <(
  find crates -name '*.rs' \( -path '*/src/*' -o -path '*/examples/*' \) \
    | sort
)

if [[ ${#all_files[@]} -eq 0 ]]; then
  echo "FAIL: no Rust sources found under crates/ — the gate has nothing to scan" >&2
  exit 1
fi

# Pass 1: the modules a `#[cfg(test)] mod NAME;` declaration pulls in.
# Their file (`NAME.rs`) or directory (`NAME/`) holds test code only.
test_modules=$(printf '%s\n' "${all_files[@]}" | xargs awk '
  /#\[cfg\(test\)\]/ {
    if (match($0, /mod[ \t]+[A-Za-z0-9_]+[ \t]*;/)) {
      name = substr($0, RSTART, RLENGTH)
      sub(/^mod[ \t]+/, "", name)
      sub(/[ \t]*;$/, "", name)
      print name
      next
    }
    pending = 1
    next
  }
  pending {
    if ($0 ~ /^[ \t]*#\[/) { next }          # another attribute
    if (match($0, /mod[ \t]+[A-Za-z0-9_]+[ \t]*;/)) {
      name = substr($0, RSTART, RLENGTH)
      sub(/^mod[ \t]+/, "", name)
      sub(/[ \t]*;$/, "", name)
      print name
      pending = 0
      next
    }
    if ($0 !~ /^[ \t]*$/) { pending = 0 }
  }
' | sort -u | tr '\n' ' ')

# Pass 2: the production files that are not test code.
production_files=()
for file in "${all_files[@]}"; do
  [[ "$file" == "$WRITER_DIR"/* ]] && continue
  case "$file" in
    */tests/*) continue ;;
  esac
  skip=0
  stem="${file##*/}"
  stem="${stem%.rs}"
  dir="${file%/*}"
  while [[ "$dir" == */* ]]; do
    parent="${dir##*/}"
    for name in $test_modules; do
      if [[ "$stem" == "$name" || "$parent" == "$name" ]]; then
        skip=1
        break 2
      fi
    done
    dir="${dir%/*}"
  done
  [[ "$skip" -eq 1 ]] && continue
  production_files+=("$file")
done

if [[ ${#production_files[@]} -eq 0 ]]; then
  echo "FAIL: no production files left to scan — the gate would pass vacuously" >&2
  exit 1
fi

# The scan itself. `code_only` drops comments and string contents, so
# a doc comment that mentions a call is not a call; the brace counter
# then knows where an inline `#[cfg(test)] mod` block ends.
report=$(printf '%s\n' "${production_files[@]}" | xargs awk -F: '
# `in_block` carries across lines: a `/* … */` comment that opens on
# one line and closes on a later one must hide its body on every line
# in between.
function code_only(text,   out, i, c, len, in_str, in_chr, esc) {
  out = ""
  len = length(text)
  i = 1
  in_str = 0; in_chr = 0; esc = 0
  while (i <= len) {
    c = substr(text, i, 1)
    if (in_block) {
      if (c == "*" && substr(text, i + 1, 1) == "/") { in_block = 0; i += 2; continue }
      i++; continue
    }
    if (esc) { esc = 0; i++; continue }
    if (in_str) {
      if (c == "\\") { esc = 1; i++; continue }
      if (c == "\"") { in_str = 0 }
      i++; continue
    }
    if (in_chr) {
      if (c == "\\") { esc = 1; i++; continue }
      if (c == "\047") { in_chr = 0 }
      i++; continue
    }
    if (c == "/" && substr(text, i + 1, 1) == "/") { break }
    if (c == "/" && substr(text, i + 1, 1) == "*") { in_block = 1; i += 2; continue }
    if (c == "\"") { in_str = 1; i++; continue }
    if (c == "\047") {
      if (substr(text, i + 1, 1) == "\\" || substr(text, i + 2, 1) == "\047") {
        in_chr = 1; i++; continue
      }
      i++; continue
    }
    out = out c
    i++
  }
  return out
}

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

# The receiver of a call whose receiver sits on the previous line:
# the last identifier the previous line ended with.
function tail_of(text,   t) {
  t = text
  sub(/[ \t]*$/, "", t)
  if (!match(t, /[A-Za-z0-9_]+$/)) { return "" }
  return substr(t, RSTART, RLENGTH)
}

# A session sink: a value whose name ends in `sink`, or `session_store`.
function is_sink(name) {
  if (name == "") { return 0 }
  if (name == "session_store") { return 1 }
  return name ~ /sink$/
}

FNR == 1 {
  depth = 0; skip_depth = -1; pending = 0; prev_tail = ""
  in_block = 0
}

{
  code = code_only($0)
  delta = braces(code)

  # Inside an inline test module: count the braces until the block
  # closes, and scan nothing.
  if (skip_depth >= 0) {
    depth += delta
    if (depth <= skip_depth) { skip_depth = -1 }
    prev_tail = ""
    next
  }

  opened = 0
  if (code ~ /#\[cfg\(test\)\]/) {
    rest = code
    sub(/#\[cfg\(test\)\]/, "", rest)
    if (rest ~ /mod[ \t]+[A-Za-z0-9_]+[ \t]*\{/) {
      skip_depth = depth
      opened = 1
    } else if (rest !~ /mod[ \t]+[A-Za-z0-9_]+[ \t]*;/) {
      pending = 1
    }
  } else if (pending) {
    if (code ~ /^[ \t]*#\[/) {
      prev_tail = tail_of(code)
      next
    }
    if (code ~ /mod[ \t]+[A-Za-z0-9_]+[ \t]*\{/) {
      skip_depth = depth
      opened = 1
    } else if (code ~ /mod[ \t]+[A-Za-z0-9_]+[ \t]*;/) {
      pending = 0
    } else if (code !~ /^[ \t]*$/) {
      pending = 0
    }
  }

  if (!opened) {
    # `append_many` — the writer has no such method.
    if (code ~ /\.append_many[ \t]*\(/) {
      printf "%s:%d: append_many outside the session writer\n", FILENAME, FNR
    }
    # `append` on a session sink.
    receiver = ""
    if (match(code, /^[ \t]*\.append[ \t]*\(/)) {
      receiver = prev_tail
    } else if (match(code, /[A-Za-z0-9_]+[ \t]*\.[ \t]*append[ \t]*\(/)) {
      receiver = substr(code, RSTART, RLENGTH)
      sub(/[ \t]*\.[ \t]*append[ \t]*\($/, "", receiver)
      gsub(/[ \t]/, "", receiver)
    }
    if (is_sink(receiver)) {
      printf "%s:%d: %s.append outside the session writer\n", FILENAME, FNR, receiver
    }
  }

  depth += delta
  prev_tail = tail_of(code)
}
')

if [[ -n "$report" ]]; then
  echo "FAIL: a session's rows must go through its one writer:"
  printf '%s\n' "$report" | sed 's/^/  /'
  echo "Every row a session gains goes through the session's LogWriter"
  echo "($WRITER_DIR) — the sink's append/append_many are the writer's"
  echo "own business. Route the row through the writer instead."
  exit 1
fi

echo "OK: no session sink is written outside $WRITER_DIR ($test_modules modules skipped as test code)"
