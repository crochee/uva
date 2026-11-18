//! The arbitration every interception pass runs under.
//!
//! One rule, one place: **what a registration's decision means at a
//! mount point**. Two surfaces run registrations — the external
//! [`HookRegistry`](crate::HookRegistry) and the harness's typed host
//! — and they used to answer that question three times, in three
//! shapes, with three different answers:
//!
//! - a gate that could not answer ended the run on the registry's
//!   path and was downgraded to "allow" on the host's;
//! - a registration that panicked was withdrawn on the host's path
//!   and kept on the registry's;
//! - a decision the point did not accept was dropped silently on one
//!   path and reported on another.
//!
//! This module is the one answer. The surfaces apply it; they do not
//! restate it.
//!
//! ## The rule
//!
//! - **Legality** — a decision stands only where the point's table
//!   ([`HookEvent::accepts`]) accepts it. Elsewhere it is a no-op
//!   there: reported, never silently dropped.
//! - **Fail-closed synthesis** — a gate that cannot answer refuses
//!   the pass where the point can refuse: [`HookDecision::Block`]
//!   before a tool call (the model reads the refusal as the tool
//!   result), then [`HookDecision::Terminate`] at the points that can
//!   end a run. The points that accept no denial cannot refuse — the
//!   failure is reported instead ([`Refusal::Reported`]).
//! - **Panic policy** — a registration that panics is either kept
//!   ([`PanicPolicy::Keep`]: an external plugin may fail transiently,
//!   and it is not told it was disabled) or withdrawn
//!   ([`PanicPolicy::Withdraw`]: an in-process registration that
//!   panics is broken). Two policies, named here, so no surface can
//!   invent a third.
//! - **The fold** — deny-only and monotonic: once a decision denies,
//!   no later registration turns it back into permission
//!   ([`Arbitration::absorb`]).
//!
//! What a surface does with the rule's output is the surface's: the
//! registry reports through its pass, the host through its
//! diagnostics, and the harness writes the durable row.

use uva_steering::{HookDecision, HookEvent};

/// What one pass decided.
///
/// The `decision` is the one that won the pass: `Allow` when nothing
/// objected, `Block` / `Terminate` when a registration ended the
/// pass, `ReplaceParams` when a request was rewritten. The `asks` are
/// the approval reasons the registrations collected on the way — they
/// never end a pass.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Arbitration {
    /// The decision that won the pass.
    pub decision: HookDecision,
    /// The reasons the registrations asked for an approval.
    pub asks: Vec<String>,
}

impl Arbitration {
    /// Fold one decision into the pass.
    ///
    /// The fold is deny-only and monotonic — the terminal rule from
    /// dsh (`E004`, `E048`): an `Ask` joins the collected reasons, a
    /// `ReplaceParams` schedules a rewrite while the pass is still
    /// allowed, and a `Block` / `Terminate` is final. Once a denial is
    /// recorded, no later decision can clear it: there is no allow
    /// result to absorb, so registration order cannot turn a denial
    /// back into permission.
    pub fn absorb(&mut self, decision: HookDecision) {
        match decision {
            HookDecision::Allow => {}
            HookDecision::Ask { reason } => self.asks.push(reason),
            HookDecision::ReplaceParams(_) => {
                if matches!(self.decision, HookDecision::Allow) {
                    self.decision = decision;
                }
            }
            HookDecision::Block { .. } | HookDecision::Terminate { .. } => {
                if matches!(
                    self.decision,
                    HookDecision::Allow | HookDecision::ReplaceParams(_)
                ) {
                    self.decision = decision;
                }
            }
        }
    }

    /// Whether a decision ended the pass: a block or a termination.
    #[must_use]
    pub fn decided(&self) -> bool {
        matches!(
            self.decision,
            HookDecision::Block { .. } | HookDecision::Terminate { .. }
        )
    }
}

/// What one registration's decision means at one mount point.
#[derive(Debug, Clone, PartialEq)]
pub enum Ruling {
    /// The point accepts the decision: it stands.
    Stands(HookDecision),
    /// The point does not accept the decision: it is a no-op there.
    ///
    /// The decision does not apply; the fact says so, in the shape
    /// every surface reports.
    Unsupported {
        /// The decision that does not apply here.
        decision: HookDecision,
        /// What the surface reports.
        fact: Fact,
    },
}

/// Rule one registration's decision at one mount point.
///
/// The point's own table decides: a decision the point accepts
/// stands, one it does not accept is a no-op there. The caller
/// reports the no-op — it is a registration's misbehaviour, and the
/// rule does not decide what a surface does with that.
#[must_use]
pub fn rule(event: &HookEvent, decision: HookDecision, source: &str) -> Ruling {
    if event.accepts(&decision) {
        Ruling::Stands(decision)
    } else {
        Ruling::Unsupported {
            fact: Fact {
                kind: FactKind::Unsupported,
                source: source.to_owned(),
                detail: format!(
                    "`{source}` decided something `{}` does not accept",
                    uva_steering::event_name(event)
                ),
            },
            decision,
        }
    }
}

/// What a gate that could not answer means at one mount point.
#[derive(Debug, Clone)]
pub enum Refusal {
    /// The point can refuse: the pass ends with this decision.
    Decided(HookDecision),
    /// The point accepts no denial: the failure is reported instead.
    Reported(Fact),
}

/// Rule a gate that could not answer at one mount point.
///
/// The pass fails closed where the point can refuse: `Block` before a
/// tool call (the model reads the refusal as the tool result), then
/// `Terminate` at the points that can end a run. The points that
/// accept no denial cannot refuse — there is no decision to carry the
/// refusal — so the failure is reported instead.
///
/// The reason names the gate, so the refusal says which registration
/// could not answer.
#[must_use]
pub fn refusal(event: &HookEvent, source: &str, why: &str) -> Refusal {
    let reason = format!("gate `{source}` could not answer: {why}");
    let block = HookDecision::Block {
        reason: reason.clone(),
    };
    if event.accepts(&block) {
        return Refusal::Decided(block);
    }
    let terminate = HookDecision::Terminate { reason };
    if event.accepts(&terminate) {
        return Refusal::Decided(terminate);
    }
    Refusal::Reported(Fact {
        kind: FactKind::GateFailed,
        source: source.to_owned(),
        detail: format!(
            "gate `{source}` could not answer at `{}`: {why}",
            uva_steering::event_name(event)
        ),
    })
}

/// What a surface does with a registration that panicked.
///
/// Two policies, named in one place, so the two surfaces cannot drift
/// into a third.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanicPolicy {
    /// Skip the call and keep the registration.
    ///
    /// An external plugin may fail transiently, and nothing tells it
    /// that it was disabled — so a kept registration gets another
    /// chance.
    Keep,
    /// Withdraw the registration and report it.
    ///
    /// An in-process registration that panics is broken, and a broken
    /// registration that keeps running panics again.
    Withdraw,
}

impl PanicPolicy {
    /// What this policy does to the registration, in one line.
    #[must_use]
    pub fn consequence(&self) -> &'static str {
        match self {
            Self::Keep => "it stays registered",
            Self::Withdraw => "it is withdrawn",
        }
    }

    /// The fact a panicking registration leaves behind under this
    /// policy.
    #[must_use]
    pub fn fact(&self, source: &str, why: &str) -> Fact {
        Fact {
            kind: FactKind::Panicked,
            source: source.to_owned(),
            detail: format!(
                "`{source}` panicked: {why}; {}",
                self.consequence()
            ),
        }
    }
}

/// One fact about a registration's behaviour at one mount point.
///
/// A fact is what a surface reports: the registry hands them back
/// with its pass, the host records them in its diagnostics, and the
/// harness writes the ones that belong in the run's durable record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    /// What happened.
    pub kind: FactKind,
    /// The registration's name.
    pub source: String,
    /// What it means, in one line.
    pub detail: String,
}

impl Fact {
    /// The one line a surface reports.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.detail
    }
}

/// What a fact is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactKind {
    /// A gate could not answer at this point.
    GateFailed,
    /// The registration's decision is not accepted at this point.
    Unsupported,
    /// The registration panicked.
    Panicked,
}

impl FactKind {
    /// The name a surface reports the fact under.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::GateFailed => "gate failed",
            Self::Unsupported => "unsupported decision",
            Self::Panicked => "panicked",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn before_tool() -> HookEvent {
        HookEvent::BeforeTool {
            tool_name: "shell".to_owned(),
            call_id: "call-1".to_owned(),
            arguments: serde_json::Value::Null,
        }
    }

    fn before_step() -> HookEvent {
        HookEvent::BeforeStep {
            agent_name: "a".to_owned(),
            run_idx: 0,
            iteration: 1,
        }
    }

    fn before_request() -> HookEvent {
        HookEvent::BeforeRequest {
            provider: "test".to_owned(),
            params: uva_steering::RequestParams::from_request(
                &uva_provider::CompletionRequest::default(),
            ),
        }
    }

    #[test]
    fn a_block_stands_only_where_the_point_accepts_it() {
        let decision = HookDecision::Block {
            reason: "no".to_owned(),
        };
        assert!(matches!(
            rule(&before_tool(), decision.clone(), "h"),
            Ruling::Stands(HookDecision::Block { .. })
        ));
        match rule(&before_step(), decision, "h") {
            Ruling::Unsupported { decision, fact } => {
                assert!(matches!(decision, HookDecision::Block { .. }));
                assert_eq!(fact.kind, FactKind::Unsupported);
            }
            other => panic!("expected an unsupported ruling, got {other:?}"),
        }
    }

    #[test]
    fn a_gate_that_cannot_answer_refuses_the_tool_call() {
        let refusal = refusal(&before_tool(), "g", "the gate failed");
        match refusal {
            Refusal::Decided(HookDecision::Block { reason }) => {
                assert!(reason.contains("`g`"), "the reason names the gate");
            }
            other => panic!("expected a block, got {other:?}"),
        }
    }

    #[test]
    fn a_gate_that_cannot_answer_ends_the_run_where_it_can() {
        let refusal = refusal(&before_step(), "g", "the gate failed");
        match refusal {
            Refusal::Decided(HookDecision::Terminate { reason }) => {
                assert!(reason.contains("`g`"), "the reason names the gate");
            }
            other => panic!("expected a termination, got {other:?}"),
        }
    }

    #[test]
    fn a_gate_that_cannot_answer_is_reported_where_no_denial_exists() {
        let refusal = refusal(&before_request(), "g", "the gate failed");
        match refusal {
            Refusal::Reported(fact) => {
                assert_eq!(fact.kind, FactKind::GateFailed);
                assert!(fact.detail.contains("before_request"), "{fact:?}");
            }
            other => panic!("expected a report, got {other:?}"),
        }
    }

    #[test]
    fn the_fold_is_deny_only_and_monotonic() {
        let mut arbitration = Arbitration::default();
        arbitration.absorb(HookDecision::Block {
            reason: "no".to_owned(),
        });
        arbitration.absorb(HookDecision::Allow);
        assert!(arbitration.decided());
        assert!(matches!(arbitration.decision, HookDecision::Block { .. }));
    }
}
