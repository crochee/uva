#!/usr/bin/env bash
# Profile-document gate: the committed document restates the plugin tree.
#
# `.agents/profile.toml` is a project asset (the deployment's own
# statement of the whole plugin tree). It drifts silently: a plugin
# gains an `inject` edge, and the document — which claims to describe
# the same tree — keeps describing the old one. A reader of the
# document then sees dependencies the kernel does not order.
#
# The gate compares the document's own `inject` against the **effective**
# edge set the kernel resolves for each row (the factory manifest's
# `inject` unioned with the row's declaration). They must be equal:
#
# - a manifest edge the document does not restate → the document is
#   stale → red;
# - a document edge the manifest does not declare → the document
#   widened the plugin on purpose → legal, and the effective set still
#   equals the document's → green.
#
# The check itself is an integration test (`tests/profile_document.rs`
# in `uva-app-server`), so the rule lives with the code it checks.
#
# Exit 0 with an `OK:` line, or exit 1 with the failing row.

set -euo pipefail

cd "$(dirname "$0")/.."

DOCUMENT="${DOCUMENT:-.agents/profile.toml}"

if [[ ! -f "$DOCUMENT" ]]; then
  echo "the committed profile document is missing: $DOCUMENT" >&2
  exit 1
fi

if ! cargo test -p uva-app-server --test profile_document -- --nocapture; then
  echo "the committed profile document does not restate the plugin tree" >&2
  exit 1
fi

echo "OK: $DOCUMENT restates every plugin's edges"
