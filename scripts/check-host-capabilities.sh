#!/usr/bin/env bash
# Capability-matrix gate: the host matrix in
# `docs/host-capabilities.md` may only improve.
#
# The matrix is the *reverse view* of the resolved profile (D26): it
# says which capability domain reaches which host, and how well. This
# gate keeps that view honest:
#
# 1. every row carries a status — `full`, `adapted`, `n/a` or
#    `planned` — and nothing else;
# 2. a `planned` row must anchor itself: the reader has to learn what
#    would make it real, so an anchor (a path, a plan, an issue) is
#    mandatory;
# 3. an `n/a` row must say why the host does not apply;
# 4. a `full` / `adapted` row must cite evidence that is on disk — a
#    path that does not exist is not evidence;
# 5. every method the matrix names must exist in the server's
#    registry, so the matrix cannot invent a surface;
# 6. the gap count (`planned` + `n/a`) may never rise above the
#    baseline in `scripts/host-capabilities/baseline`.
#
# The baseline is the gap count at the moment this gate was
# introduced; raising it is a deliberate, separate commit — the same
# discipline as `check-plugin-shape`.
#
# Exit 0 with an `OK:` line, or exit 1 with the offending rows.

set -euo pipefail

cd "$(dirname "$0")/.."

MATRIX="${MATRIX:-docs/host-capabilities.md}"
BASELINE_FILE="scripts/host-capabilities/baseline"
METHOD_NAMES="crates/uva-app-server/src/rpc/methods/names.rs"
METHODS_DIR="crates/uva-app-server/src/rpc/methods"

if [[ ! -f "$MATRIX" ]]; then
  echo "the capability matrix is missing: $MATRIX" >&2
  exit 1
fi

# The status column: the second cell of every row of the host matrix
# table. The table starts at the `| 域 | ... |` header.
status_column() {
  awk -F'|' '
    /^\| *域 *\|/ { in_table = 1; next }
    in_table && /^\| *---/ { next }
    in_table && /^\|/ {
      cell = $3
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", cell)
      print cell
      next
    }
    in_table { exit }
  ' "$MATRIX"
}

# The description column (the third cell) of the same table.
descriptions_column() {
  awk -F'|' '
    /^\| *域 *\|/ { in_table = 1; next }
    in_table && /^\| *---/ { next }
    in_table && /^\|/ {
      cell = $4
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", cell)
      print cell
      next
    }
    in_table { exit }
  ' "$MATRIX"
}

# The evidence column (the last cell) of the same table.
evidence_column() {
  awk -F'|' '
    /^\| *域 *\|/ { in_table = 1; next }
    in_table && /^\| *---/ { next }
    in_table && /^\|/ {
      # the last content cell: the field before the empty one the
      # trailing pipe leaves behind
      cell = $(NF - 1)
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", cell)
      print cell
      next
    }
    in_table { exit }
  ' "$MATRIX"
}

# Every method name the server registers.
#
# The **registrations** are the truth: the router's `register` call
# sites are the methods the process answers, and the names table is
# where their spellings live. A constant the router never registers
# is a spelling with no method behind it — the gate must not report
# it as one. A registration the names table cannot spell is a defect
# the gate names rather than drops.
registered_methods() {
  local registered
  registered="$(grep -rho 'router\.register(names::[A-Z_]*' "$METHODS_DIR" \
    | sed 's/.*names:://' \
    | sort -u \
    | tr '\n' ' ' || true)"
  grep -ho 'pub const [A-Z_]*: &str = "[^"]*"' "$METHOD_NAMES" \
    | sed -E 's/pub const ([A-Z_]+): &str = "([^"]*)"/\1 \2/' \
    | awk -v wanted="$registered" '
        BEGIN {
          n = split(wanted, names, " ")
          for (i = 1; i <= n; i++) if (names[i] != "") want[names[i]] = 1
        }
        $1 in want { print $2; seen[$1] = 1 }
        END {
          missing = 0
          for (name in want) if (!(name in seen)) {
            printf "the router registers %s but the names table does not spell it\n", name > "/dev/stderr"
            missing = 1
          }
          if (missing) exit 1
        }
      ' \
    | sort -u
}

failures=0

statuses="$(status_column)"
if [[ -z "$statuses" ]]; then
  echo "the host matrix table is missing or empty: $MATRIX" >&2
  exit 1
fi

evidence="$(evidence_column)"
descriptions="$(descriptions_column)"

# 1-3: per-row status rules.
row=0
while IFS= read -r status; do
  row=$((row + 1))
  case "$status" in
    full | adapted | n/a | planned) ;;
    "") continue ;;
    *)
      echo "row $row: unknown status '$status' (full | adapted | n/a | planned)" >&2
      failures=$((failures + 1))
      ;;
  esac
done <<<"$statuses"

# 2-3: a `planned` row anchors itself, an `n/a` row says why. Both
# need the description cell to carry a sentence, not just a status.
row=0
while IFS= read -r status; do
  row=$((row + 1))
  case "$status" in
    planned | n/a) ;;
    *) continue ;;
  esac
  description="$(printf '%s\n' "$descriptions" | sed -n "${row}p")"
  if [[ -z "${description// }" ]]; then
    echo "row $row ($status): the row must anchor itself — say what would make it real, or why the host does not apply" >&2
    failures=$((failures + 1))
  fi
done <<<"$statuses"

# 4: the evidence of a landed row must be on disk.
row=0
while IFS= read -r cell; do
  row=$((row + 1))
  status="$(printf '%s\n' "$statuses" | sed -n "${row}p")"
  case "$status" in
    full | adapted) ;;
    *) continue ;;
  esac
  # Backticked paths are the evidence; every one must exist.
  while IFS= read -r path; do
    [[ -z "$path" ]] && continue
    if [[ ! -e "$path" ]]; then
      echo "row $row ($status): the cited evidence does not exist: $path" >&2
      failures=$((failures + 1))
    fi
  done < <(printf '%s' "$cell" | grep -o '`[^`]*`' | tr -d '`' | grep '/' || true)
done <<<"$evidence"

# 5: the methods the matrix names must be registered by the server.
registered="$(registered_methods)"
registered_domains="$(printf '%s\n' "$registered" | cut -d/ -f1 | sort -u)"
while IFS= read -r method; do
  [[ -z "$method" ]] && continue
  if ! printf '%s\n' "$registered" | grep -qx "$method"; then
    echo "the matrix names a method the server does not register: $method" >&2
    failures=$((failures + 1))
  fi
done < <(
  # Only RPC method names: `domain/verb` (optionally `domain/group/verb`),
  # lower-case segments, no leading or trailing slash. The REST-shaped
  # take-item pointers (`/models`, `/resolved`) and the `n/a` status are
  # not methods.
  grep -o '`[a-z][a-z-]*/[a-z][a-z-]*\(/[a-z][a-z-]*\)*`' "$MATRIX" \
    | tr -d '`' \
    | grep -v -x 'n/a' \
    | sort -u \
    | while IFS= read -r candidate; do
        # A name whose first segment is not a domain the server
        # registers is not a method — it is a file path the document
        # mentions.
        [[ -e "$candidate" ]] && continue
        domain="${candidate%%/*}"
        if printf '%s\n' "$registered_domains" | grep -qx "$domain"; then
          printf '%s\n' "$candidate"
        fi
      done
)

# 6: the gap count only ratchets down.
gaps="$(printf '%s\n' "$statuses" | grep -c -E '^(planned|n/a)$' || true)"
baseline="$(cat "$BASELINE_FILE")"
if (( gaps > baseline )); then
  echo "the gap count rose: $gaps gaps (planned + n/a), the baseline is $baseline" >&2
  failures=$((failures + 1))
fi

if (( failures > 0 )); then
  echo "the capability matrix gate found $failures problem(s)" >&2
  exit 1
fi

echo "OK: the capability matrix is complete, anchored, on disk and within its baseline ($gaps gaps, baseline $baseline)"
