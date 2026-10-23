//! [`synthia_safety`] — defense-in-depth scanning plug-in.
//!
//! The `safety` feature is opt-in (not in the default set): pulling
//! it in adds the `synthia-safety` crate, which ships `SafetyScanner`
//! implementations for four stages (`Input` / `ToolCall` /
//! `ToolOutput` / `Output`). The crate ships scanners, not loop
//! wiring: a consumer builds a
//! [`HookMap`](synthia::steering::HookMap) with
//! `CompositeScanner::hook_map()` and installs it on
//! `Steering::hooks`. The harness loop itself calls no scanner.
//!
//! ```rust,ignore
//! use std::sync::Arc;
//!
//! use synthia::safety::{CompositeScanner, RegexInjectionScanner, ScanStage};
//! use synthia::steering::Steering;
//!
//! let scanner = Arc::new(
//!     CompositeScanner::new()
//!         .with_stage(ScanStage::Input, Arc::new(RegexInjectionScanner::default())),
//! );
//! let steering = Steering {
//!     hooks: scanner.hook_map(),
//!     ..Steering::noop()
//! };
//! ```

pub use synthia_safety::*;
