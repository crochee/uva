#!/usr/bin/env bash
# Assert the hook contract is usable from outside the harness: the
# `hook-only-consumer` fixture names `uva-hook` as its only normal
# dependency, so its normal dependency tree must contain no
# `uva-harness` — and its dev-only tests must still drive a real run
# through the harness with the fixture's own handler.
#
# The check runs *inside the fixture* rather than as
# `cargo tree -p uva-hook`: the contract crate is a dependency of the
# harness, so a workspace-level query would be true by construction
# and prove nothing about a third-party consumer (ADR 0004).
set -euo pipefail

cd "$(dirname "$0")/.."

fixture="docs/examples/hook-only-consumer"
tree=$(cargo tree --manifest-path "$fixture/Cargo.toml" -e normal --prefix none)

if grep -Eq '^uva-harness( |$)' <<<"$tree"; then
  echo "FAIL: the hook-only consumer's normal tree pulls in uva-harness:" >&2
  grep -E '^uva-harness( |$)' <<<"$tree" >&2
  echo "The hook contract must be usable without the loop; keep the" >&2
  echo "harness in dev-dependencies only and fix the leak at its source." >&2
  exit 1
fi

# The behavioral half: the fixture's tests drive one real tool call and
# assert the handler saw it, a Block short-circuits it, a dropped
# registration stops firing, and an out-of-tree `before_run` observer
# receives the observation point (ADR 0028).
cargo test --manifest-path "$fixture/Cargo.toml" --quiet

echo "OK: hook-only consumer's normal tree has no uva-harness; its" \
  "handler intercepts a real run"
