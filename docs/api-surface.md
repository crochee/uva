# The public surface, as a tour

[`api-frozen.md`](api-frozen.md) is the promise: it lists every
name the seven library crates keep importable, says how long the
promise runs, and names the process that changes it. This page is
the map for someone arriving new — what each crate hands you,
which names belong in your `use` lines, and which corners of the
tree are not part of the promise at all. Both pages describe the
same 405 curated names. The gate compares the **live curated surface**
against `scripts/frozen-api/baseline` — it does not read either page;
keeping the page and the baseline in step is the review convention
`docs/api-frozen.md` describes.

## The seven library crates

| Crate | You reach for it when you need… | Curated names |
|---|---|---|
| `elysper-core` | the runtime seams without a runtime: clock, cancellation, ids, sleep, plus shared registries and text helpers | 41 |
| `elysper-context` | the context window: managers and the memory tiers | 17 |
| `elysper-harness` | the agent loop: strategies, approval/resume, the host registry, events | 60 |
| `elysper-provider` | a model provider and everything around a completion: request/response types, credentials, retry classification, cache policy, JSON repair | 89 |
| `elysper-session` | the durable ledger: the sink and its version header, the log contract, events, history fold, compaction, search, token metering | 120 |
| `elysper-steering` | the vocabulary of policy: guards, hints, trackers, output transformers, hook events | 36 |
| `elysper-tool` | the tool paradigm: registry, traits, annotations, restriction, surface projection | 42 |

The full, sorted lists live in
[the frozen names](api-frozen.md#the-frozen-names).

## Where to start

The smallest useful consumer holds four crates, not one. The
harness hands back traits it does not own and does not re-export
(`ModelProvider` stays in `elysper-provider`, `Tool` in
`elysper-tool`, `CancelToken` and `Spawner` in `elysper-core`), so
your manifest names all four plus `async-trait`, the proc macro
those traits are declared with:

```toml
elysper-harness  = "0.1"
elysper-core     = "0.1"
elysper-provider = { version = "0.1", default-features = false }
elysper-tool     = "0.1"
async-trait      = "0.1"
```

The `default-features = false` line matters: `elysper-provider`'s
default features turn on the two shipped HTTP adapters, and each
adapter carries `reqwest` and `tokio`. Leave the defaults off and
no HTTP client — and no runtime — enters your tree. The
[`crates/elysper-harness/README.md`](crates/elysper-harness/README.md)
walks the full reasoning, and
[`docs/examples/harness-only-consumer`](examples/harness-only-consumer)
is the runnable proof (its `cargo tree` counts zero `tokio`).

The other three library crates — `elysper-session`,
`elysper-context`, `elysper-steering` — are already inside the
harness's dependency closure: it names them in its signatures.
You add them to your own manifest when you want to write their
names yourself — `elysper_session::LogWriter` for a durable run
log, a `elysper_context::ContextManager` of your own, a
`elysper_steering::HookMap` for guards and hooks over the loop.

## Which names to write in your `use` lines

One rule: **import from the crate root, spelled exactly as the
frozen list spells it.**

```rust
use elysper_harness::{Agent, AgentEvent, ReActAgent};
use elysper_provider::ModelProvider;
use elysper_tool::{Tool, ToolRegistry};
```

Every curated name is promised for the remainder of the `0.1`
line — no removals, no renames, in any `0.1.x` release. At the
next minor bump (`0.2`) the commitment is revisited; names may
move there, but only together with an updated
[`api-frozen.md`](api-frozen.md).

A handful of these names exist only behind a Cargo feature or
platform flag: `OpenAICompatibleProvider` needs the `openai`
feature, the `AnthropicStream*` types need `anthropic`,
`signal_of` is unix-only. The name stays on the curated surface;
the code behind it obeys its feature flag.

## What is *not* promised

The same boundary, from the tourist's side:

- **Deep paths.** `elysper_harness::agent::host`, the `pub` items
  inside modules the crate root never re-exported — reachable,
  compileable, and free to move or disappear in any release.
  Experimental territory: fine for a spike, wrong for a
  dependency. If a deep path is the only way to do what you need,
  say so, and we will consider curating it.
- **Signatures.** The promise covers the *set of names*, not the
  shapes behind them. Parameter lists, generics and trait bounds
  follow ordinary pre-1.0 evolution, recorded in the changelog.
- **The server's HTTP API.** `/api/v1` URLs version themselves;
  the library crates listed above are the whole of this promise.
- **Session-log compatibility across format versions.** The log
  has its own version gate — [`log-format.md`](log-format.md)
  states what it promises (and refuses).

## How the surface stays true

Three gates, run by `make ci`, keep the tour and the promise
honest:

- `check-pub-surface` bans glob re-exports outright — every
  public name a crate exports appears literally in its `lib.rs`;
- `check-pub-surface-strict` ratchets the *number* of `pub use`
  statements per crate;
- `check-frozen-api` pins the *identity* of every exported name
  against [`scripts/frozen-api/baseline`](scripts/frozen-api/baseline)
  — a new name fails the gate until a human adds it to the
  baseline and to [`api-frozen.md`](api-frozen.md), and a removed
  name fails it inside `0.1.x`, which is exactly the promise.

Adding a name is therefore one reviewed change, three files, one
commit: the `pub use` entry, the regenerated baseline, the list.
The review is where "does this belong on the curated surface?"
gets answered — new names are liabilities, not features.
