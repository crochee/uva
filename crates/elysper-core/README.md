# elysper-core

The foundation crate: the vocabulary every other `elysper-*` crate
depends on, and the only one with no dependency on a runtime, a
provider SDK, or the pieces above it (`elysper` → `elysper-*` →
`elysper-core`, never back).

## What it owns

| Module | Primitive |
|---|---|
| `error` | the single `Error` enum every fallible API returns, plus `Result<T, E = Error>` — one domain taxonomy, one snake_case wire code (`Error::kind()`) |
| `cancel` | the `CancelToken` trait + the std-only `AtomicCancelToken`; a `tokio-util` bridge behind the `tokio-util` feature, so a tokio caller's `CancellationToken` coerces with no adapter |
| `spawn` | the `Spawner` trait, `BoxFuture`, and `SharedSpawner` — detached work with no runtime in the signature |
| `blocking` | the `Blocking` trait + `ThreadBlocking` — a blocking call with no runtime in the signature, so a session's fs write does not name `tokio::task::spawn_blocking` |
| `clock` | the `Clock` trait + `SystemClock` / `FixedClock` and the `SharedClock` handle; chrono `DateTime<Utc>` is the wall-clock vocabulary |
| `sleeper` | the `Sleeper` trait + `ThreadSleeper` / `FixedSleeper` — a delay with no runtime in the signature, so a timeout the harness arms does not name `tokio::time::sleep` |
| `idgen` | the `IdGen` trait + `UlidGenerator` / `SequenceGenerator` and the `SharedIdGen` handle |
| `registry` | `Registry` / `RegistryItem` — the generic named catalog with cursor pagination that agents, tools, skills and memories all plug into |
| `schema` | JSON-Schema-subset validation with dotted-path, all-at-once violations |
| `sensitive` | `Sensitive` / `SensitiveData` redaction so secret-bearing values stay out of `Debug` output |
| `text` | `cap_to_char_boundary` — UTF-8-safe truncation |
| `token` | the cheap token heuristic (`estimate_token_count`, 4 ASCII bytes / 1.5 CJK chars per token) for pre-flight budget checks |
| `full_output` | chunked full-output retention for tool results too large to commit |

## What is deliberately *not* here

- **Provider wire types** (`Message`, `ContentPart`, `ModelConfig`) —
  they live in `elysper-provider`, so a consumer that pulls
  `elysper-core` does not transitively pull a model SDK.
- **An async runtime** — every primitive compiles with no `tokio` /
  `async-std` / `smol` in the public API. The only async surfaces are
  the cooperative-cancellation future on `CancelToken::cancelled`, the
  delay future on `Sleeper::sleep`, and the completion future on
  `Blocking::run`, all runtime agnostic.
- **The agent loop, tools, steering, memory, sessions** — those are
  their own crates so each can be removed independently.

## Time and IDs: the standard pairing

Every piece of Elysper that stamps an instant or mints an identifier
takes a `SharedClock` / `SharedIdGen` rather than calling
`chrono::Utc::now()` or `ulid::Generator::new().generate()`:

```rust
use elysper_core::{Clock, SharedClock};

let clock = SharedClock::system();          // production, once per process
let now = clock.now();

// Tests inject a fixed instant instead:
let fixed = SharedClock::fixed_from_rfc3339("2026-01-02T03:04:05Z");
assert_eq!(fixed.now().to_rfc3339(), "2026-01-02T03:04:05+00:00");
```

A public constructor that "stamps now" also ships an explicit-`now`
sibling (`MemoryEntry::at`, `OperationSnapshot::new_at`, …), so a caller
can be deterministic without replacing a global. `make check-clock`
keeps library code off the raw wall clock.

## Usage

```rust
use elysper_core::{Clock, Error, SharedClock};

let err = Error::not_found("session");
assert_eq!(err.to_string(), "not found: session");
assert_eq!(err.kind(), "not_found");

// Cancellation and spawning are the two capabilities a runtime would
// otherwise force on you; both are traits you supply
// (`CancelToken`, `spawn::Spawner`).
let clock = SharedClock::system();
let _now = clock.now();
```

## Minimal imports

```rust,ignore
use elysper_core::{AtomicCancelToken, CancelToken, Error, Result};
```
