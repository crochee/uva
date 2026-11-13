# uva-core-tokio

Tokio-backed implementations of the runtime-neutral seams that
[`uva-core`](../uva-core) defines: [`TokioSpawner`] for the
`Spawner` trait, [`TokioSleeper`] for `Sleeper`, and
[`TokioBlocking`] for `Blocking`.

This is the opt-in seam that bridges those traits onto a tokio
executor; consumers on `smol` / `async-std` / a bare `futures`
executor do not depend on it.

## Install

```toml
[dependencies]
uva-core = "0.1"
uva-core-tokio = "0.1"
```

## Usage

```rust
use std::sync::Arc;
use uva_core::spawn::Spawner;
use uva_core_tokio::TokioSpawner;

let spawner: Arc<dyn Spawner> = Arc::new(TokioSpawner);
```

## Why a separate crate?

`uva-core` and `uva-harness` both ship runtime-neutral
public APIs: neither `[dependencies]` names tokio, and their public
surfaces reference only the traits (`Spawner` / `Sleeper` /
`Blocking`) that live in `uva-core`. Keeping the tokio adapters
in a third crate keeps that guarantee verifiable — `cargo tree -p
uva-core --no-default-features` never names tokio — while giving
tokio-based deployments a one-line adapter.

The crate is *not* harness-specific: its library surface never
mentions the harness at all. The harness only appears in the example
and in the docs, because `ReActAgent::with_spawner` is where a
consumer installs the adapter.

## Through the facade

The `uva` facade re-exports the three adapters behind its
opt-in `harness-tokio` feature, as `uva::harness_tokio::{…}`:

```toml
uva = { version = "0.1", features = ["harness-tokio"] }
```

The feature adds **no** runtime to a consumer that does not opt in —
`uva-core-tokio`'s only runtime dependency is `tokio` with the
`rt` + `time` features.

See `examples/tokio_spawner.rs` for a runnable smoke.
