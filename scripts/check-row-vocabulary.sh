#!/usr/bin/env bash
# Row-vocabulary gate: production code may not spell a row tag itself.
#
# Why this exists: a session row's `type` is a closed vocabulary owned
# by `uva-session` (`RowTag` / `RowDialect` / `classify_row`). The
# spellings are declared once — `RowTag::as_wire` / `from_wire` and
# `RowDialect`'s twin — so a reader asks the vocabulary «what is this
# row» instead of matching the string. A hand-spelled tag is a second
# declaration: it compiles, it tests green, and the first row the
# vocabulary renames (or the first new tag a consumer forgets) drifts
# silently. ADR-0006: 「生产代码不得自己认 tag 字符串」.
#
# What the gate looks at: any identifier-shaped string literal — in
# any position — whose value is one of the 33 typed tags or the 4
# legacy dialects. No position rules, on purpose: the position
# grammar the gate used to carry had holes a reader could walk a tag
# through (`const ROW: &str = "<tag>";`, `let t = "<tag>";`,
# `foo("<tag>")`, `assert_eq!(row["type"], "<tag>")`), and a rule
# that must enumerate every place a string can sit is a rule nobody
# can keep closed. Any bare tag is a violation; the vocabulary is the
# only place a spelling may live.
#
# A spelling that is merely *equal* to a tag but is not a row tag —
# a config key, a payload key, an operation kind, a provider
# content-block kind, a display label — is listed in the audited
# table below, one `path|literal` pair per line with a reason. The
# table ratchets: a listed pair that no longer appears fails the gate
# until the entry is deleted, so a migrated site cannot leave a stale
# licence behind. Listing a literal is the *only* way past the gate,
# which makes every remaining hole visible in this file instead of
# distributed through a regex nobody reads.
#
# Three documented scope exclusions, and no more:
#   - `crates/uva-session/src/events/vocab/mod.rs` is excluded per
#     file: it is where the spellings are *declared* (`as_wire` /
#     `from_wire`), so every tag appears there by definition.
#   - `crates/uva-session/src/typed_event_builders.rs` is excluded per
#     file: it is where every row's shape is *written* (the
#     constructors), so every payload key and every variant literal
#     appears there by definition.
#   - `crates/uva-test-support` is excluded per crate: it is the test
#     surface, and its `src/` reads recorded `events.jsonl` fixtures
#     (a replay parser), so a tag there is fixture data. The crate
#     does not — and should not — depend on `uva-session` just to
#     compare a recorded tag.
#
# Two rules about the row's *body*, over the same production set:
#
#   - The construction rule: a `SessionEvent::X {` literal at a
#     construction site is a second owner of the row's shape. The
#     scope is the **whole workspace**, `uva-session` included: the
#     ruling (ADR-0015) is that the row's shape has one owner, and a
#     crate that owns the shape is not a crate that may spell it
#     anywhere else. (The read side — patterns — is not restricted:
#     a literal whose closing brace is followed by `=>` is a match arm
#     over restored rows, not a construction.)
#   - The payload rule: an index-assignment into a JSON bag named
#     `data` or `payload` is a row's payload being rewritten after
#     its constructor wrote it. A row's payload and its typed fields
#     are one pair, written by one constructor (ADR-0015 / the
#     row-payload-shape decision); a caller that patches
#     `data["message"] = …` after the fact is the shape drift the
#     owner exists to prevent, and it is invisible to the reader
#     until the two halves disagree. The scope is the **whole
#     workspace** too: the drift grew inside `uva-session` (the
#     repairer, the compactor), so a crate-level exemption would
#     leave the drift where it grew. Only the constructor module —
#     the file that writes each row's payload on purpose — is out of
#     the rule's reach, and it is out of it per file.
#
# Two files own a row's shape and are therefore the two places a
# variant literal may be spelled: the constructor module (every row's
# payload and every variant literal is written there on purpose) and
# the vocabulary's tag↔event table (the correspondence between a tag
# and its event is the point of the file). Both are exempted per file
# in the audited table below, and the exemption for the constructor
# module covers every spelling in it — the payload keys it writes are
# not row tags.
#
# The payload rule is a shape rule, so it names the bags the row
# vocabulary uses (`data`, `payload`) and not every JSON bag in the
# tree, and it names one write shape — an index-assignment into the
# bag. The shapes it does not cover are listed here, not in the
# audited table: the table ratchets the *hits* the rule produces, so
# a write the rule never matches cannot be recorded there — a hole is
# not a hit, and the table's ratchet would call such an entry stale.
# The known holes:
#
#   - a write through a field (`event.data["x"] = …`): the bag is
#     reached through a struct field, and the shape cannot tell that
#     field's bag from any other field's bag;
#   - a whole-bag replacement (`data = json!(…)`): the write is an
#     assignment to the bag, not an index into it;
#   - a key inserted through the JSON API (`data.insert("x", v)`,
#     `payload.as_object_mut()…insert(…)`): the write goes through a
#     method, not through an index;
#   - a bag under another name (`row["message"] = …`): the rule names
#     the bags the vocabulary uses.
#
# Covering the first three is not free: the sites they would catch
# are real payload writes the vocabulary itself performs (the
# free-form `tool_output` bag in `crates/uva-session/src/events/`),
# so each shape would need an audited entry — the exception table
# would grow instead of shrink. The rule stays a shape rule until
# that cost is worth paying.
#
# Where it looks: `crates/*/src/**/*.rs` — production code only. Test
# code is skipped: inline `#[cfg(test)] mod` blocks for their whole
# extent, `tests/` directories, `tests.rs`, and the files a
# `#[cfg(test)] mod NAME;` declaration pulls in (`NAME.rs`, or the
# `NAME/` directory) — the same rule `check-single-writer` applies.
#
# The browser's source is scanned too — `uva-web/src/**/*.{ts,tsx}`,
# production code only (`uva-web/tests` is not scanned), by
# `uva-web/scripts/check-row-vocabulary.mjs` over the TypeScript
# compiler API. The web half guards the **dialect spellings only**:
# the server publishes current rows, so a dialect name in the
# browser's source is a reader recognising a shape the wire no longer
# carries. The 33 current tags stay legal there — the browser's row
# table is a `Record` over the generated union and `tsc` already
# forces it to account for every tag. Web hits flow through the same
# audited table below and the same ratchet.
#
# Exit 0 with an `OK:` line, or exit 1 with every offending site.
# `--print` lists every hit with its disposition (auditing the table);
# `--self-test` proves the detector still fires on a fixture.

set -euo pipefail

cd "$(dirname "$0")/.."

VOCAB_SOURCE="crates/uva-session/src/events/vocab/mod.rs"

# The one crate-shaped scope exclusion (see the header): `uva-test-support`
# is the test surface, and its `src/` parses recorded `events.jsonl`
# fixtures, so a tag there is fixture data rather than a reader
# declaring the vocabulary a second time.
SCAN_EXCLUDED_CRATES=(
  crates/uva-test-support
)

TAGS=(
  user_message
  assistant_message
  assistant_chunk
  tool_result
  compaction
  compaction_start
  compaction_summary
  compaction_end
  tool_call
  tool_output
  step
  turn
  iteration
  warning
  steering_guard
  steering_hint
  hook_block
  subagent_enter
  subagent_exit
  request_header
  approval_asked
  approval_decided
  approval_granted
  tool_dispatch
  request_snapshot
  usage
  sandbox_mode
  session_shutdown
  tool_result_transformed
  parked_prompt
  park_consumed
  feedback
  plugin
)

DIALECTS=(UserInput Model Message Agent)

if [[ ${#TAGS[@]} -ne 33 || ${#DIALECTS[@]} -ne 4 ]]; then
  echo "FAIL: the gate's vocabulary must be 33 tags + 4 dialects" >&2
  exit 1
fi

# Every spelling the gate knows must still exist in the vocabulary
# module; a renamed tag left in this list would silently stop being
# guarded.
stale_vocab=()
for tag in "${TAGS[@]}" "${DIALECTS[@]}"; do
  if ! grep -qF "\"$tag\"" "$VOCAB_SOURCE"; then
    stale_vocab+=("$tag")
  fi
done
if [[ ${#stale_vocab[@]} -gt 0 ]]; then
  echo "FAIL: the gate's vocabulary is stale — not found in $VOCAB_SOURCE:"
  printf '  %s\n' "${stale_vocab[@]}"
  echo "Update the list in this script (and the exemption table) with the rename."
  exit 1
fi

VOCAB=" ${TAGS[*]} ${DIALECTS[*]} "

# The audited exception table. One entry per line:
#
#   path|literal|reason
#
# `literal` may be `*` to exempt every spelling in one file. Two
# files need it, and both are the ones that own a row's shape: the
# vocabulary module (where the spellings are the point) and the
# constructor module (where every row's payload key and every variant
# literal is written on purpose). Do not add a directory-shaped
# exemption: the gate is deliberately per file + per spelling. Every
# entry means "this spelling is not a session row tag in this file";
# a spelling that *is* a row tag has no entry — the code asks the
# vocabulary instead. An entry licenses every occurrence of that
# spelling in that file, so keep the table as short as the tree
# allows: fix a real row-tag spelling, never exempt it.
EXEMPTIONS='
crates/uva-app-server/src/api/operation.rs|compaction|an OperationKind, not a session row tag
crates/uva-app-server/src/routes/chat/stream.rs|tool_call|a chat/turn/event frame kind, not a session row tag
crates/uva-app-server/src/routes/chat/stream.rs|tool_result|a chat/turn/event frame kind, not a session row tag
crates/uva-app-server/src/routes/chat/stream.rs|usage|a chat/turn/event frame kind, not a session row tag
crates/uva-app-server/src/routes/chat/stream.rs|warning|a chat/turn/event frame kind, not a session row tag
crates/uva-app-server/src/state/boot.rs|compaction|a run-config key, not a session row tag
crates/uva-context/src/memory/file/entry.rs|feedback|a MemoryKind, not a session row tag
crates/uva-events/src/events/mod.rs|Model|an AgentEvent kind, not a session row tag
crates/uva-provider/src/anthropic/types.rs|tool_result|a provider content-block kind, not a session row tag
crates/uva-provider/src/openai/types/mod.rs|tool_call|a provider content-block kind, not a session row tag
crates/uva-provider/src/openai/types/mod.rs|tool_result|a provider content-block kind, not a session row tag
crates/uva-session/src/compaction_plugin.rs|compaction|the plugin key (KEY_COMPACTION), not a session row tag
crates/uva-session/src/events/mod.rs|step|a payload field name read by normalize(), not a row tag
crates/uva-session/src/events/mod.rs|turn|a payload field name read by normalize(), not a row tag
crates/uva-session/src/events/vocab/mod.rs|*|the vocabulary module: RowTag::as_wire/from_wire and RowDialect spell every tag here on purpose
crates/uva-session/src/content_blocks.rs|tool_result|a provider content-block kind, not a session row tag
crates/uva-session/src/typed_event_builders.rs|*|the constructor module: every row payload key and every variant literal is written here on purpose
crates/uva-tool-task/src/control/artifacts.rs|warning|the fallback label of a warning notice, not a row tag
crates/uva-tui/src/app.rs|warning|the fallback label of a warning notice, not a row tag
uva-web/src/components/ui/visual-utils.ts|Agent|a UI persona label, not a session row dialect
uva-web/src/settings/sections/AutomationSection.tsx|Agent|a settings label, not a session row dialect
crates/uva-app-server/src/api/contract.rs|Message|the provider message type catalogued by name, not a session row dialect
'

declare -A EXEMPT=()
declare -A EXEMPT_REASON=()
declare -A USED=()

while IFS='|' read -r path literal reason; do
  [[ -z "$path" || "$path" == \#* ]] && continue
  key="$path|$literal"
  if [[ -n "${EXEMPT[$key]+x}" ]]; then
    echo "FAIL: duplicate exemption entry for $key" >&2
    exit 1
  fi
  EXEMPT["$key"]=1
  EXEMPT_REASON["$key"]="$reason"
done <<< "$EXEMPTIONS"

# The scan itself. Comments are dropped and string literals are
# replaced by `@N@` markers with their values kept beside them, so a
# doc comment that mentions a tag is not a tag. Raw strings are read
# to their closing `"` + `#` run; a raw string that is the whole line
# is still one literal, so a tag spelled inside a JSON fixture is not
# a tag spelling. (The markers are no longer read for positions —
# every literal is judged on its value alone — but the value table
# and the comment stripping are still what makes the scan a scan of
# code rather than of bytes.)
SCAN_AWK='
function hashes(n) { return substr("################", 1, n) }

function norm_line(text,   out, i, n, c, rest, tok, h, body, endpos, val, ch, j) {
  out = ""
  NL_BASE = lit_idx
  i = 1; n = length(text)
  while (i <= n) {
    c = substr(text, i, 1)
    if (raw >= 0) {
      if (c == "\"" && substr(text, i + 1, raw) == hashes(raw)) {
        i += 1 + raw; raw = -1; continue
      }
      i++; continue
    }
    if (in_block) {
      if (c == "*" && substr(text, i + 1, 1) == "/") {
        in_block = 0; i += 2; continue
      }
      i++; continue
    }
    if (c == "/" && substr(text, i + 1, 1) == "/") { break }
    if (c == "/" && substr(text, i + 1, 1) == "*") {
      in_block = 1; i += 2; continue
    }
    rest = substr(text, i)
    if (match(rest, /^(br|rb|r)#*"/)) {
      tok = substr(rest, 1, RLENGTH)
      h = RLENGTH - 2
      if (tok ~ /^br/ || tok ~ /^rb/) { h = RLENGTH - 3 }
      body = substr(rest, RLENGTH + 1)
      endpos = index(body, "\"" hashes(h))
      out = out "@" lit_idx "@"
      if (endpos > 0) {
        LVAL[lit_idx] = substr(body, 1, endpos - 1)
        i += RLENGTH + (endpos - 1) + 1 + h
      } else {
        LVAL[lit_idx] = "<raw-multiline>"
        raw = h
        i = n + 1
      }
      lit_idx++
      continue
    }
    if (c == "\"") {
      val = ""
      j = i + 1
      while (j <= n) {
        ch = substr(text, j, 1)
        if (ch == "\\") { val = val substr(text, j, 2); j += 2; continue }
        if (ch == "\"") { break }
        val = val ch
        j++
      }
      out = out "@" lit_idx "@"
      LVAL[lit_idx] = val
      lit_idx++
      i = j + 1
      continue
    }
    if (c == "\047") {
      if (substr(text, i + 1, 1) == "\\" && substr(text, i + 3, 1) == "\047") {
        out = out " "; i += 4; continue
      }
      if (substr(text, i + 1, 1) != "\\" && substr(text, i + 2, 1) == "\047") {
        out = out " "; i += 3; continue
      }
      out = out c
      i++
      continue
    }
    out = out c
    i++
  }
  NORM = out
}

FNR == 1 {
  in_block = 0; raw = -1; lit_idx = 0; NL_BASE = 0
  depth = 0; skip_depth = -1; pending = 0
  PEND_N = 0
}

# The text after the brace that closes a literal: the `=>` of a match
# arm sits there, and nothing else does. The closing brace is the one
# that balances the opening one, so the walk starts at the opening
# brace of the literal and stops at the brace that closes it.
function closing_tail(text,   i, n, c, depth) {
  n = length(text)
  depth = 0
  for (i = 1; i <= n; i++) {
    c = substr(text, i, 1)
    if (c == "{") {
      depth++
    } else if (c == "}") {
      depth--
      if (depth == 0) {
        return substr(text, i + 1)
      }
    }
  }
  return ""
}

# Walk the braces of a line in order. A literal held back from an
# earlier line closes on the brace that balances it, and the text
# after that brace says whether the literal was a match arm (`=>`) or
# the end of a construction.
function walk_braces(text,   i, n, c, tail) {
  n = length(text)
  for (i = 1; i <= n && PEND_N > 0; i++) {
    c = substr(text, i, 1)
    if (c == "{") {
      PEND_BALANCE[PEND_N]++
    } else if (c == "}") {
      PEND_BALANCE[PEND_N]--
      if (PEND_BALANCE[PEND_N] == 0) {
        tail = substr(text, i + 1)
        if (tail !~ /=>/) {
          printf "%s\t%d\t%s\n", FILENAME, PEND_LINE[PEND_N], \
            PEND_VARIANT[PEND_N]
        }
        PEND_N--
      }
    }
  }
}

{
  norm_line($0)
  code = NORM

  tmp = code
  delta = gsub(/\{/, "", tmp)
  tmp = code
  delta -= gsub(/\}/, "", tmp)

  # The literals held back from earlier lines close on this one, if
  # this one closes them.
  walk_braces(code)

  if (skip_depth >= 0) {
    depth += delta
    if (depth <= skip_depth) { skip_depth = -1 }
    next
  }

  opened = 0
  if (code ~ /#\[cfg\(test\)\]/) {
    rest = code
    sub(/^.*#\[cfg\(test\)\]/, "", rest)
    if (rest ~ /mod[ \t]+[A-Za-z0-9_]+[ \t]*\{/) {
      skip_depth = depth; opened = 1
    } else if (rest !~ /mod[ \t]+[A-Za-z0-9_]+[ \t]*;/) {
      pending = 1
    }
  } else if (pending) {
    if (code ~ /^[ \t]*#\[/) { next }
    if (code ~ /mod[ \t]+[A-Za-z0-9_]+[ \t]*\{/) {
      skip_depth = depth; opened = 1; pending = 0
    } else if (code ~ /mod[ \t]+[A-Za-z0-9_]+[ \t]*;/) {
      pending = 0
    } else if (code !~ /^[ \t]*$/) {
      pending = 0
    }
  }

  if (!opened) {
    if (!ctor && !payload) for (k = NL_BASE; k < lit_idx; k++) {
      val = LVAL[k]
      if (val !~ /^[A-Za-z_][A-Za-z0-9_]*$/) { continue }
      if (index(vocab, " " val " ") == 0) { continue }
      printf "%s\t%d\t%s\n", FILENAME, FNR, val
    }

    # Construction half (only when asked): a `SessionEvent::X {`
    # whose line carries no pattern markers (`..`, `matches!`) and
    # whose prefix — path segments stripped — does not end in a
    # pattern introducer (`let `, `Some(`, `match`, `|`, `@`) is
    # construction-shaped: the row shape is being spelled at the
    # call site instead of coming from a builder.
    #
    # A pattern is not a construction, and the `=>` of the arm is
    # what says so — the `=>` that *closes* the literal, not one that
    # happens to sit on the opening line. A construction inside the
    # body of a match arm (`_ => SessionEvent::Warning { … }`)
    # carries the `=>` of the arm on its own line, so an
    # opening-line `=>` says nothing about the literal. The rule is
    # therefore decided on the **closing** line: the text after the
    # brace that closes the literal. If that text says `=>`, the
    # literal was a match arm over restored rows, and the read side
    # is not restricted; otherwise the literal was written.
    #
    # A literal whose braces do not close on its own line is held
    # back until the line that closes it (the pending mechanism
    # below), so a multi-line arm and a multi-line construction are
    # judged by the same line.
    if (ctor && code ~ /SessionEvent::[A-Za-z0-9_]+[ \t]*\{/) {
      if (code !~ /\.\./ && code !~ /matches!/) {
        pre = code; sub(/SessionEvent::.*$/, "", pre)
        while (pre ~ /[A-Za-z0-9_]+::[ \t]*$/) {
          sub(/[A-Za-z0-9_]+::[ \t]*$/, "", pre)
        }
        if (pre !~ /(let[ \t]+|Some\(|match[ \t]*[({]|[ \t]*\||@[ \t]*)[ \t]*$/) {
          match(code, /SessionEvent::[A-Za-z0-9_]+/)
          variant = substr(code, RSTART + 14, RLENGTH - 14)
          after = substr(code, RSTART)
          lit = substr(after, index(after, "{"))
          opens = gsub(/\{/, "", lit)
          closes = gsub(/\}/, "", lit)
          balance = opens - closes
          if (balance <= 0) {
            # The literal closes on this line: the text after its
            # closing brace says whether it was a match arm.
            if (closing_tail(lit) !~ /=>/) {
              printf "%s\t%d\t%s\n", FILENAME, FNR, variant
            }
          } else {
            # It closes on a later line: hold it until then.
            PEND_N++
            PEND_LINE[PEND_N] = FNR
            PEND_VARIANT[PEND_N] = variant
            PEND_BALANCE[PEND_N] = balance
          }
        }
      }
    }

    # Payload half (only when asked): an index-assignment into a
    # JSON bag named `data` or `payload` — the payload of a row
    # rewritten after its constructor wrote it. The bag must not be
    # reached through a field (`page.data[...]` is some other bag),
    # and the assignment must be a write, not a comparison (`==`).
    if (payload) {
      if (match(code, /(^|[^A-Za-z0-9_.])data[ \t]*\[[^]]*\][ \t]*=/)) {
        if (substr(code, RSTART + RLENGTH, 1) != "=") {
          printf "%s\t%d\tdata\n", FILENAME, FNR
        }
      } else if (match(code, /(^|[^A-Za-z0-9_.])payload[ \t]*\[[^]]*\][ \t]*=/)) {
        if (substr(code, RSTART + RLENGTH, 1) != "=") {
          printf "%s\t%d\tpayload\n", FILENAME, FNR
        }
      }
    }
  }

  depth += delta

}
'

scan_paths() {
  printf '%s\n' "$@" | xargs awk -v vocab="$VOCAB" "$SCAN_AWK"
}

# The construction half scans the same production set as the whole
# workspace: the ruling is that a row's shape has one owner, and the
# crate that owns it is not a crate that may spell it anywhere else.
# The two files that own the shape — the constructor module and the
# vocabulary's tag↔event table — are exempted per file in the
# audited table below, and the table's own filter drops their hits.
scan_constructors() {
  if [[ $# -eq 0 ]]; then return; fi
  printf '%s\n' "$@" | xargs awk -v vocab="$VOCAB" -v ctor=1 "$SCAN_AWK"
}
# The payload half scans the same production set minus the one file
# that owns a row's payload: the constructor module writes each row's
# payload on purpose, so it is out of the rule's reach per file. Every
# other file is in scope — the rest of `uva-session` included, because
# a payload patched after the fact is a second owner of the shape
# wherever it is written, and the drift grew inside the crate.
scan_payload_writes() {
  local files=() file
  for file in "$@"; do
    [[ "$file" == crates/uva-session/src/typed_event_builders.rs ]] && continue
    files+=("$file")
  done
  if [[ ${#files[@]} -eq 0 ]]; then return; fi
  printf '%s\n' "${files[@]}" | xargs awk -v vocab="$VOCAB" -v payload=1 \
    "$SCAN_AWK"
}
# The production files: a crate's `src/` tree, minus test code.
production_files() {
  local all_files=() file dir stem parent skip name excluded crate_root
  mapfile -t all_files < <(
    find crates -name '*.rs' -path '*/src/*' | sort
  )
  if [[ ${#all_files[@]} -eq 0 ]]; then
    echo "FAIL: no source files under crates/*/src — the gate would pass vacuously" >&2
    exit 1
  fi

  # Pass 1: the modules a `#[cfg(test)] mod NAME;` declaration pulls
  # in — their file (`NAME.rs`) or directory (`NAME/`) is test code.
  mapfile -t test_modules < <(
    printf '%s\n' "${all_files[@]}" | xargs awk '
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
        if ($0 ~ /^[ \t]*#\[/) { next }
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
    ' | sort -u
  )

  # Pass 2: the production files that are not test code.
  for file in "${all_files[@]}"; do
    excluded=0
    for crate_root in "${SCAN_EXCLUDED_CRATES[@]}"; do
      if [[ "$file" == "$crate_root"/* ]]; then
        excluded=1
        break
      fi
    done
    [[ "$excluded" -eq 1 ]] && continue
    case "$file" in
      */tests/*) continue ;;
    esac
    stem="${file##*/}"
    stem="${stem%.rs}"
    skip=0
    for name in "${test_modules[@]}"; do
      if [[ "$stem" == "$name" ]]; then
        skip=1
        break
      fi
    done
    dir="${file%/*}"
    while [[ "$skip" -eq 0 && "$dir" == */* ]]; do
      parent="${dir##*/}"
      for name in "${test_modules[@]}"; do
        if [[ "$parent" == "$name" ]]; then
          skip=1
          break 2
        fi
      done
      dir="${dir%/*}"
    done
    [[ "$skip" -eq 1 ]] && continue
    printf '%s\n' "$file"
  done
}

# Record which exemption (if any) covers `file` + `literal`, and set
# EX_KEY to the matching table key.
is_exempt() {
  local file="$1" literal="$2"
  EX_KEY=""
  if [[ -n "${EXEMPT["$file|$literal"]+x}" ]]; then
    EX_KEY="$file|$literal"
    return 0
  fi
  if [[ -n "${EXEMPT["$file|*"]+x}" ]]; then
    EX_KEY="$file|*"
    return 0
  fi
  return 1
}

# --- Self-test: the detector still has teeth -----------------------
if [[ "${1:-}" == "--self-test" ]]; then
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  cat > "$tmp/fixture.rs" <<'RS'
fn probe(row: &serde_json::Value, tag: &str) {
    if row.get("type").and_then(|v| v.as_str()) == Some("user_message") {}
    match tag {
        "tool_result" => {}
        _ => {}
    }
    let _ = serde_json::json!({ "type": "assistant_message" });
    let _ = serde_json::json!("parked_prompt");
    let _ = row.get("type").and_then(|v| v.as_str()) != Some("warning");

    // "user_message" in a line comment is not code
    /* "tool_result" in a block comment is not code */
    let _ = serde_json::json!({ "turn": 1, "usage": null });
    let _ = r#"{"type":"user_message"}"#;

    // The shapes the old position grammar named no rule for: a named
    // constant, a local binding, a call argument, a macro argument.
    const ROW: &str = "user_message";
    let t = "turn";
    foo("plugin");
    assert_eq!(row["type"], "warning");

    #[cfg(test)]
    mod tests {
        fn probe() { let _ = serde_json::json!({ "type": "user_message" }); }
    }
}
RS
  expected=$'2\tuser_message\n4\ttool_result\n7\tassistant_message\n8\tparked_prompt\n9\twarning\n13\tturn\n13\tusage\n18\tuser_message\n19\tturn\n20\tplugin\n21\twarning'
  got=$(scan_paths "$tmp/fixture.rs" | cut -f2-)
  if [[ "$got" != "$expected" ]]; then
    echo "FAIL: --self-test: the detector no longer reports the fixture's bare tags" >&2
    printf 'expected:\n%s\nactual:\n%s\n' "$expected" "$got" >&2
    exit 1
  fi

  # The exemption table filters by file + literal, and a hit outside
  # the table is a violation.
  EXEMPT["$tmp/fixture.rs|warning"]=1
  EXEMPT_REASON["$tmp/fixture.rs|warning"]="self-test"
  if ! is_exempt "$tmp/fixture.rs" "warning"; then
    echo "FAIL: --self-test: an audited exemption did not filter its hit" >&2
    exit 1
  fi
  if is_exempt "$tmp/fixture.rs" "user_message"; then
    echo "FAIL: --self-test: an unaudited hit was filtered" >&2
    exit 1
  fi
  if is_exempt "crates/elsewhere.rs" "warning"; then
    echo "FAIL: --self-test: an exemption leaked to another file" >&2
    exit 1
  fi

  # The web detector proves its own teeth: a dialect spelling in a
  # fixture `.ts` is reported with its line, a comment is not, and a
  # current tag is not (the web half guards dialects only).
  mkdir "$tmp/web"
  cat > "$tmp/web/probe.ts" <<'TS'
const a = 'UserInput';
// 'Model' in a comment is not code
const c = 'Agent';
const ok = 'user_message';
TS
  web_got=$(node uva-web/scripts/check-row-vocabulary.mjs \
    --root "$tmp/web" UserInput Model Message Agent \
    | cut -f2-)
  web_expected=$'1\tUserInput\n3\tAgent'
  if [[ "$web_got" != "$web_expected" ]]; then
    echo "FAIL: --self-test: the web detector no longer reports the fixture's dialect spellings" >&2
    printf 'expected:\n%s\nactual:\n%s\n' "$web_expected" "$web_got" >&2
    exit 1
  fi

  # The construction detector: a variant literal at a construction
  # site is reported; the pattern shapes read models use are not.
  cat > "$tmp/ctor_fixture.rs" <<'RS'
fn write(event: &mut uva_session::SessionEvent) {
    let e = uva_session::SessionEvent::Warning {
        seq: 0,
        ts: String::new(),
        data: serde_json::Value::Null,
    };
    emit_typed(uva_session::SessionEvent::Feedback {
        seq: 0,
        ts: String::new(),
        message_id: String::new(),
        thumbs_up: true,
    });
    let Some(uva_session::SessionEvent::Turn { .. }) = Some(event) else {
        return;
    };
    if let Some(uva_session::SessionEvent::Usage { .. }) = None {
    }
    match event {
        uva_session::SessionEvent::Step { .. } => {}
        _ => {}
    }
    let _ = matches!(event, uva_session::SessionEvent::Plugin { .. });
    let _ = e;
}
RS
  ctor_expected=$'2\tWarning\n7\tFeedback'
  ctor_got=$(scan_constructors "$tmp/ctor_fixture.rs" | cut -f2-)
  if [[ "$ctor_got" != "$ctor_expected" ]]; then
    echo "FAIL: --self-test: the construction detector lost its teeth" >&2
    printf 'expected:\n%s\nactual:\n%s\n' "$ctor_expected" "$ctor_got" >&2
    exit 1
  fi

  # The construction detector's scope: the whole workspace, the row
  # owner's own crate included. The probe below lives under a crate's
  # `src/` tree — the shape the old crate-wide skip used to hide —
  # and its multi-line construction is reported.
  mkdir -p "$tmp/fakerepo/crates/uva-session/src"
  cat > "$tmp/fakerepo/crates/uva-session/src/probe.rs" <<'RS'
fn write() {
    let _ = uva_session::SessionEvent::Warning {
        seq: 0,
        ts: String::new(),
        data: serde_json::Value::Null,
    };
}
RS
  scope_got=$(cd "$tmp/fakerepo" \
    && scan_constructors "crates/uva-session/src/probe.rs" | cut -f2-)
  if [[ "$scope_got" != $'2\tWarning' ]]; then
    echo "FAIL: --self-test: the construction detector no longer scans the owner's own crate" >&2
    printf 'expected:\n2\tWarning\nactual:\n%s\n' "$scope_got" >&2
    exit 1
  fi

  # A multi-line match arm is a pattern, not a construction: the read
  # side is not restricted, and the markers of the arm sit on the
  # lines the literal does not open on. A guarded arm is a pattern
  # too. The multi-line construction in the same fixture is still
  # reported.
  cat > "$tmp/arm_fixture.rs" <<'RS'
fn read(event: &uva_session::SessionEvent) {
    match event {
        uva_session::SessionEvent::Turn {
            kind,
            turn,
            ..
        } => {
            let _ = (kind, turn);
        }
        uva_session::SessionEvent::Usage {
            usage,
            ..
        } if usage.is_some() => {}
        _ => {}
    }
    let _ = uva_session::SessionEvent::Warning {
        seq: 0,
        ts: String::new(),
        data: serde_json::Value::Null,
    };
}
RS
  arm_expected=$'16\tWarning'
  arm_got=$(scan_constructors "$tmp/arm_fixture.rs" | cut -f2-)
  if [[ "$arm_got" != "$arm_expected" ]]; then
    echo "FAIL: --self-test: the construction detector no longer tells a multi-line match arm from a construction" >&2
    printf 'expected:\n%s\nactual:\n%s\n' "$arm_expected" "$arm_got" >&2
    exit 1
  fi

  # A construction inside the body of a match arm: the arm's `=>`
  # sits on the line the literal opens on, and the literal that
  # follows it is written, not read. The verdict comes from the
  # closing line — the text after the brace that closes the literal
  # — so the opening line's `=>` does not hide the write, whether
  # the literal closes on its own line or a later one.
  cat > "$tmp/arm_body_fixture.rs" <<'RS'
fn write(tag: &str) -> uva_session::SessionEvent {
    match tag {
        "multi-line" => uva_session::SessionEvent::Warning {
            seq: 0,
            ts: String::new(),
            data: serde_json::Value::Null,
        },
        _ => uva_session::SessionEvent::Feedback { message_id: String::new(), thumbs_up: true },
    }
}
RS
  arm_body_expected=$'3\tWarning\n8\tFeedback'
  arm_body_got=$(scan_constructors "$tmp/arm_body_fixture.rs" | cut -f2-)
  if [[ "$arm_body_got" != "$arm_body_expected" ]]; then
    echo "FAIL: --self-test: the construction detector no longer catches a construction inside a match arm's body" >&2
    printf 'expected:\n%s\nactual:\n%s\n' "$arm_body_expected" "$arm_body_got" >&2
    exit 1
  fi

  # The two files that own a row's shape are the two places a variant
  # literal may be spelled: the constructor module and the
  # vocabulary's tag↔event table.
  for owner in \
    crates/uva-session/src/typed_event_builders.rs \
    crates/uva-session/src/events/vocab/mod.rs; do
    if ! is_exempt "$owner" "Warning"; then
      echo "FAIL: --self-test: the owner file $owner is not exempt" >&2
      exit 1
    fi
  done

  # The payload detector: a row's payload rewritten after its
  # constructor wrote it is reported; a read, a comparison, another
  # bag and a field access are not.
  cat > "$tmp/payload_fixture.rs" <<'RS'
fn probe(row: &mut serde_json::Value, page: &mut Page) {
    let mut data = serde_json::Value::Null;
    data["message"] = serde_json::Value::String("boom".into());
    payload["kind"] = "turn";
    let read = data["kind"];
    other[0] = 1;
    page.data[0] = 2;
    if data["kind"] == "end" {}
}
RS
  payload_expected=$'3\tdata\n4\tpayload'
  payload_got=$(scan_payload_writes "$tmp/payload_fixture.rs" | cut -f2-)
  if [[ "$payload_got" != "$payload_expected" ]]; then
    echo "FAIL: --self-test: the payload detector lost its teeth" >&2
    printf 'expected:\n%s\nactual:\n%s\n' "$payload_expected" "$payload_got" >&2
    exit 1
  fi

  # The payload detector's scope: the whole workspace, the row
  # owner's own crate included. The constructor module is the one
  # file that writes a row's payload on purpose, so a payload
  # patched anywhere else — inside the owner's crate too — is the
  # second owner the rule exists to catch, and the crate-shaped
  # skip the probe used to carry would have hidden it.
  mkdir -p "$tmp/payloadrepo/crates/uva-session/src"
  cat > "$tmp/payloadrepo/crates/uva-session/src/probe.rs" <<'RS'
fn probe(data: &mut serde_json::Value) {
    data["message"] = serde_json::Value::String("boom".into());
}
RS
  cat > "$tmp/payloadrepo/crates/uva-session/src/typed_event_builders.rs" <<'RS'
fn probe(data: &mut serde_json::Value) {
    data["message"] = serde_json::Value::String("boom".into());
}
RS
  payload_scope_got=$(cd "$tmp/payloadrepo" \
    && scan_payload_writes "crates/uva-session/src/probe.rs" \
      "crates/uva-session/src/typed_event_builders.rs" | cut -f1-)
  payload_scope_expected=$'crates/uva-session/src/probe.rs\t2\tdata'
  if [[ "$payload_scope_got" != "$payload_scope_expected" ]]; then
    echo "FAIL: --self-test: the payload detector no longer scans the owner's own crate" >&2
    printf 'expected:\n%s\nactual:\n%s\n' "$payload_scope_expected" "$payload_scope_got" >&2
    exit 1
  fi

  echo "OK: --self-test — all four detectors report their fixture's spellings and the table filters per file + spelling"
  exit 0
fi
mapfile -t files < <(production_files)
if [[ ${#files[@]} -eq 0 ]]; then
  echo "FAIL: no production files left to scan — the gate would pass vacuously" >&2
  exit 1
fi

rust_hits=$(scan_paths "${files[@]}")
ctor_hits=$(scan_constructors "${files[@]}")
payload_hits=$(scan_payload_writes "${files[@]}")
rust_hits="${rust_hits}"$'\n'"${ctor_hits}"$'\n'"${payload_hits}"

# The web half: the dialect spellings in `uva-web/src`, reported by
# the TypeScript-compiler scanner with the same `file\tline\tliteral`
# shape, so the verdict, the audited table and the ratchet below are
# one mechanism for both languages.
web_hits=$(node uva-web/scripts/check-row-vocabulary.mjs \
  "${DIALECTS[@]}")
hits="${rust_hits}"$'\n'"${web_hits}"

violations=()
if [[ "${1:-}" == "--print" ]]; then
  printf '%-60s %-26s %s\n' "site" "literal" "disposition"
fi

while IFS=$'\t' read -r file line literal; do
  [[ -z "${file:-}" ]] && continue
  if is_exempt "$file" "$literal"; then
    USED["$EX_KEY"]=1
    if [[ "${1:-}" == "--print" ]]; then
      printf '%-60s %-26s %s\n' "$file:$line" "$literal" \
        "${EXEMPT_REASON[$EX_KEY]}"
    fi
    continue
  fi
  violations+=("$file:$line: \"$literal\"")
  if [[ "${1:-}" == "--print" ]]; then
    printf '%-60s %-26s %s\n' "$file:$line" "$literal" "VIOLATION"
  fi
done <<< "$hits"

# The table ratchets: an entry that no longer covers a hit must be
# deleted, so a migrated site cannot leave a stale licence behind.
stale=()
for key in "${!EXEMPT[@]}"; do
  if [[ -z "${USED[$key]+x}" ]]; then
    stale+=("$key — ${EXEMPT_REASON[$key]}")
  fi
done

if [[ ${#stale[@]} -gt 0 ]]; then
  echo "FAIL: stale row-vocabulary exemption(s) — the listed hit is gone:"
  printf '%s\n' "${stale[@]}" | sort | sed 's/^/  /'
  echo "Delete the entry from the table in this script."
  exit 1
fi

if [[ ${#violations[@]} -gt 0 ]]; then
  echo "FAIL: production code spells a session row tag, constructs a"
  echo "SessionEvent variant itself, or rewrites a row's payload:"
  printf '%s\n' "${violations[@]}" | sort | sed 's/^/  /'
  echo "A row's tag is declared once, in $VOCAB_SOURCE."
  echo "Read it through uva_session::{classify_row, RowTag, RowDialect};"
  echo "write it through the typed constructors in"
  echo "uva_session::typed_event_builders — a variant literal outside"
  echo "the two files that own a row's shape is a second owner of it,"
  echo "and so is a payload patched after its constructor wrote it:"
  echo "ask the constructor for the row you want. If the spelling is"
  echo "not a row tag, add an audited entry (path|literal|reason) to"
  echo "this script."
  exit 1
fi

echo "OK: no bare row tag, no construction-shaped SessionEvent literal outside the two files that own a row's shape and no payload write outside the constructor module in ${#files[@]} production files (37 spellings guarded; ${#EXEMPT[@]} audited non-tag spellings; ${SCAN_EXCLUDED_CRATES[*]} excluded as test infrastructure)"
