#!/usr/bin/env bash
set -euo pipefail
if cargo tree -p uva-kernel -e normal | grep -Eq 'uva-(provider|tool|session|harness|app-server|mcp|skill|eval|scheduler|attachment)'; then
  echo "kernel dependency leak" >&2
  exit 1
fi
echo "OK: kernel stays agent-free"
