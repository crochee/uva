# elysper-harness-tokio

Tokio-backed `Spawner` implementation for the runtime-neutral
[`elysper-harness`](../elysper-harness) agent loop. This is the
opt-in seam that bridges the harness's `Spawner` trait onto a tokio
executor; consumers on `smol` / `async-std` / a bare `futures`
executor do not depend on it.

## Install

```toml
[dependencies]
elysper-harness = "0.1"
elysper-harness-tokio = "0.1"
```

## Usage

```rust
use std::sync::Arc;
use elysper_harness::ReActAgent;
use elysper_harness_tokio::TokioSpawner;

let agent = ReActAgent::new(provider, tool_registry)
    .with_spawner(Arc::new(TokioSpawner));
```

## Why a separate crate (not a facade feature)?

`elysper-harness` ships with a runtime-neutral public API: its
`[dependencies]` does not name tokio, and its loop only references
the `Spawner` trait from `elysper-core`. Moving `TokioSpawner` into
this companion crate keeps that guarantee verifiable — `cargo tree
-p elysper-harness --no-default-features` never names tokio — while
giving tokio-based deployments a one-line adapter. The facade does
not re-export it: `elysper::TokioSpawner` would put a tokio type on
the facade's public API and fail `make check-public-api-runtime`;
consumers add this crate directly.

See `examples/tokio_spawner.rs` for a runnable smoke.
