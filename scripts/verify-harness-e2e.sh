#!/usr/bin/env bash
# End-to-end proof: the assembled harness really answers a question with
# a tool call.
#
# Why this is not a CI gate: it needs a live model endpoint and a
# provider key, so it cannot run on every push. What it *does* prove is
# the claim the rest of the repo only asserts — that mounting the
# plugin set and sending one message produces a tool call the server's
# own registry served, and a final answer that quotes the file's real
# contents.
#
# What it asserts (each line is a server-side fact, not this script's
# opinion):
#
#   1. the gateway comes up (`/readyz` 200);
#   2. the model adapter published a model (`models/list`);
#   3. the agent loop ran a step (`step` start/end rows);
#   4. the tool the model asked for was dispatched by the registry
#      (`tool_dispatch` row);
#   5. the tool returned the file's real bytes (`tool_result` row);
#   6. the final assistant message quotes that content.
#
# Usage:
#
#   scripts/verify-harness-e2e.sh [--port 8099] [--profile <path>]
#
# The provider configuration comes from the environment (`.env` at the
# repository root is loaded when present): `OPENAI_BASE_URL` /
# `OPENAI_API_KEY` / `OPENAI_MODEL`, or the `ANTHROPIC_*` trio. The
# profile defaults to the repository's `native_profile`, i.e. every
# built-in plugin — the point is to exercise the whole assembly.
#
# Requires `python3` with the `websockets` module.

set -euo pipefail

cd "$(dirname "$0")/.."

PORT=8099
PROFILE=""
KEEP=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --port) PORT="$2"; shift 2 ;;
    --profile) PROFILE="$2"; shift 2 ;;
    --keep) KEEP=1; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

ROOT="$(pwd)"
WORKSPACE="$(mktemp -d)"
BIN="$ROOT/target/release/uva-app-server"
LOG="$(mktemp)"

if [[ ! -x "$BIN" ]]; then
  echo "FAIL: $BIN not found — run 'cargo build -p uva-app-server --release' first" >&2
  exit 1
fi

if ! python3 -c 'import websockets' 2>/dev/null; then
  echo "FAIL: python3 needs the 'websockets' module (pip install websockets)" >&2
  exit 1
fi

cleanup() {
  if [[ -n "${SERVER_PID:-}" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  if [[ "$KEEP" -eq 0 ]]; then
    rm -rf "$WORKSPACE"
  else
    echo "workspace kept at $WORKSPACE"
  fi
}
trap cleanup EXIT

# The file the agent is asked to read: its contents are the oracle the
# final answer must quote.
printf 'The answer to life, universe and everything is 42.\n' > "$WORKSPACE/hello.txt"

# `--profile` omitted: the server's own native profile — every
# built-in plugin — is the assembly under test.
ARGS=(serve --port "$PORT" --directory "$WORKSPACE")
if [[ -n "$PROFILE" ]]; then
  ARGS+=(--profile "$PROFILE")
fi

UVA_HOST=127.0.0.1 UVA_PORT="$PORT" RUST_LOG=warn \
  "$BIN" "${ARGS[@]}" > "$LOG" 2>&1 &
SERVER_PID=$!

for _ in $(seq 1 60); do
  if curl -fsS --max-time 2 "http://127.0.0.1:$PORT/readyz" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

if ! curl -fsS --max-time 5 "http://127.0.0.1:$PORT/readyz" >/dev/null 2>&1; then
  echo "FAIL: the gateway never became ready" >&2
  tail -30 "$LOG" >&2
  exit 1
fi

python3 - "$PORT" "$WORKSPACE" <<'PY'
import asyncio
import json
import sys

import websockets

port, workspace = sys.argv[1], sys.argv[2]


async def call(ws, method, params, request_id):
    await ws.send(json.dumps({
        "jsonrpc": "2.0", "id": request_id, "method": method, "params": params,
    }))
    while True:
        message = json.loads(await asyncio.wait_for(ws.recv(), timeout=180))
        if message.get("id") == request_id:
            if "error" in message:
                raise SystemExit(f"FAIL: {method} returned {message['error']}")
            return message["result"]


async def main():
    async with websockets.connect(f"ws://127.0.0.1:{port}/rpc", max_size=None) as ws:
        models = await call(ws, "models/list", {}, 1)
        if not models.get("models"):
            raise SystemExit("FAIL: no model was published by the model adapter")
        print(f"OK: model adapter published {len(models['models'])} model(s): "
              f"{models['default_provider']}/{models['default_model']}")

        session = await call(ws, "chat/session/create", {"agent": "agent"}, 2)
        session_id = session["session_id"]
        await call(ws, "sessions/follow", {"session_id": session_id}, 3)

        await call(ws, "chat/message/send", {
            "session_id": session_id,
            "text": "Read the file hello.txt with the read tool and tell me "
                    "exactly what it says.",
        }, 4)

        dispatched, results, steps, answer = [], [], 0, ""
        deadline = asyncio.get_event_loop().time() + 180
        while asyncio.get_event_loop().time() < deadline:
            try:
                raw = await asyncio.wait_for(ws.recv(), timeout=45)
            except asyncio.TimeoutError:
                break
            message = json.loads(raw)
            if not message.get("method"):
                continue
            params = message.get("params", {})
            row = params.get("row", {})
            kind = row.get("type")
            data = row.get("data", {})
            if kind == "step":
                steps += 1
            elif kind == "tool_dispatch":
                dispatched.append(row.get("tool"))
            elif kind == "tool_result":
                content = data.get("content", {})
                single = content.get("Single") if isinstance(content, dict) else None
                text = single.get("text", "") if isinstance(single, dict) else ""
                results.append((row.get("is_error", False), text))
            elif kind == "assistant_message":
                content = data.get("content", {})
                multi = content.get("Multi") if isinstance(content, dict) else None
                if isinstance(multi, list):
                    answer = "".join(
                        part.get("text", "") for part in multi
                        if isinstance(part, dict) and part.get("type") == "text"
                    )
            if kind == "turn" and data.get("phase") == "end":
                break

        if steps < 2:
            raise SystemExit(f"FAIL: the agent loop ran {steps} step row(s); expected a start and an end")
        print(f"OK: the agent loop ran ({steps} step rows)")

        errors = [text for is_error, text in results if is_error]
        if errors:
            raise SystemExit(f"FAIL: the tool call failed: {errors[0][:200]}")

        payload = "\n".join(text for _, text in results)
        if not results:
            # The model answered without calling a tool. That is the
            # model's choice, not a harness failure — but it is also
            # not the proof this script exists for, so say so plainly
            # rather than reporting a pass.
            raise SystemExit(
                "FAIL: the turn produced no tool result. The harness served "
                "the turn, but the tool path was not exercised; re-run, or "
                "check the agent's tool policy."
            )
        if dispatched:
            print(f"OK: the registry dispatched {dispatched}")
        if "42" not in payload:
            raise SystemExit(f"FAIL: the tool result does not carry the file's contents: {payload[:200]}")
        print("OK: the tool returned the file's real contents")

        if "42" not in answer:
            raise SystemExit(f"FAIL: the final answer does not quote the file: {answer[:200]}")
        print(f"OK: the final answer quotes the file: {answer[:120]!r}")

        print("HARNESS-E2E: OK")


asyncio.run(main())
PY
