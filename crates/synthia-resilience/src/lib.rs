//! # synthia-resilience
//!
//! Circuit breaker + adaptive backoff + jitter per provider or tool.
//!
//! [`ResilientProvider`] / [`ResilientTool`] wrap any
//! [`synthia_provider::ModelProvider`] / [`synthia_tool::Tool`] and
//! drive a `classify → retry → breaker` pipeline. The breaker
//! state machine itself lives in
//! [`synthia_core::breaker`] so this crate
//! is runtime-neutral.
//!
//! ```
//! use std::sync::Arc;
//!
//! use synthia_resilience::{
//!     BreakerConfig,
//!     ResiliencePolicy,
//!     ResilientProvider,
//! };
//!
//! let provider: Arc<dyn synthia_provider::ModelProvider> =
//!     Arc::new(synthia_provider::traits_stub::ModelProviderStub::default());
//! let resilient = ResilientProvider::new(
//!     provider,
//!     ResiliencePolicy::default()
//!         .max_attempts(5)
//!         .breaker(BreakerConfig::default().failure_threshold(5)),
//!     Arc::new(synthia_core::SystemClock),
//! );
//! ```
//!
//! ## Layering
//!
//! ```text
//!   caller
//!     └─ ResilientProvider  ← classify(err) → Retry | Open | GiveUp
//!          └─ inner provider
//! ```
//!
//! `ResilientProvider::complete` is the only method that drives the
//! policy; `complete_with_stream` is pass-through (the agent loop's
//! streaming path is the policy-free surface; the cache / breaker
//! decisions all happen on the `complete` boundary).

pub mod classify;
mod provider;
mod tool;

pub use classify::{
    ClassifyFn,
    DefaultClassifier,
    Outcome as ClassifyOutcome,
    default_classifier,
};
pub use provider::{
    Jitter,
    ResiliencePolicy,
    ResilientProvider,
    retry_with_backoff,
};
pub use synthia_core::breaker::{
    BreakerConfig,
    BreakerObservation,
    BreakerPhase,
    BreakerState,
    Outcome,
};
use thiserror::Error;
pub use tool::ResilientTool;

/// Errors surfaced by the wrapper.
#[derive(Debug, Error)]
pub enum ResilienceError {
    /// The breaker is `Open` and the call was rejected without
    /// consulting the inner provider.
    #[error("circuit open: call rejected by breaker")]
    Open,
    /// All `max_attempts` retries returned `Retry`-class outcomes
    /// without success.
    #[error("retries exhausted after {attempts} attempts: {source}")]
    Exhausted {
        attempts: u32,
        #[source]
        source: synthia_core::Error,
    },
    /// The classifier mapped the error to `GiveUp` and the call was
    /// rejected without retry.
    #[error("classifier refused retry: {0}")]
    GiveUp(synthia_core::Error),
    /// The inner provider / tool surfaced an error.
    #[error("inner error: {0}")]
    Inner(#[from] synthia_core::Error),
}

pub type Result<T, E = ResilienceError> = core::result::Result<T, E>;

#[cfg(test)]
mod tests;
