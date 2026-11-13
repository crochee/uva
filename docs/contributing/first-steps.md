# First steps: from a clone to one full turn

This page is the keyboard trail for a first contact with the
repository: what to install, which commands to run, and what
output means it worked. At the end you will have built the agent
loop, watched it run one complete turn (a user message, a model
call, a tool call, the final answer) on an executor that is not
tokio, and run a crate's test suite. Process questions — commit
conventions, DCO, review flow — live in
[`CONTRIBUTING.md`](../../CONTRIBUTING.md); this page stays on the
commands.

Every command below was run in a fresh clone checked out at commit
`aec2999d`, and every output block is pasted from that run,
trimmed to the load-bearing lines (elisions are marked with `…`).
Timing numbers and compile-line paths vary by machine; proof lines
(`RUNTIME-AGNOSTIC: OK`, `LIB-GRADE: OK`, `test result: ok.`) do
not.

## What you need

- **A Rust toolchain via rustup.** The channel is pinned by
  [`rust-toolchain.toml`](../../rust-toolchain.toml); entering the
  directory with rustup installed resolves it automatically.
- **`make`** — the repository's gates are Makefile targets.
- **No API key.** Every example and test in this trail runs
  offline against scripted providers and local fixtures (the
  first `cargo build` still fetches crates.io dependencies, as
  any Rust build does).

## Step 1 — clone

```console
$ git clone git@github.com:crochee/uva.git
Cloning into 'uva'...
```

```console
$ cd uva
```

## Step 2 — build one crate

The workspace is large; you never need to build all of it.
Building the harness builds its dependency closure and nothing
else:

```console
$ cargo build -p uva-harness
   Compiling proc-macro2 v1.0.107
   Compiling quote v1.0.47
   Compiling unicode-ident v1.0.24
   … 118 more `Compiling` lines; each names a crate and the
     absolute path of the checkout that built it …
   Compiling uva-harness v0.1.0 (/tmp/t12/fresh/crates/uva-harness)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 20.38s
```

A clean `Finished` line is success. The first build pays for the
closure; later builds are incremental.

## Step 3 — run one full turn

```console
$ cargo run --example runtime_agnostic -p uva-harness
   … first invocation also compiles the example …
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 11.80s
     Running `target/debug/examples/runtime_agnostic`
=== uva: agent loop on a non-tokio executor ===

spawner          : std::thread + futures::executor::block_on
events observed  : 11
model calls      : 2
tool calls       : 1
answer           : the loop ran without tokio

RUNTIME-AGNOSTIC: OK
```

That output **is** one full turn, end to end:

- a scripted provider (no network) receives the user message;
- its first response asks for the `echo` tool — a consumer-owned
  `Tool` implementation the example carries — which runs and
  returns;
- the second model call sees the tool result and produces the
  answer, `the loop ran without tokio`;
- the loop published 11 `AgentEvent`s on the way.

`RUNTIME-AGNOSTIC: OK` is the proof line: the turn ran, the event
count and call counts matched what the example expects. Its
source,
[`runtime_agnostic.rs`](../../crates/uva-harness/examples/runtime_agnostic.rs),
is short enough to read in one sitting.

## Step 4 — the zero-tokio path

The turn above was no accident of configuration. The harness's
one runtime touchpoint — where a run's detached task goes — is the
`Spawner` trait, and the example installs the smallest
implementation there is: `std::thread::spawn` plus
`futures::executor::block_on`. Nothing in the loop, the tool
dispatch, or the event stream names an executor.

Two ways to convince yourself:

```console
$ make check-no-runtime
OK: uva-core uva-scheduler uva-macros uva-eval uva-search uva-session, uva-telemetry --no-default-features, and uva-tool's own deps are tokio-free
```

and the standing example
[`docs/examples/harness-only-consumer`](../examples/harness-only-consumer)
— a standalone crate whose `cargo tree` counts zero `tokio`, driven
by `futures::executor::block_on`. The JSONL session sink is
runtime-neutral too — it is on that list — since the session crate
moved its fs work behind the `Blocking` seam. What is *not*
runtime-neutral is documented in the harness README: the provider
HTTP adapters and the built-in tools.

## Step 5 — run one crate's tests

Tests run **per crate** — `cargo test --workspace` is banned by
the repository's conventions (see `AGENTS.md`), because it hides
which crate broke:

```console
$ cargo test -p uva-harness --lib
   … compile lines for the dev-dependencies …
running 228 tests
… 228 `test … ok` lines …
test result: ok. 228 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.14s
```

The same pattern applies to the other crates:
`cargo test -p <crate> --lib`. `228 passed; 0 failed` is the
success shape.

## Step 6 — the five-minute smoke (optional)

```console
$ cargo run --example lib_grade -p uva-harness
   … compile lines …
     Running `target/debug/examples/lib_grade`
LIB-GRADE: OK
```

`lib_grade` assembles an agent from independent pieces, registers
a consumer-side strategy in `StrategyRegistry`, and swaps the
loop (`react` → `chain-of-thought` → one you wrote) by changing
one line — a good next file to read after `runtime_agnostic`.

## Where to go next

- [`crates/uva-harness/README.md`](../../crates/uva-harness/README.md) —
  the harness tour: assembly, strategies, hooks, the seams table.
- [`../api-surface.md`](../api-surface.md) — which names your
  `use` lines should depend on, and which are not promised.
- [`../api-frozen.md`](../api-frozen.md) — the promise behind
  those names and how it is enforced.
- [`../log-format.md`](../log-format.md) — the session log's
  on-disk shape and its version gate.
- [`../../SEAMS.md`](../../SEAMS.md) and
  [`../../MINIMAL.md`](../../MINIMAL.md) — the seven swap points
  and the smallest working consumer.
- [`../examples/README.md`](../examples/README.md) — every
  example, indexed by the seam it demonstrates.

Before sending a PR, [`CONTRIBUTING.md`](../../CONTRIBUTING.md)
asks for `make ci` — the full gate list, re-run verbatim by the
project's CI.
