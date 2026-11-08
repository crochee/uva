//! # elysper-reflection
//!
//! Post-execution reflection / self-critique hook.
//!
//! [`ReflectionHook`] implements `elysper_harness::agent::host::Hook`:
//! install it with `host.hook(Arc::new(hook))`, and its `after_tool`
//! observer asks a **critic model** (cheap + fast) to critique the
//! running trajectory every `every_n_steps` tool calls. If the
//! critique flags drift (severity ≥ `Warn`), the hook stages a
//! [`Hint`] that the next LLM call surfaces to the model.
//! Reflexion / Self-Refine style.
//!
//! ```rust,ignore
//! use std::sync::Arc;
//!
//! use elysper_core::SharedClock;
//! use elysper_provider::ModelProvider;
//! use elysper_reflection::{ReflectorConfig, ReflectionHook};
//! use elysper_harness::agent::host::Host;
//!
//! // The critic is the consumer's own provider: this crate takes the
//! // `ModelProvider` trait, not one of `elysper-provider`'s HTTP
//! // adapters.
//! let critic: Arc<dyn ModelProvider> = todo!("your cheap, fast critic");
//! let hook = Arc::new(ReflectionHook::new(
//!     ReflectorConfig::default()
//!         .critic_provider(critic)
//!         .every_n_steps(3),
//!     Arc::new(SharedClock::system()),
//! ));
//! let mut host = Host::default_policy(root);
//! let _registration = host.hook(hook);
//! ```

pub mod critique;
mod hook;

pub use critique::{CriticProvider, Critique, Severity};
// Re-exports for ergonomic plumbing.
pub use elysper_core::Clock;
pub use elysper_provider::ModelProvider;
pub use elysper_steering::{
    Hint,
    HintMessage,
    HintPriority,
    HookDecision,
    HookEvent,
    InjectionPoint,
    OutputTransformer,
};
pub use hook::{ReflectionHook, ReflectiveOutputTransformer, ReflectorConfig};

#[cfg(test)]
mod tests;
