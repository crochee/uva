#!/usr/bin/env bash
# Public-docs ratchet: the undocumented `pub` items under the seven
# frozen crates' `src/` must not grow past the per-crate counts in
# `scripts/public-docs/baseline`.
#
# Why this exists: the `[workspace.lints]` rustdoc lints
# (`missing_docs = "warn"`, ...) only reach a crate that opts in
# with `[lints] workspace = true`, and no crate in this workspace
# does, so nothing in the build enforces public docs today (see
# AGENTS.md §3.4). This gate is that enforcement: it counts, per
# frozen crate, the `pub fn|struct|enum|trait|const|type|mod`
# declaration lines whose nearest preceding non-empty line is not a
# `///` doc comment, and fails the moment a crate's count grows.
# The counts only ratchet down: a documentation round lowers them,
# never raises them.
#
# What the rule deliberately does *not* do: `pub use` lines are not
# counted (they re-export names, they do not declare items);
# `pub(crate)` / `pub(super)` are not public surface; attribute
# lines (`#[derive(..)]`, `#[serde(..)]`) are not skipped — an item
# whose doc is separated from its declaration by an attribute
# counts as undocumented, which is the conservative reading the
# baseline was measured with. Test code is out of scope: `tests/`
# directories and `tests.rs` files are excluded from the scan.
#
# `--print` writes the current counts, one `crate=count` per line,
# so a deliberate cleanup can rebaseline:
#   scripts/check-public-docs.sh --print > scripts/public-docs/baseline
#
# Exit 0 with an `OK:` line, or exit 1 with the per-crate overshoot.

set -euo pipefail

cd "$(dirname "$0")/.."

# The seven frozen crates only: their crate-root `pub use` surface
# is pinned by `make check-frozen-api`, so their public items are
# exactly the contract this gate keeps documented. Kept explicit so
# a new crate never joins silently.
CRATES=(
  elysper-core
  elysper-tool
  elysper-provider
  elysper-session
  elysper-context
  elysper-steering
  elysper-harness
)

BASELINE_FILE="scripts/public-docs/baseline"

# A crate without a src tree cannot be counted. Fail with the crate
# named rather than letting `find` fail with raw stderr under
# `set -e` — the header promises a `FAIL:` line.
for crate in "${CRATES[@]}"; do
  if [[ ! -d "crates/$crate/src" ]]; then
    echo "FAIL: crates/$crate/src does not exist — the gate cannot count $crate's public items" >&2
    exit 1
  fi
done

# One crate's undocumented-public-item count. The classifier is the
# literal rule: a declaration line is a trimmed line of the form
# `pub <keyword>` for the seven item keywords, and the item counts
# as documented only when the nearest preceding non-empty line is a
# `///` doc line — blank lines do not interrupt that line, and
# nothing else (attributes, plain `//` comments) is looked past.
count_undocumented() {
  local crate="$1"
  find "crates/$crate/src" -name '*.rs' ! -name 'tests.rs' ! -path '*/tests/*' -print0 |
    xargs -0 -r awk '
      function trim(s) { sub(/^[ \t]+/, "", s); sub(/[ \t]+$/, "", s); return s }
      # A new file must not inherit the state of the one before
      # it: a file that ends in a dangling `///` run must not
      # document the first declaration of the next file.
      # `documented` is the only per-file flag; `count` is the
      # running total, carried across files within one invocation
      # on purpose.
      FNR == 1 { documented = 0 }
      {
        t = trim($0)
        # A blank line does not interrupt the doc line above.
        if (t == "") next
        if (t ~ /^\/\/\//) { documented = 1; next }
        if (t ~ /^pub[ \t]+(fn|struct|enum|trait|const|type|mod)([^[:alnum:]_]|$)/) {
          if (!documented) count++
          documented = 0
          next
        }
        documented = 0
      }
      END { print count + 0 }
    ' |
    awk '{ sum += $1 } END { print sum + 0 }'
}

case "${1:-}" in
  --print)
    for crate in "${CRATES[@]}"; do
      printf '%s=%s\n' "$crate" "$(count_undocumented "$crate")"
    done
    exit 0
    ;;
  "") ;;
  *)
    echo "FAIL: unknown argument '$1' — only --print is supported" >&2
    exit 1
    ;;
esac

if [[ ! -f "$BASELINE_FILE" ]]; then
  echo "FAIL: $BASELINE_FILE is missing; create it with: scripts/check-public-docs.sh --print > $BASELINE_FILE" >&2
  exit 1
fi

# The baseline is a file a human edits: validate the shape before
# trusting it, so a typo cannot silently drop a crate from the gate.
declare -A baseline=()
while IFS='=' read -r crate count; do
  if [[ -z "$crate" && -z "$count" ]]; then
    continue
  fi
  if [[ " ${CRATES[*]} " != *" $crate "* || ! "$count" =~ ^[0-9]+$ ]]; then
    echo "FAIL: malformed baseline line '$crate=$count' in $BASELINE_FILE" >&2
    exit 1
  fi
  baseline["$crate"]="$count"
done <"$BASELINE_FILE"

fail=0
for crate in "${CRATES[@]}"; do
  current=$(count_undocumented "$crate")
  base=${baseline[$crate]:-}
  if [[ -z "$base" ]]; then
    echo "FAIL: $crate is missing from $BASELINE_FILE — rebuild it with --print" >&2
    fail=1
    continue
  fi
  if (( current > base )); then
    echo "FAIL: $crate has $current undocumented public items, over its $base baseline" >&2
    fail=1
  fi
done

if (( fail )); then
  echo "Document the new items (a /// line directly above the" >&2
  echo "declaration), or — after a deliberate documentation round —" >&2
  echo "lower $BASELINE_FILE via --print. The counts never go up." >&2
  exit 1
fi

echo "OK: all ${#CRATES[@]} frozen crates at or under their public-docs baselines ($BASELINE_FILE)"
