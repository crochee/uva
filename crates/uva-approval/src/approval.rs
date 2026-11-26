//! The approval vocabulary: the risk a call carries, the request the
//! gate site builds when it holds a call, the policy that decides
//! which calls are held, and the token a suspended run resumes with.
//!
//! The risk lives in `uva-tool` (a tool declares its own), and
//! the request lives here: it is built at the gate site, where the
//! call's id, tool, arguments and agent are known. The steering
//! layer cannot name it — `uva-steering` does not depend on
//! `uva-tool` — so the vocabulary it carries is the bare
//! `HookDecision::Ask { reason }`.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub use uva_tool::Risk;

/// One held tool call, waiting for an approval decision.
///
/// The request is the unit an operator approves: it names the call,
/// the tool, a digest of the arguments the call was made with, and
/// the risk the tool declared. The digest is what lets a decision be
/// checked against the call it was made for — an approval for one
/// set of arguments is not an approval for another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalRequest {
    /// The request's identity — what an approval decision refers to.
    pub id: String,
    /// The provider's call id for the held call.
    pub call_id: String,
    /// The tool the call addresses.
    pub tool: String,
    /// A digest of the call's arguments.
    pub arguments_hash: String,
    /// The risk the tool declared for this call.
    pub risk: Risk,
    /// The agent the call belongs to.
    pub agent: String,
}

impl ApprovalRequest {
    /// Build the request the gate site holds a call with.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        call_id: impl Into<String>,
        tool: impl Into<String>,
        arguments_hash: impl Into<String>,
        risk: Risk,
        agent: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            call_id: call_id.into(),
            tool: tool.into(),
            arguments_hash: arguments_hash.into(),
            risk,
            agent: agent.into(),
        }
    }
}

/// Which calls a run stops to ask about.
///
/// The default is [`ApprovalMode::AskHighRisk`]: only a call the tool
/// classified as high risk is held, so a read-only tool is never
/// asked about. `overrides` narrows or widens the mode per tool;
/// `grants` are host pre-authorizations bound to one workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalPolicy {
    /// The run-wide default.
    pub mode: ApprovalMode,
    /// Whether an unclassified call is held.
    pub ask_unknown: bool,
    /// Per-tool overrides, keyed by the tool's name.
    pub overrides: BTreeMap<String, ApprovalMode>,
    /// Host grants: a matching tool + workspace is pre-approved.
    pub grants: BTreeSet<GrantKey>,
    /// How long a held call waits for an answer, in seconds.
    ///
    /// `None` waits forever: the ask stays open until an operator
    /// answers or the turn is cancelled. A deadline is the headless
    /// deployment's answer to the same question — a client that never
    /// comes back must not park the session forever. On expiry the
    /// held call is **denied** (the log records the decision, and the
    /// model reads a distinct "nobody answered" refusal), and the
    /// turn continues.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

impl Default for ApprovalPolicy {
    fn default() -> Self {
        Self {
            mode: ApprovalMode::AskHighRisk,
            ask_unknown: false,
            overrides: BTreeMap::new(),
            grants: BTreeSet::new(),
            timeout_secs: None,
        }
    }
}

// The approval stance for a call.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    // Hold a call whose risk is high (or unknown, when
    // `ApprovalPolicy::ask_unknown` is set).
    #[default]
    AskHighRisk,
    // Hold every call, including one a grant pre-approved: the
    // fail-closed stance cannot be narrowed.
    Always,
    // Hold nothing, including high-risk calls.
    Never,
}

// One host grant: a tool, the workspace it was granted for, and how
// long it lasts.
//
// There is no global, unbounded grant: a key always names a
// workspace, and `scope` says whether a replay can rebuild it
// ([`GrantScope::Session`]) or only deployment configuration can
// ([`GrantScope::Deployment`]).
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize,
)]
pub struct GrantKey {
    /// The granted tool.
    pub tool: String,
    /// The workspace the grant is bound to.
    pub workspace_hash: String,
    /// Whether a session row or deployment config created it.
    pub scope: GrantScope,
}

// How long a grant lasts.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum GrantScope {
    // Rebuildable from an `approval/granted` log row.
    Session,
    // Only from server configuration.
    Deployment,
}

impl ApprovalPolicy {
    /// The mode that applies to `tool`.
    #[must_use]
    pub fn mode_for(&self, tool: &str) -> ApprovalMode {
        self.overrides.get(tool).copied().unwrap_or(self.mode)
    }

    /// How long a held call waits for an answer.
    ///
    /// `None` means the ask waits forever — the deployment's default,
    /// and the right one for a deployment where an operator is always
    /// watching.
    #[must_use]
    pub fn timeout(&self) -> Option<std::time::Duration> {
        self.timeout_secs.map(std::time::Duration::from_secs)
    }

    /// Whether `tool` is pre-approved for `workspace_hash`.
    #[must_use]
    pub fn is_granted(&self, tool: &str, workspace_hash: &str) -> bool {
        self.grants.iter().any(|grant| {
            grant.tool == tool && grant.workspace_hash == workspace_hash
        })
    }

    /// Whether a call of `risk` to `tool` must be held.
    ///
    /// A grant wins over the mode: the host already approved this
    /// tool for this workspace. `Always` cannot be narrowed by a
    /// grant — it is the fail-closed stance — so the grant check
    /// only applies to the other modes.
    #[must_use]
    pub fn requires_approval(
        &self,
        tool: &str,
        risk: Risk,
        workspace_hash: &str,
    ) -> bool {
        match self.mode_for(tool) {
            ApprovalMode::Never => false,
            ApprovalMode::Always => true,
            ApprovalMode::AskHighRisk => {
                let asks = match risk {
                    Risk::High => true,
                    Risk::Unknown => self.ask_unknown,
                    Risk::Low => false,
                };
                asks && !self.is_granted(tool, workspace_hash)
            }
        }
    }
}

/// Fold a log's session-scope grants into `policy`.
///
/// Deployment-scope grants never come from a log row (only server
/// configuration creates them), so a row is honoured only when its
/// scope says `session`.
pub fn merge_grants(policy: &mut ApprovalPolicy, rows: &[Value]) {
    for row in rows {
        // Decode only the rows that can carry a grant: a log holds
        // thousands of other rows, and a `serde` decode of each one
        // is the expensive half of this fold. What a row is comes
        // from the vocabulary, not from a spelling held here.
        let restored = uva_session::restore_row(row);
        if !restored.restores_to(uva_session::RowTag::ApprovalGranted) {
            continue;
        }
        let Some(event) = restored.event() else {
            continue;
        };
        match event {
            uva_session::SessionEvent::ApprovalGranted {
                tool,
                scope,
                workspace_hash,
                ..
            } if scope == "session" => {
                policy.grants.insert(GrantKey {
                    tool,
                    workspace_hash,
                    scope: GrantScope::Session,
                });
            }
            _ => {}
        }
    }
}

/// Whether a resume decision's payload asks for a session-scope
/// grant (one a log replay can rebuild, as opposed to a
/// deployment-scope grant only server configuration creates).
pub fn wants_session_grant(payload: Option<&Value>) -> bool {
    payload
        .and_then(|payload| payload.get("scope"))
        .and_then(Value::as_str)
        == Some("session")
}

/// The token that resumes a suspended run.
///
/// It is derived from the log, not from memory: `last_seq` is the
/// tail the caller saw, `pending` is the set of call ids still
/// waiting for a decision, and `request_snapshot_hash` binds the
/// resume to the request envelope that was suspended.
///
/// A log-only reader derives the same token without an in-memory
/// handoff: `last_seq` is the maximum row ordinal, `pending` is the
/// `approval_asked` call ids of the last suspended turn minus those
/// with an `approval_decided` row, and `request_snapshot_hash` is the
/// fingerprint of the last `request_snapshot` row (provider, model,
/// parameter/tool/system/policy hashes and section ids; seq and ts
/// excluded).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResumeToken {
    /// The tail ordinal the suspension ended at.
    pub last_seq: u64,
    /// Call ids still awaiting a decision, sorted for comparison.
    pub pending: Vec<String>,
    /// Fingerprint of the request envelope at suspension time.
    pub request_snapshot_hash: String,
}

impl ResumeToken {
    /// Build a token with a canonical (sorted) pending set.
    #[must_use]
    pub fn new(
        last_seq: u64,
        mut pending: Vec<String>,
        request_snapshot_hash: impl Into<String>,
    ) -> Self {
        pending.sort();
        pending.dedup();
        Self {
            last_seq,
            pending,
            request_snapshot_hash: request_snapshot_hash.into(),
        }
    }
}

/// Why a run stopped without finishing its work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Suspension {
    // High-risk calls are held; a resume has to decide every one.
    AwaitingApproval {
        // The calls waiting for a decision.
        interrupts: Vec<ApprovalRequest>,
        // How to resume.
        resume: ResumeToken,
    },
}

/// Whether a call whose outcome was lost may be retried.
///
/// Only a read-only, idempotent call is safe to run again: any other
/// call may have produced its side effect before the process died,
/// so the outcome stays unknown and the call is never auto-rerun.
#[must_use]
pub fn may_retry(risk: Risk, idempotent_hint: Option<bool>) -> bool {
    risk == Risk::Low && idempotent_hint == Some(true)
}

/// The operator's answer to one held call.
///
/// [`ResumeStatus::Resolved`] lets the call execute;
/// [`ResumeStatus::Cancelled`] and [`ResumeStatus::Rejected`] record
/// the decision and write a model-visible `is_error` tool result
/// instead of executing it. [`ResumeStatus::TimedOut`] is the
/// deadline's own answer: nobody replied, so the call did not run —
/// but the model is told *that*, not that an operator said no.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ResumeStatus {
    Resolved,
    Cancelled,
    Rejected,
    /// Nobody answered before the deadline.
    ///
    /// The durable row says `denied` — the log's vocabulary has no
    /// fourth answer — while the model-visible text names the
    /// deadline, so the model can tell "the operator said no" from
    /// "nobody was there".
    TimedOut,
}

/// One decision from the client. `Resolved` approves this call;
/// `Cancelled`/`Rejected` writes the model-visible is_error result
/// instead of executing it. The resume must cover every open
/// interrupt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResumeDecision {
    /// `ApprovalRequest::id` (or its `call_id`) this answers.
    pub id: String,
    /// What the operator decided.
    pub status: ResumeStatus,
    /// Decision detail (a denial reason, a grant scope, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

impl ResumeDecision {
    /// The status the durable `approval_decided` row carries.
    ///
    /// The log's vocabulary is `approved` / `denied` / `cancelled`, so
    /// a wire-level `rejected` — and a deadline's `timed_out` — are
    /// recorded as `denied`.
    #[must_use]
    pub fn durable_status(&self) -> &'static str {
        match self.status {
            ResumeStatus::Resolved => "approved",
            ResumeStatus::Cancelled => "cancelled",
            ResumeStatus::Rejected | ResumeStatus::TimedOut => "denied",
        }
    }

    /// Whether the held call executes.
    #[must_use]
    pub fn executes(&self) -> bool {
        self.status == ResumeStatus::Resolved
    }

    /// The model-visible text of a result for a call that did not run.
    #[must_use]
    pub fn denial_message(&self) -> String {
        let detail = self.payload.as_ref().and_then(|payload| {
            payload
                .as_str()
                .or_else(|| payload.get("reason").and_then(Value::as_str))
                .map(str::to_string)
        });
        // The deadline is not the operator: the text says so, because
        // "nobody answered" and "somebody said no" lead the model to
        // different next moves (retry later vs. try another way).
        if self.status == ResumeStatus::TimedOut {
            return match detail {
                Some(reason) => format!(
                    "tool call timed out before an operator answered; it \
                     did not run: {reason}"
                ),
                None => "tool call timed out before an operator answered; \
                         it did not run"
                    .to_string(),
            };
        }
        let verb = match self.status {
            ResumeStatus::Resolved => "approved",
            ResumeStatus::Cancelled => "cancelled",
            ResumeStatus::Rejected => "rejected",
            ResumeStatus::TimedOut => "timed out",
        };
        match detail {
            Some(reason) => {
                format!(
                    "tool call {verb} by the operator; it did not run: {reason}"
                )
            }
            None => {
                format!("tool call {verb} by the operator; it did not run")
            }
        }
    }
}

/// Why a resume's decisions do not answer the open interrupts.
///
/// [`ResumeCoverageError::Missing`] names an interrupt left undecided,
/// [`ResumeCoverageError::Duplicate`] a call answered twice, and
/// [`ResumeCoverageError::Unknown`] an id the log does not hold open.
/// Each carries the offending id, so the wire diagnostic names it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResumeCoverageError {
    #[error("resume does not answer interrupt {id}")]
    Missing { id: String },
    #[error("resume decides {id} more than once")]
    Duplicate { id: String },
    #[error("resume decides {id}, which is not an open interrupt")]
    Unknown { id: String },
}

/// Check that `decisions` answer exactly the `open` requests, and
/// return the statuses in request order.
///
/// An empty `decisions` list is *not* a wildcard: partial coverage is
/// refused, because an undecided call must not inherit another call's
/// answer. A caller that means "approve everything" says so with one
/// `Resolved` decision per interrupt.
///
/// # Errors
///
/// Returns [`ResumeCoverageError`] when a request is undecided, a
/// decision is repeated, or a decision names an unknown interrupt.
pub fn check_coverage(
    open: &[ApprovalRequest],
    decisions: &[ResumeDecision],
) -> Result<Vec<ResumeStatus>, ResumeCoverageError> {
    Ok(align_decisions(open, decisions)?
        .into_iter()
        .map(|decision| decision.status)
        .collect())
}

/// Answer each open request with the decision that names it, in
/// request order.
///
/// The wire carries a list, but its order is the client's, not the
/// log's: a decision is bound to a request **by id**, never by
/// position. An answer for one call must never authorize another.
///
/// # Errors
///
/// Returns [`ResumeCoverageError`] when a request is undecided, a
/// decision is repeated, or a decision names an unknown interrupt.
pub fn align_decisions(
    open: &[ApprovalRequest],
    decisions: &[ResumeDecision],
) -> Result<Vec<ResumeDecision>, ResumeCoverageError> {
    let mut answered: Vec<Option<ResumeDecision>> = vec![None; open.len()];
    for decision in decisions {
        let index = open
            .iter()
            .position(|request| {
                request.id == decision.id || request.call_id == decision.id
            })
            .ok_or_else(|| ResumeCoverageError::Unknown {
                id: decision.id.clone(),
            })?;
        if answered[index].replace(decision.clone()).is_some() {
            return Err(ResumeCoverageError::Duplicate {
                id: decision.id.clone(),
            });
        }
    }
    let mut aligned = Vec::with_capacity(open.len());
    for (index, request) in open.iter().enumerate() {
        let decision = answered[index].take().ok_or_else(|| {
            ResumeCoverageError::Missing {
                id: request.id.clone(),
            }
        })?;
        // Canonicalise the id to the request's, so the durable row and
        // the execution plan cannot disagree about which interrupt
        // was answered.
        aligned.push(ResumeDecision {
            id: request.id.clone(),
            ..decision
        });
    }
    Ok(aligned)
}

/// The approval requests still open in `rows`: the `approval_asked`
/// calls of the last suspended turn minus those with an
/// `approval_decided` row.
///
/// This is the log-only derivation the server and the loop share: the
/// paused state is a property of the log, not of any in-memory flag.
#[must_use]
pub fn open_requests(rows: &[Value]) -> Vec<ApprovalRequest> {
    let Some(segment) = suspended_segment(rows) else {
        return Vec::new();
    };
    let mut asked: BTreeMap<String, ApprovalRequest> = BTreeMap::new();
    let mut decided: BTreeSet<String> = BTreeSet::new();
    for row in segment {
        let Some(event) = uva_session::SessionEvent::from_value(row) else {
            continue;
        };
        match event {
            uva_session::SessionEvent::ApprovalAsked {
                id,
                call_id,
                tool,
                arguments_hash,
                risk,
                agent,
                ..
            } => {
                asked.insert(
                    call_id.clone(),
                    ApprovalRequest::new(
                        id,
                        call_id,
                        tool,
                        arguments_hash,
                        risk_from_str(&risk),
                        agent,
                    ),
                );
            }
            uva_session::SessionEvent::ApprovalDecided {
                id, call_id, ..
            } => {
                if let Some((key, _)) = asked.iter().find(|(_, request)| {
                    request.id == id || request.call_id == call_id
                }) {
                    decided.insert(key.clone());
                }
            }
            _ => {}
        }
    }
    asked
        .into_iter()
        .filter(|(call_id, _)| !decided.contains(call_id))
        .map(|(_, request)| request)
        .collect()
}

/// The fingerprint of the log's newest `request_snapshot` row, or
/// `None` when the log has none.
#[must_use]
pub fn latest_request_snapshot_hash(rows: &[Value]) -> Option<String> {
    rows.iter().rev().find_map(|row| {
        uva_session::SessionEvent::from_value(row)
            .as_ref()
            .and_then(request_snapshot_hash)
    })
}

/// The fingerprint a `request_snapshot` row stands for: provider,
/// model and every hash, with seq and ts excluded.
#[must_use]
pub fn request_snapshot_hash(
    event: &uva_session::SessionEvent,
) -> Option<String> {
    let uva_session::SessionEvent::RequestSnapshot {
        provider,
        model,
        params_hash,
        tools_hash,
        system_hash,
        sections,
        policy_hash,
        ..
    } = event
    else {
        return None;
    };
    Some(fingerprint(&format!(
        "{provider}|{model}|{params_hash}|{tools_hash}|{system_hash}|{}|{policy_hash}",
        sections.join(",")
    )))
}

fn fingerprint(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

// The rows of the last suspended turn: after the previous turn end,
// through the turn end whose reason is `suspended`.
pub fn suspended_segment(rows: &[Value]) -> Option<&[Value]> {
    let mut last_end = None;
    let mut suspended = false;
    for (idx, row) in rows.iter().enumerate() {
        let Some(event) = uva_session::SessionEvent::from_value(row) else {
            continue;
        };
        let uva_session::SessionEvent::Turn { data, .. } = &event else {
            continue;
        };
        if event.boundary_kind() != Some("end") {
            continue;
        }
        last_end = Some(idx);
        suspended = data.get("reason").and_then(Value::as_str)
            == Some("suspended")
            || data.get("status").and_then(Value::as_str) == Some("suspended");
    }
    let end = last_end?;
    if !suspended {
        return None;
    }
    let start = rows[..end]
        .iter()
        .rposition(|row| {
            uva_session::SessionEvent::from_value(row).is_some_and(|event| {
                matches!(event, uva_session::SessionEvent::Turn { .. })
                    && event.boundary_kind() == Some("end")
            })
        })
        .map(|idx| idx + 1)
        .unwrap_or(0);
    // The segment runs to the log tail: a resume appends its decision
    // and dispatch rows AFTER the suspended turn end, and those rows
    // are exactly the crash window this scan must see.
    Some(&rows[start..])
}

fn risk_from_str(risk: &str) -> Risk {
    match risk {
        "low" => Risk::Low,
        "high" => Risk::High,
        _ => Risk::Unknown,
    }
}

/// A stable workspace identity for grant matching.
#[must_use]
pub fn workspace_hash(path: impl AsRef<std::path::Path>) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(path.as_ref().to_string_lossy().as_bytes());
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(tool: &str, workspace: &str) -> GrantKey {
        GrantKey {
            tool: tool.to_string(),
            workspace_hash: workspace.to_string(),
            scope: GrantScope::Session,
        }
    }

    #[test]
    fn the_request_names_the_call_it_holds() {
        let request = ApprovalRequest::new(
            "req-1",
            "call-1",
            "shell",
            "sha256:abc",
            Risk::High,
            "default",
        );
        assert_eq!(request.id, "req-1");
        assert_eq!(request.call_id, "call-1");
        assert_eq!(request.tool, "shell");
        assert_eq!(request.arguments_hash, "sha256:abc");
        assert_eq!(request.risk, Risk::High);
        assert_eq!(request.agent, "default");
    }

    #[test]
    fn the_deadline_denies_the_call_and_says_so() {
        // The durable row says `denied` — the log's vocabulary has no
        // fourth answer — while the model reads a refusal that names
        // the deadline instead of an operator's "no".
        let decision = ResumeDecision {
            id: "a1".to_string(),
            status: ResumeStatus::TimedOut,
            payload: Some(serde_json::json!({
                "reason": "no decision within the 300s approval deadline",
            })),
        };
        assert_eq!(decision.durable_status(), "denied");
        assert!(!decision.executes());
        let message = decision.denial_message();
        assert!(
            message.contains("timed out"),
            "the model is told the deadline, not an operator: {message}"
        );
        assert!(
            !message.contains("by the operator"),
            "nobody answered: {message}"
        );
        assert!(
            message.contains("300s"),
            "and the deadline is named: {message}"
        );
    }

    #[test]
    fn the_policy_carries_its_deadline() {
        assert_eq!(ApprovalPolicy::default().timeout(), None);
        let policy = ApprovalPolicy {
            timeout_secs: Some(300),
            ..ApprovalPolicy::default()
        };
        assert_eq!(policy.timeout(), Some(std::time::Duration::from_secs(300)));
    }

    #[test]
    fn the_risk_is_the_tools_own_vocabulary() {
        // The re-export is the same type the tool crate exports, so
        // a tool's declaration and the harness's request agree.
        assert_eq!(Risk::default(), uva_tool::Risk::Unknown);
    }

    #[test]
    fn read_only_tools_are_never_asked_about() {
        let policy = ApprovalPolicy::default();
        assert!(
            !policy.requires_approval("read", Risk::Low, "ws"),
            "a low-risk call runs without an approval"
        );
        assert!(
            policy.requires_approval("shell", Risk::High, "ws"),
            "the default holds high-risk calls"
        );
        assert!(
            !policy.requires_approval("custom", Risk::Unknown, "ws"),
            "unknown is not asked about by default"
        );

        let unknown_policy = ApprovalPolicy {
            ask_unknown: true,
            ..ApprovalPolicy::default()
        };
        assert!(unknown_policy.requires_approval(
            "custom",
            Risk::Unknown,
            "ws"
        ));

        let never = ApprovalPolicy {
            mode: ApprovalMode::Never,
            ..ApprovalPolicy::default()
        };
        assert!(!never.requires_approval("shell", Risk::High, "ws"));

        let overridden = ApprovalPolicy {
            overrides: BTreeMap::from([(
                "shell".to_string(),
                ApprovalMode::Never,
            )]),
            ..ApprovalPolicy::default()
        };
        assert!(!overridden.requires_approval("shell", Risk::High, "ws"));
        assert!(overridden.requires_approval("write", Risk::High, "ws"));

        let always = ApprovalPolicy {
            mode: ApprovalMode::Always,
            ..ApprovalPolicy::default()
        };
        assert!(always.requires_approval("read", Risk::Low, "ws"));
    }

    #[test]
    fn a_grant_covers_only_its_tool_and_workspace() {
        let policy = ApprovalPolicy {
            grants: BTreeSet::from([grant("shell", "ws")]),
            ..ApprovalPolicy::default()
        };
        assert!(!policy.requires_approval("shell", Risk::High, "ws"));
        assert!(policy.requires_approval("shell", Risk::High, "elsewhere"));
        assert!(policy.requires_approval("write", Risk::High, "ws"));
    }

    #[test]
    fn only_a_low_idempotent_call_may_be_retried() {
        assert!(may_retry(Risk::Low, Some(true)));
        assert!(!may_retry(Risk::Low, Some(false)));
        assert!(!may_retry(Risk::Low, None));
        assert!(!may_retry(Risk::High, Some(true)));
        assert!(!may_retry(Risk::Unknown, Some(true)));
    }

    #[test]
    fn a_resume_token_sorts_its_pending_calls() {
        let token = ResumeToken::new(
            7,
            vec!["c2".to_string(), "c1".to_string(), "c1".to_string()],
            "fp",
        );
        assert_eq!(token.pending, vec!["c1", "c2"]);
        assert_eq!(token.last_seq, 7);
        assert_eq!(token.request_snapshot_hash, "fp");
    }

    #[test]
    fn a_suspension_round_trips_with_its_token() {
        let suspension = Suspension::AwaitingApproval {
            interrupts: vec![ApprovalRequest::new(
                "a1",
                "c1",
                "shell",
                "sha256:x",
                Risk::High,
                "default",
            )],
            resume: ResumeToken::new(9, vec!["c1".into()], "fp"),
        };
        let json = serde_json::to_string(&suspension).expect("serialises");
        let back: Suspension =
            serde_json::from_str(&json).expect("deserialises");
        assert_eq!(back, suspension);
    }

    #[test]
    fn merge_grants_folds_session_rows_only() {
        let mut policy = ApprovalPolicy::default();
        let rows = vec![
            serde_json::to_value(uva_session::approval_granted(
                "g1", "shell", "session", "ws",
            ))
            .expect("grant row"),
            serde_json::to_value(uva_session::approval_granted(
                "g2",
                "shell",
                "deployment",
                "ws",
            ))
            .expect("grant row"),
        ];
        merge_grants(&mut policy, &rows);
        assert!(policy.is_granted("shell", "ws"));
        assert_eq!(
            policy.grants.len(),
            1,
            "a deployment row is not a log-replayable grant"
        );
        assert_eq!(
            policy.grants.iter().next().expect("one grant").scope,
            GrantScope::Session
        );
        assert!(wants_session_grant(Some(&serde_json::json!({
            "scope": "session"
        }))));
        assert!(!wants_session_grant(Some(&serde_json::json!({
            "scope": "deployment"
        }))));
        assert!(!wants_session_grant(None));
    }
}
