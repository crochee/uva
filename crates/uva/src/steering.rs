//! [`uva_steering`] — the interception vocabulary.
//!
//! What a policy *is*, with no wiring: [`Guard`] (fail-closed veto),
//! [`HookEvent`] / [`HookDecision`] (async interception), [`Hint`]
//! (advisory injection), [`tracker::Tracker`] (sync observation +
//! concurrency advice), and [`OutputTransformer`] (tool-output
//! rewrite), plus the built-in guards and hints.
//!
//! Interception registrations live on `uva_hook::HookRegistry`:
//! `registry.register_hook_at(..)` / `register_gate_at(..)` /
//! `register_request_transformer_at(..)` /
//! `register_result_transformer_at(..)` / `register_prompt_section(..)`
//! cover the twelve mount points. The typed in-process policy —
//! `host.guard(..)` / `host.hint(..)` / `host.tracker(..)` /
//! `host.output_transformer(..)` / `host.tool(..)` — stays on
//! `uva::harness::Host`, whose `default_policy(workspace, registry)`
//! installs the defensive set over the deployment's table.

pub use uva_steering::*;
