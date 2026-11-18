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
# Two documented scope exclusions, and no more:
#   - `crates/uva-session/src/events/vocab/mod.rs` is excluded per
#     file: it is where the spellings are *declared* (`as_wire` /
#     `from_wire`), so every tag appears there by definition.
#   - `crates/uva-test-support` is excluded per crate: it is the test
#     surface, and its `src/` reads recorded `events.jsonl` fixtures
#     (a replay parser), so a tag there is fixture data. The crate
#     does not — and should not — depend on `uva-session` just to
#     compare a recorded tag.
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
# `literal` may be `*` to exempt every tag spelled in one file (used
# only for the vocabulary module, where the spellings are the point).
# Do not add a directory-shaped exemption: the gate is deliberately
# per file + per spelling. Every entry means "this spelling is not a
# session row tag in this file"; a spelling that *is* a row tag has
# no entry — the code asks the vocabulary instead. An entry licenses
# every occurrence of that spelling in that file, so keep the table
# as short as the tree allows: fix a real row-tag spelling, never
# exempt it.
EXEMPTIONS='
crates/uva-approval/src/approval.rs|ApprovalGranted|read-side match arm over restored rows, not a construction
crates/uva-approval/src/approval.rs|ApprovalAsked|read-side match arm over restored rows, not a construction
crates/uva-approval/src/approval.rs|ApprovalDecided|read-side match arm over restored rows, not a construction
crates/uva-app-server/src/api/operation.rs|compaction|an OperationKind, not a session row tag
crates/uva-app-server/src/state/boot.rs|compaction|a run-config key, not a session row tag
crates/uva-context/src/memory/file/entry.rs|feedback|a MemoryKind, not a session row tag
crates/uva-events/src/events/mod.rs|Model|an AgentEvent kind, not a session row tag
crates/uva-policy-plugins/src/cot/mod.rs|step|a heading label in chain-of-thought text, not a session row tag
crates/uva-provider/src/anthropic/types.rs|tool_result|a provider content-block kind, not a session row tag
crates/uva-provider/src/openai/types/mod.rs|tool_call|a provider content-block kind, not a session row tag
crates/uva-provider/src/openai/types/mod.rs|tool_result|a provider content-block kind, not a session row tag
crates/uva-session/src/compaction/mod.rs|usage|a payload key inside the compaction row data, not a row tag
crates/uva-session/src/compaction_plugin.rs|compaction|the plugin key (KEY_COMPACTION), not a session row tag
crates/uva-session/src/events/mod.rs|step|a payload field name read by normalize(), not a row tag
crates/uva-session/src/events/mod.rs|turn|a payload field name read by normalize(), not a row tag
crates/uva-session/src/events/vocab/mod.rs|*|the vocabulary module: RowTag::as_wire/from_wire and RowDialect spell every tag here on purpose
crates/uva-session/src/repair/mod.rs|step|a payload key of the closer row data, not a row tag
crates/uva-session/src/repair/mod.rs|turn|a payload key of the closer row data, not a row tag
crates/uva-session/src/content_blocks.rs|tool_result|a provider content-block kind, not a session row tag
crates/uva-session/src/typed_event_builders.rs|iteration|a payload key of the event data, not a row tag
crates/uva-session/src/typed_event_builders.rs|step|a payload key of the event data, not a row tag
crates/uva-session/src/typed_event_builders.rs|turn|a payload key of the event data, not a row tag
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
}

{
  norm_line($0)
  code = NORM

  tmp = code
  delta = gsub(/\{/, "", tmp)
  tmp = code
  delta -= gsub(/\}/, "", tmp)

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
    if (!ctor) for (k = NL_BASE; k < lit_idx; k++) {
      val = LVAL[k]
      if (val !~ /^[A-Za-z_][A-Za-z0-9_]*$/) { continue }
      if (index(vocab, " " val " ") == 0) { continue }
      printf "%s\t%d\t%s\n", FILENAME, FNR, val
    }

    # Construction half (only when asked): a `SessionEvent::X {`
    # whose line carries no pattern markers (`..`, `=>`, `matches!`)
    # and whose prefix — path segments stripped — does not end in a
    # pattern introducer (`let `, `Some(`, `match`, `|`, `@`) is
    # construction-shaped: the row shape is being spelled at the
    # call site instead of coming from a builder. A bare multi-line
    # match arm has the same shape; those land in the audited table
    # below with a reason, same mechanism as a non-tag spelling.
    if (ctor && code ~ /SessionEvent::[A-Za-z0-9_]+[ \t]*\{/) {
      if (code !~ /\.\./ && code !~ /=>/ && code !~ /matches!/) {
        pre = code; sub(/SessionEvent::.*$/, "", pre)
        while (pre ~ /[A-Za-z0-9_]+::[ \t]*$/) {
          sub(/[A-Za-z0-9_]+::[ \t]*$/, "", pre)
        }
        if (pre !~ /(let[ \t]+|Some\(|match[ \t]*[({]|[ \t]*\||@[ \t]*)[ \t]*$/) {
          match(code, /SessionEvent::[A-Za-z0-9_]+/)
          printf "%s\t%d\t%s\n", FILENAME, FNR, \
            substr(code, RSTART + 14, RLENGTH - 14)
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

# The construction half scans the same production set minus the
# vocabulary owner: inside `uva-session` the enum, its constructors
# and its own folds may spell variants — everywhere else a variant
# literal at a construction site is a second owner of the row shape.
scan_constructors() {
  local files=() file
  for file in "$@"; do
    [[ "$file" == crates/uva-session/src/* ]] && continue
    files+=("$file")
  done
  if [[ ${#files[@]} -eq 0 ]]; then return; fi
  printf '%s\n' "${files[@]}" | xargs awk -v vocab="$VOCAB" -v ctor=1 "$SCAN_AWK"
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

  echo "OK: --self-test — all three detectors report their fixture's spellings and the table filters per file + spelling"
  exit 0
fi
mapfile -t files < <(production_files)
if [[ ${#files[@]} -eq 0 ]]; then
  echo "FAIL: no production files left to scan — the gate would pass vacuously" >&2
  exit 1
fi

rust_hits=$(scan_paths "${files[@]}")
ctor_hits=$(scan_constructors "${files[@]}")
rust_hits="${rust_hits}"$'\n'"${ctor_hits}"

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
  echo "FAIL: production code spells a session row tag or constructs a"
  echo "SessionEvent variant itself:"
  printf '%s\n' "${violations[@]}" | sort | sed 's/^/  /'
  echo "A row's tag is declared once, in $VOCAB_SOURCE."
  echo "Read it through uva_session::{classify_row, RowTag, RowDialect};"
  echo "write it through the typed constructors in"
  echo "uva_session::typed_event_builders — a variant literal outside"
  echo "uva-session is a second owner of the row shape. If the spelling"
  echo "is not a row tag, add an audited entry (path|literal|reason) to"
  echo "this script."
  exit 1
fi

echo "OK: no bare row tag and no construction-shaped SessionEvent literal in ${#files[@]} production files (37 spellings guarded; ${#EXEMPT[@]} audited non-tag spellings; ${SCAN_EXCLUDED_CRATES[*]} excluded as test infrastructure)"
