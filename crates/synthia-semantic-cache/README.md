# synthia-semantic-cache

Embedding-similarity cache for LLM completions. A `ModelProvider`
wrapper that short-circuits near-duplicate requests via cosine
similarity over a pluggable embedder and store, leaving the rest of
the agent loop untouched.

This crate is **opt-in** (`synthia = { features = ["semantic-cache"] }`
in the facade, or `cargo add synthia-semantic-cache` for direct
consumers). It is offline by default — no HTTP, no DB, no OTel —
and stays that way under the `make check-mvp-deps` gate.

## Surface

- `SemanticCacheStore` — pluggable storage trait (`lookup`, `store`,
  `clear`, `len`).
- `InMemorySemanticCache` — built-in, thread-safe LRU.
- `SemanticCacheProvider` — `ModelProvider` wrapper that consults the
  store on `complete` and streams through unchanged.
- `SemanticCacheConfig` — `threshold` (cosine), `ttl`, `max_entries`.
- `HashingEmbedder` (from `synthia-search`) is the default. A consumer
  plugs `ModelProviderEmbedder` or their own `Embedder` for higher
  hit-rate.

## Wiring

```rust
use std::sync::Arc;

use synthia_provider::AnthropicProvider;
use synthia_search::{CjkTokenizer, Embedder, HashingEmbedder};
use synthia_semantic_cache::{
    InMemorySemanticCache, SemanticCacheConfig, SemanticCacheProvider,
};
use std::time::Duration;

let provider = Arc::new(AnthropicProvider::from_env()?);
let embedder: Arc<dyn Embedder> = Arc::new(HashingEmbedder::new(
    64,
    Arc::new(CjkTokenizer),
));
let store = Arc::new(InMemorySemanticCache::default());
let cached = SemanticCacheProvider::new(
    provider,
    store,
    embedder,
    SemanticCacheConfig::default()
        .threshold(0.92)
        .ttl(Duration::from_secs(60 * 60)),
);
```

## Demo

```
cargo run --example semantic_cache_demo -p synthia-semantic-cache
```

Expected tail: `OK: 60% hit-rate`.

## Design notes

- Cache decision lives on `ModelProvider::complete`; streaming
  passes through unchanged (the wire shape is `Stream<StreamChunk>`
  and a cache hit is a single terminal `IsDone`).
- Embedding key = `Sha256(model + tool_set_names + serialized
  messages)` plus the request's embedding vector. Lookup is k=1 ANN
  over the in-memory store (cosine ≥ `threshold`).
- TTL is wall-clock via `Clock`; tests inject `FixedClock`.
- No edits to `synthia-provider` — composition only.

See `crates/synthia-semantic-cache/src/lib.rs` for the full API.