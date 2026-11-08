#!/usr/bin/env bash
set -euo pipefail
if cargo tree -p elysper-kernel -e normal | grep -Eq 'elysper-(provider|tool|session|harness|app-server|mcp|skill|eval|scheduler|attachment)'; then
  echo "kernel dependency leak" >&2
  exit 1
fi
echo "OK: kernel stays agent-free"
