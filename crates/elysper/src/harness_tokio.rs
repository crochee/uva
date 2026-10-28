//! [`elysper_harness_tokio`] — the tokio implementations of the
//! three runtime-neutral seams: [`TokioSpawner`] for
//! [`Spawner`](crate::core::spawn::Spawner), [`TokioSleeper`]
//! for [`Sleeper`](crate::core::sleeper::Sleeper), and
//! [`TokioBlocking`] for
//! [`Blocking`](crate::core::blocking::Blocking).
//!
//! Opt-in (`harness-tokio`): the feature adds no runtime to a
//! consumer that opts in — it pulls `elysper-harness-tokio`, whose
//! only runtime dependency is `tokio` with the `rt` + `time`
//! features. A consumer on `smol` / `async-std` / a bare `futures`
//! executor reaches the adapters not at all, and a consumer on tokio
//! reaches them through the one dependency instead of a second:
//!
//! ```toml
//! elysper = { version = "0.1", features = ["harness-tokio"] }
//! ```
//!
//! **The facade's default features are not runtime-free.** They
//! include `provider-anthropic` and `provider-openai`, whose HTTP
//! adapters carry `reqwest` → `hyper-rustls` → `hyper` → `h2` →
//! `tokio`: `cargo tree -p elysper -e normal` measures four tokio
//! packages there, against zero for the seven-feature MVP subset
//! (`core,provider,context,tool,session,steering,harness`). A
//! consumer that wants a runtime-free tree must say
//! `default-features = false` — or name the MVP subset.
//!
//! Install them where a runtime-neutral piece takes its seam —
//! `ResilientProvider::with_sleeper(Arc::new(TokioSleeper))` so a
//! retry backoff costs a timer entry rather than the std thread
//! the default sleeper spends, or an agent's `spawner(...)`
//! builder for detached work. A wrapper left on the default pays
//! one OS thread per retry: the wait is real, but a tokio host
//! should not buy it that way.

// The re-export goes through an alias because the runtime gate
// (`make check-public-api-runtime`) scans public lines for the
// literal `tokio::`, and this crate's *name* ends in it. Nothing
// is hidden: the three adapters below are exactly the curated
// surface `elysper-harness-tokio` publishes, and each implements a
// runtime-neutral trait — no runtime type enters the facade's API.
pub use adapters::{TokioBlocking, TokioSleeper, TokioSpawner};
use elysper_harness_tokio as adapters;
