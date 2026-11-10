//! The enablement-judge vocabulary shared by the host and plugins.
//!
//! A profile row may delegate its enablement decision to a plugin: the
//! row writes `enabled = { service, check, arg }` and the host resolves
//! an `Arc<dyn EnableChecks>` under `service` at the assembly boundary,
//! then asks the trait's `check` method.
//!
//! The contract is deliberately one method. The host owns the profile
//! vocabulary, the judge owns the domain fact (is this skill enabled in
//! its frontmatter, is this server reachable, …); keeping the trait
//! here means neither side has to depend on the other.

use serde_json::Value;

/// A plugin-provided enablement judge.
///
/// The host treats every answer other than `Some(true)` conservatively:
/// `Some(false)` means the row stays out, and `None` (unknown method,
/// unknown subject) is *unavailable*, which the host also reads as
/// "off" — a delegated open/close decision fails closed when its judge
/// cannot answer.
pub trait EnableChecks: Send + Sync {
    /// Answer one check.
    ///
    /// `None` means "not mine / unknown" — the host treats that as
    /// unavailable (fail-closed).
    fn check(&self, method: &str, arg: &Value) -> Option<bool>;
}
