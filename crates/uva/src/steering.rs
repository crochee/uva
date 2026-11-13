//! [`uva_steering`] — the interception vocabulary.
//!
//! What a policy *is*, with no wiring: [`Guard`] (fail-closed veto),
//! [`HookEvent`] / [`HookDecision`] (async interception), [`Hint`]
//! (advisory injection), [`tracker::Tracker`] (sync observation +
//! concurrency advice), and [`OutputTransformer`] (tool-output
//! rewrite), plus the built-in guards and hints.
//!
//! The registry that holds them is `uva::harness::Host`:
//! `host.guard(..)` / `host.hook(..)` / `host.hint(..)` /
//! `host.tracker(..)` / `host.output_transformer(..)`, and
//! `host.default_policy(workspace)` for the defensive set.

pub use uva_steering::*;
