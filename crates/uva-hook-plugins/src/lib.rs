//! # uva-hook-plugins
//!
//! The shipped **hook implementations** — one module and one feature
//! per hook. Every module implements a trait from `uva_hook`, the
//! interception contract that lives below the agent loop:
//!
//! | feature | module | what the hook does |
//! |---|---|---|
//! | `reflection` | `reflection` | critique the trajectory and feed a hint back |
//! | `redact` | `redact` | rewrite a tool result before the loop commits it |
//! | `traffic-light` | `traffic_light` | block a tool call above a risk threshold |
//!
//! ## What is not here
//!
//! The built-in five steering guards (workspace confinement, loop
//! detection, …) live in `uva-steering::guards`; the loop installs
//! them once at boot through `Host::default_policy`. They are not
//! mountable hooks and do not ship in this family.
//!
//! ## The default build ships no hook
//!
//! `default = []`: a consumer opens exactly the hook it wants, and
//! the hook's own dependencies open with that feature.
//!
//! ## Registration
//!
//! A hook is registered through the contract crate, not through
//! this one:
//!
//! ```rust,ignore
//! use std::sync::Arc;
//!
//! use uva_hook::{HookRegistry, seam};
//! use uva_hook_plugins::redact::RedactTransformer;
//! use uva_tool::registry::ToolRegistry;
//!
//! let mut host = uva_harness::Host::new();
//! # let registry = ToolRegistry::new();
//! let _registration = host.result_transformer(Arc::new(
//!     RedactTransformer::default(),
//! ));
//! ```

#[cfg(feature = "redact")]
pub use redact::{RedactConfig, RedactTransformer, RedactionRule};
#[cfg(feature = "reflection")]
pub use reflection::{
    CriticProvider,
    Critique,
    ReflectionHook,
    ReflectiveOutputTransformer,
    ReflectorConfig,
    Severity,
};
#[cfg(feature = "traffic-light")]
pub use traffic_light::{TrafficLight, TrafficPolicy};

#[cfg(feature = "redact")]
pub mod redact;
#[cfg(feature = "reflection")]
pub mod reflection;
#[cfg(feature = "traffic-light")]
pub mod traffic_light;
