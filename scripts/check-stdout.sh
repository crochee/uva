#!/usr/bin/env bash
# stdout-discipline gate: the headless core writes protocol frames only.
#
# Why this exists: the core's stdout is the JSON-RPC transport. A
# `println!` (or a `print!`, or a `dbg!`, or a raw `std::io::stdout`)
# in the binary reaches the peer as bytes that are not a
# Content-Length frame, and the peer either drops the connection or
# reports a protocol error. Logs and diagnostics belong on stderr.
#
# The one writer allowed to touch stdout is the protocol writer in
# `crates/uva-app-server/src/rpc/stdio.rs`; this gate scans the
# binaries (`src/bin/` and `src/main.rs`) and nothing else, so that
# writer is not in scope.
#
# Why not the whole `src/` tree: `clippy.toml` sets
# `allow-dbg-in-tests = true`, so test code is explicitly allowed to
# use `dbg!` / `println!`. Inline `#[cfg(test)] mod … { … }` blocks
# live in library files, and a line-oriented grep cannot tell them
# from shipped code; a src-wide scan would therefore fail on test
# code. Skipping those blocks needs the brace-depth scanner that
# `check-single-writer.sh` carries, and this gate deliberately does
# not duplicate it. The bins + `main.rs` are where a process-level
# stdout write would be introduced; library code that writes stdout
# remains a review concern.
#
# Exit 0 with an `OK:` line, or exit 1 with every offending call site.
set -euo pipefail

cd "$(dirname "$0")/.."

CRATE="crates/uva-app-server"
targets=()
if [[ -d "$CRATE/src/bin" ]]; then
  targets+=("$CRATE/src/bin")
fi
if [[ -f "$CRATE/src/main.rs" ]]; then
  targets+=("$CRATE/src/main.rs")
fi

if [[ ${#targets[@]} -eq 0 ]]; then
  echo "FAIL: $CRATE has neither src/bin/ nor src/main.rs — the gate would pass vacuously" >&2
  exit 1
fi

# `println!` also covers `print!` via the optional `ln`; `std::io::stdout`
# catches a hand-rolled writer that bypasses the print macros.
pattern='\b(print|println|dbg)!|std::io::stdout'

# `--dump-config` is a CLI mode that prints one document and exits
# before the protocol pump starts: its output is the requested dump,
# not a frame stream. A line opts in with this marker, so the exception
# is explicit and reviewable rather than a silent gap.
if hits=$(grep -rnE "$pattern" "${targets[@]}" 2>/dev/null \
            | grep -v 'stdout-ok: dump-config'); then
  echo "FAIL: the headless core must not write to stdout directly:"
  printf '%s\n' "$hits" | sed 's/^/  /'
  echo "Logs and diagnostics go to stderr; stdout carries only Content-Length"
  echo "frames. Route a frame through the protocol writer in"
  echo "crates/uva-app-server/src/rpc/stdio.rs instead."
  exit 1
fi

echo "OK: no direct stdout writes in the core's binaries (${targets[*]})"
