//! # synthia-reflection
//!
//! Post-execution reflection / self-critique hook.
//!
//! A [`ReflectionHook`] produces a `synthia_steering::HookMap`
//! (installed on `Steering::hooks`) whose `after_tool` handler,
//! every `every_n_steps` tool calls, asks a **critic model**
//! (cheap + fast) to critique the running trajectory. If the
//! critique flags drift (severity ≥ `Warn`), the hook stages a
//! [`Hint`] that the next LLM call surfaces to the model.
//! Reflexion / Self-Refine style.
//!
//! ```rust,ignore
//! use std::sync::Arc;
//!
//! use synthia_core::SharedClock;
//! use synthia_provider::ModelProvider;
//! use synthia_reflection::{ReflectorConfig, ReflectionHook};
//! use synthia_steering::Steering;
//!
//! // The critic is the consumer's own provider: this crate takes the
//! // `ModelProvider` trait, not one of `synthia-provider`'s HTTP
//! // adapters.
//! let critic: Arc<dyn ModelProvider> = todo!("your cheap, fast critic");
//! let hook = Arc::new(ReflectionHook::new(
//!     ReflectorConfig::default()
//!         .critic_provider(critic)
//!         .every_n_steps(3),
//!     Arc::new(SharedClock::system()),
//! ));
//! let steering = Steering::default_policy(root).add_hooks(hook.hook_map());
//! ```

pub mod critique;
mod hook;

pub use critique::{CriticProvider, Critique, Severity};
pub use hook::{ReflectionHook, ReflectiveOutputTransformer, ReflectorConfig};
// Re-exports for ergonomic plumbing.
pub use synthia_core::Clock;
pub use synthia_provider::ModelProvider;
pub use synthia_steering::{
    Hint,
    HintMessage,
    HintPriority,
    HookDecision,
    HookEvent,
    InjectionPoint,
    OutputTransformer,
};

#[cfg(test)]
mod tests;
