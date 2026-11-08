#!/usr/bin/env bash
# Publishable gate: every crate in the workspace must be packagable by
# `cargo package`, i.e. a consumer can `cargo add elysper-<name>` and get
# a working crate instead of a resolution error.
#
# Why this exists: `cargo package` refuses a crate whose path-only
# workspace dependencies carry no version requirement ("all dependencies
# must have a version requirement specified when packaging"). That
# failure mode is invisible to `cargo build` / `cargo test` — the whole
# workspace resolves fine — so it only shows up when the crate is about
# to be published. This gate runs the packaging step for every member,
# so the "installable" claim is verified instead of asserted.
#
# `--no-verify` skips the unpack-and-build step (which would compile
# every dependency for every crate — minutes per crate); the version
# requirement and metadata checks still run. `--allow-dirty` keeps the
# gate usable on a working tree with uncommitted changes.
#
# The `[patch.crates-io]` overrides passed via `--config` are what make
# the gate runnable *before* the first publication: `cargo package`
# rewrites a path dependency into a registry dependency, so resolving
# `elysper-core` would hit crates.io and fail with "no matching package
# named `elysper-core` found" until the crate is actually on the
# registry. Pointing every sibling crate at its local path keeps the
# resolution local, so the gate measures the manifest (the part this
# gate is about) rather than the publication order. The overrides are
# passed on the command line, never written into the workspace
# manifest, so the published crates keep clean `[dependencies]`.
#
# Exit 0 with an `OK:` line, or exit 1 with cargo's own diagnostics.
set -euo pipefail

cd "$(dirname "$0")/.."

# Every member of the workspace, in normal dependency topology order.
# `cargo package` does not care about the order (it packages a single
# crate at a time), but keeping the list in dependency order means it
# doubles as the order a future `cargo publish` sweep would walk: a
# crate is only publishable once everything it depends on is on the
# registry.
#
# One member is package-only: `elysper-app-server` carries `publish =
# false` (its surface is a deployment contract, not a crates.io one),
# so `cargo publish` refuses it. It is still packaged here — the gate
# is about "installable", and a crate that cannot be packaged cannot
# be installed either.
ORDER=(
  elysper-core elysper-kernel elysper-agents elysper-telemetry elysper-provider
  elysper-attachment
  elysper-tool elysper-budget elysper-session elysper-context elysper-eval
  elysper-steering elysper-harness elysper-harness-tokio elysper-search
  elysper-macros elysper-mcp elysper-plugin-protocol
  elysper-plugin-host elysper-reflection elysper-resilience
  elysper-scheduler elysper-semantic-cache elysper-skill elysper-runtime
  elysper-strategy-best-of-n elysper-strategy-cot elysper-strategy-plan-execute
  elysper-test-support elysper-tool-memo elysper-tool-read
  elysper-tool-scheduler elysper-tool-search elysper-tool-shell
  elysper-tool-task elysper-tool-todo elysper-tool-web elysper-tool-write
  elysper-wasm-runtime
  elysper-workflow elysper elysper-app-server elysper-mcp-server
)

# The gate must cover every workspace member. A member missing from
# ORDER would never be packaged, and the gate would still print OK —
# the exact silent under-coverage this gate exists to prevent.
#
# The member list comes from `cargo metadata --no-deps`, not from the
# root manifest's `members` array: a path dependency of a member is a
# member too (`elysper-attachment` is one today, and the manifest
# never names it), so the manifest's array alone would under-count.
workspace_members() {
  # A package object starts with `{"name":"…","version":"…"`; a
  # dependency object carries `"source"` in that slot, so the shape
  # tells the two apart without a JSON parser.
  cargo metadata --no-deps --format-version 1 2>/dev/null \
    | grep -o '{"name":"[^"]*","version":"[^"]*"' \
    | sed 's/{"name":"//;s/","version":".*//' \
    | sort -u
}

ordered=$(printf '%s\n' "${ORDER[@]}" | sort -u)
members=$(workspace_members)
if [[ -z "$members" ]]; then
  echo "FAIL: the workspace member list is empty — 'cargo metadata' failed" >&2
  echo "or its output shape changed. The gate cannot tell whether it" >&2
  echo "covers every member, so it refuses to report OK." >&2
  exit 1
fi
missing=$(comm -23 <(printf '%s\n' "$members") <(printf '%s\n' "$ordered"))
extra=$(comm -13 <(printf '%s\n' "$members") <(printf '%s\n' "$ordered"))
if [[ -n "$missing" || -n "$extra" ]]; then
  echo "FAIL: the publish order does not match the workspace members" >&2
  [[ -n "$missing" ]] && printf '  not in the order: %s\n' $missing >&2
  [[ -n "$extra" ]] && printf '  not a member:     %s\n' $extra >&2
  echo "Every workspace member must be packaged; add it to ORDER in" >&2
  echo "scripts/check-publishable.sh (and the member list to the root" >&2
  echo "Cargo.toml) in the same change." >&2
  exit 1
fi

pack_one() {
  local crate="$1"
  shift
  local out
  # Try offline first: the gate should not need the network when the
  # registry index is already cached. Fall back to a networked run.
  if out=$(cargo package --no-verify --allow-dirty --offline "$@" -p "$crate" 2>&1); then
    return 0
  fi
  if out=$(cargo package --no-verify --allow-dirty "$@" -p "$crate" 2>&1); then
    return 0
  fi
  echo "FAIL: $crate did not package" >&2
  echo "$out" | tail -n 5 >&2
  exit 1
}

for crate in "${ORDER[@]}"; do
  if [[ ! -d "crates/$crate" ]]; then
    echo "FAIL: $crate is listed in the publish order but crates/$crate does not exist" >&2
    exit 1
  fi
  # Resolve every sibling crate from its local path so the packaging
  # step does not need the crates.io copies of our own crates.
  config_args=()
  for dep in "${ORDER[@]}"; do
    [[ "$dep" == "$crate" ]] && continue
    config_args+=(--config "patch.crates-io.$dep.path=\"crates/$dep\"")
  done
  pack_one "$crate" "${config_args[@]}"
done

echo "OK: ${#ORDER[@]} crates package"
