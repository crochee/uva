# elysper-hpc-batch

Batched-embedder / batched-completer adapters. Two thin wrappers
that turn one embedding call or one completion call into a
bounded-concurrency batch, without changing the wrapped provider's
per-call semantics.

This crate is **opt-in** (`elysper = { features = ["hpc-batch"] }` on
the facade, or `cargo add elysper-hpc-batch` for direct consumers).
It is offline by default—no HTTP, no DB, no OTel—and stays that
way under the `make check-mvp-deps` gate (`R_OPT_HPC_PLUGIN_CRATES`
segment).

## Surface

- `BatchEmbedder`—wraps `Arc<dyn Embedder>`; `embed_batch(Vec<String>)`
  splits the input into chunks of `BatchPolicy::chunk_size`, dispatches
  up to `BatchPolicy::max_inflight` at a time, and returns embeddings
  in input order.
- `BatchCompleter`—wraps `Arc<dyn ModelProvider>`; same chunking
  discipline for `complete_batch(Vec<CompletionRequest>)`.
- `BatchPolicy`—`chunk_size = 32`, `max_inflight = 8` by default.
- `BatchStats`—live `submitted` / `inflight` / `completed` /
  `failed` counters.

## Runtime

The public surface is `Send + Sync` on the traits and `async fn` on the
batch entry points. No `tokio::*` / `async-std::*` / `smol::*` appears
in the public API (`make check-public-api-runtime` stays green). The
sync `Embedder::embed_batch` chunks run on their own `std::thread`;
the async `ModelProvider::complete` calls are awaited on whichever
runtime the wrapped provider uses.

## Wiring

```rust
use std::sync::Arc;

use elysper_hpc_batch::{BatchEmbedder, BatchPolicy};
use elysper_search::{CjkTokenizer, Embedder, HashingEmbedder};

# async fn example() {
let inner: Arc<dyn Embedder> =
    Arc::new(HashingEmbedder::new(64, Arc::new(CjkTokenizer)));
let batch = BatchEmbedder::with_policy(
    inner,
    BatchPolicy { chunk_size: 32, max_inflight: 8 },
);
let embeddings = batch
    .embed_batch(vec!["hello".into(), "world".into()])
    .await
    .unwrap();
assert_eq!(embeddings.len(), 2);
assert_eq!(batch.stats().completed, 2);
# }
```

## Demo

```bash
cargo run --example embed_batch_demo -p elysper-hpc-batch
cargo run --example completer_batch_demo -p elysper-hpc-batch
```

Expected tails:

```text
EMBED-BATCH: ok submitted=64 completed=64 inflight_peak=0 wall_ms=…
COMPLETER-BATCH: ok submitted=32 completed=32 wall_ms=…
```

All five plug-ins together, the design invariants, and the
per-crate verification commands are documented in
[`docs/research/r-opt-hpc-round.md`](../../docs/research/r-opt-hpc-round.md).
