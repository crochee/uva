#!/usr/bin/env bash
# Frontend contract gate: `uva-web/src/api/types.ts` must be exactly
# what the server's wire types generate.
#
# The contract has one source of truth. `crates/uva-app-server/src/api/
# contract.rs` catalogues every type the browser reads (with the RPC
# method that answers it); `examples/emit-contract.rs` prints that
# catalog as JSON Schema; `uva-web/scripts/generate-types.mjs` compiles
# it into TypeScript. The committed `types.ts` is a projection of the
# Rust types, so it is never hand-edited — and the moment a response
# type changes without a regeneration, the two disagree about a field
# name, a nullability, or a closed set of wire values.
#
# This gate is the diff that catches that. Generation is
# deterministic (sorted keys, no timestamps, no filesystem paths, one
# prettier config) and was verified byte-identical across reruns, so
# regenerating and comparing bytes is enough: any difference means the
# committed file is stale.
#
# The regeneration goes to a scratch directory under `$HOME`, never
# `/tmp`: this workspace's `/tmp` is a small tmpfs and the emitter
# compiles `uva-app-server`, so a build there fills it. The trap
# removes the scratch on every exit path, and `--out` means the
# committed file is not touched even while the gate runs.
#
# Fix a failure with `make generate-web-types`, then commit
# `uva-web/src/api/types.ts`.
#
# Exit 0 with an `OK:` line, or exit 1 naming the stale file and the
# first divergence.

set -euo pipefail

cd "$(dirname "$0")/.."

CONTRACT="uva-web/src/api/types.ts"

if [[ ! -d uva-web/node_modules ]]; then
  echo 'FAIL: uva-web/node_modules is missing — run `npm ci` in uva-web first' >&2
  exit 1
fi

if [[ ! -f "$CONTRACT" ]]; then
  echo "FAIL: $CONTRACT does not exist — the generated contract is committed" >&2
  exit 1
fi

scratch="$(mktemp -d "${HOME:?HOME must hold the gate scratch directory}/.uva-check-web-contract.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT

generated="$scratch/types.ts"
report="$scratch/contract.diff"

# The same entry point `make generate-web-types` runs, pointed at the
# scratch file instead of the committed one.
(
  cd uva-web
  node scripts/generate-types.mjs --out "$generated"
)

if diff -u \
  --label "$CONTRACT (committed)" \
  --label "$CONTRACT (generated)" \
  "$CONTRACT" "$generated" >"$report"; then
  echo "OK: $CONTRACT matches the server's wire types"
  exit 0
fi

echo "FAIL: $CONTRACT is stale — it does not match a fresh run of the generator." >&2
echo '      Regenerate with `make generate-web-types` and commit uva-web/src/api/types.ts.' >&2
echo >&2
cat "$report" >&2
exit 1
