//! The session surface: the listing, the detail, the durable
//! transcript, the follow stream, resume, compaction and usage.
use serde::Deserialize;
use serde_json::Value;
use validator::Validate;

use super::{parse, parse_raw};
use crate::{
    api::SessionPageQuery,
    routes::{
        follow::{self, FollowQuery, InterruptDecision, ResumeRequest},
        session_search::{self, SessionSearchParams},
        sessions::{self, ForkSessionRequest},
    },
    rpc::{RpcRouter, encode},
    state::AppState,
};

/// `sessions/fork` — the session id beside the request body.
#[derive(Deserialize, validator::Validate)]
struct ForkParams {
    id: String,
    #[serde(flatten)]
    request: ForkSessionRequest,
}
impl ForkParams {
    fn parse(raw: Value) -> Result<Self, crate::rpc::RpcError> {
        let parsed: Self = parse_raw(raw)?;
        parsed.request.validate()?;
        Ok(parsed)
    }
}
/// `sessions/resume` — the session id beside the request body.
#[derive(Deserialize, validator::Validate)]
struct ResumeParams {
    session_id: String,
    #[serde(flatten)]
    request: ResumeRequest,
}
impl ResumeParams {
    fn parse(raw: Value) -> Result<Self, crate::rpc::RpcError> {
        let parsed: Self = parse_raw(raw)?;
        parsed.request.validate()?;
        Ok(parsed)
    }
}
/// `approval/respond` — the approval-shaped entry to `sessions/resume`.
///
/// A transport projects a durable suspension as an
/// `approval/request` notification; the client answers with this
/// request. `approval_id` is the UI correlation id — not the durable
/// truth. Either one interrupt is named (`approval_id` + `decision`),
/// or the whole decision set is sent (`interrupts`). The handler maps
/// both onto the existing resume path, which owns coverage, replay
/// detection and the controller submit.
#[derive(Deserialize, validator::Validate)]
struct ApprovalRespondParams {
    session_id: String,
    /// The interrupt to answer (its `approval_id`, or its call id).
    #[serde(default)]
    approval_id: Option<String>,
    /// `allow` / `deny` / `reject` / `cancel`, or a durable status.
    #[serde(default)]
    decision: Option<String>,
    /// Decision detail (a denial reason, a grant scope, …).
    #[serde(default)]
    payload: Option<Value>,
    /// The complete decision set, when every open call is answered.
    #[serde(default)]
    interrupts: Option<Vec<InterruptDecision>>,
    /// A token the client already holds; derived from the log otherwise.
    #[serde(default)]
    resume_token: Option<uva::harness::ResumeToken>,
}
/// Register the session methods.
pub fn register(router: &mut RpcRouter) {
    router.register("sessions/list", |state, raw| async move {
        let query: SessionPageQuery = parse(raw)?;
        encode(sessions::list_sessions(state, query).await?)
    });
    router.register("sessions/get", |state, raw| async move {
        let params: IdParams = parse(raw)?;
        encode(sessions::get_session(state, params.id).await?)
    });
    router.register("sessions/delete", |state, raw| async move {
        let params: IdParams = parse(raw)?;
        encode(sessions::delete_session(state, params.id).await?)
    });
    router.register("sessions/export", |state, raw| async move {
        let params: IdParams = parse(raw)?;
        encode(sessions::export_session(state, params.id).await?)
    });
    router.register("sessions/fork", |state, raw| async move {
        let params = ForkParams::parse(raw)?;
        encode(sessions::fork_session(state, params.id, params.request).await?)
    });
    router.register("sessions/events", |state, raw| async move {
        let params: IdParams = parse(raw)?;
        encode(sessions::list_session_events(state, params.id).await?)
    });
    router.register("sessions/status", |state, raw| async move {
        let params: IdParams = parse(raw)?;
        encode(sessions::session_status(state, params.id).await?)
    });
    router.register("sessions/follow", |state, raw| async move {
        let params: FollowParams = parse(raw)?;
        encode(follow::follow(state, params.session_id, params.query).await?)
    });
    router.register("session/unfollow", |state, raw| async move {
        let params: SessionIdParams = parse(raw)?;
        state.rpc.follows().stop(&params.session_id);
        Ok(serde_json::json!({ "stopped": true }))
    });
    router.register("sessions/resume", |state, raw| async move {
        let params = ResumeParams::parse(raw)?;
        encode(follow::resume(state, params.session_id, params.request).await?)
    });
    router.register("approval/respond", |state, raw| async move {
        let params: ApprovalRespondParams = parse(raw)?;
        encode(approval_respond(state, params).await?)
    });
    router.register("sessions/compact", |state, raw| async move {
        let params: SessionIdParams = parse(raw)?;
        encode(follow::compact(state, params.session_id).await?)
    });
    router.register("sessions/usage", |state, raw| async move {
        let params: SessionIdParams = parse(raw)?;
        encode(follow::usage(state, params.session_id).await?)
    });
    router.register("sessions/search", |state, raw| async move {
        let params: SearchParams = parse(raw)?;
        let user_id = state.default_user_id().to_string();
        encode(
            session_search::search_sessions(state, user_id, params.query)
                .await?,
        )
    });
}
/// Map `approval/respond` onto the durable `sessions/resume` path.
///
/// The log's open `approval_asked` rows decide what can be answered;
/// the token is derived from the log when the client does not hold
/// one; then the existing resume path does coverage, replay detection
/// and the controller submit. No decision is kept in memory here, so
/// a restart cannot lose one.
async fn approval_respond(
    state: std::sync::Arc<AppState>,
    mut params: ApprovalRespondParams,
) -> Result<(), crate::rpc::RpcError> {
    let (open, derived) =
        follow::open_interrupts_with_token(&state, &params.session_id).await?;
    if open.is_empty() {
        return Err(unknown_approval(&params.session_id));
    }
    let decisions = approval_decisions(&mut params, &open)?;
    let Some(token) = params.resume_token.take().or(derived) else {
        return Err(crate::rpc::RpcError::application(
            "resume_no_snapshot",
            format!(
                "session '{}' has no request snapshot; this round cannot be \
                 resumed",
                params.session_id
            ),
        ));
    };
    follow::resume(
        state,
        params.session_id,
        ResumeRequest {
            resume_token: token,
            interrupts: decisions,
            allow_drift: false,
        },
    )
    .await
}

/// Resolve the request's shape into the durable decision list.
fn approval_decisions(
    params: &mut ApprovalRespondParams,
    open: &[uva::harness::ApprovalRequest],
) -> Result<Vec<InterruptDecision>, crate::rpc::RpcError> {
    match (params.interrupts.take(), params.approval_id.as_deref()) {
        (Some(_), Some(_)) => Err(crate::rpc::RpcError::application(
            "validation",
            "approval/respond takes either approval_id or interrupts, not both",
        )),
        (Some(decisions), None) => {
            for decision in &decisions {
                if !open.iter().any(|request| {
                    request.id == decision.id || request.call_id == decision.id
                }) {
                    return Err(unknown_approval(&decision.id));
                }
            }
            Ok(decisions)
        }
        (None, Some(approval_id)) => {
            let request = open
                .iter()
                .find(|request| {
                    request.id == approval_id || request.call_id == approval_id
                })
                .ok_or_else(|| unknown_approval(approval_id))?;
            if open.len() > 1 {
                let remaining: Vec<&str> = open
                    .iter()
                    .filter(|other| other.id != request.id)
                    .map(|other| other.id.as_str())
                    .collect();
                return Err(crate::rpc::RpcError::application(
                    "incomplete_resume",
                    format!(
                        "approval '{approval_id}' does not answer every open \
                         interrupt; still open: {}",
                        remaining.join(", ")
                    ),
                ));
            }
            Ok(vec![InterruptDecision {
                id: approval_id.to_string(),
                status: approval_status(params.decision.as_deref())?,
                payload: params.payload.take(),
            }])
        }
        (None, None) => Err(crate::rpc::RpcError::application(
            "validation",
            "approval/respond needs approval_id or interrupts",
        )),
    }
}

/// The wire decision for one held call.
///
/// `allow` authorizes the call; `deny`/`reject` writes the
/// model-visible error result instead; `cancel` abandons it. The
/// durable statuses (`resolved` / `rejected` / `cancelled`) are
/// accepted too, so a client that already speaks `sessions/resume`
/// vocabulary can reuse it.
fn approval_status(
    decision: Option<&str>,
) -> Result<uva::harness::ResumeStatus, crate::rpc::RpcError> {
    use uva::harness::ResumeStatus;
    match decision {
        Some("allow" | "resolved") => Ok(ResumeStatus::Resolved),
        Some("deny" | "reject" | "rejected") => Ok(ResumeStatus::Rejected),
        Some("cancel" | "cancelled") => Ok(ResumeStatus::Cancelled),
        Some(other) => Err(crate::rpc::RpcError::application(
            "validation",
            format!(
                "unknown approval decision '{other}'; use allow, deny, reject \
                 or cancel"
            ),
        )),
        None => Err(crate::rpc::RpcError::application(
            "validation",
            "approval/respond needs a decision",
        )),
    }
}

/// The refusal for an id that names no open interrupt.
fn unknown_approval(id: &str) -> crate::rpc::RpcError {
    crate::rpc::RpcError::application(
        "not_found",
        format!("approval '{id}' is not an open interrupt"),
    )
}

/// The one-parameter shape: `{"id": "…"}`.
#[derive(Deserialize, validator::Validate)]
struct IdParams {
    id: String,
}
/// The one-parameter shape: `{"session_id": "…"}`.
#[derive(Deserialize, validator::Validate)]
struct SessionIdParams {
    session_id: String,
}
/// `sessions/follow` — the session id beside the staleness check.
#[derive(Deserialize, validator::Validate)]
struct FollowParams {
    session_id: String,
    #[serde(flatten)]
    query: FollowQuery,
}
/// `sessions/search` — the query beside the filters.
#[derive(Deserialize, validator::Validate)]
struct SearchParams {
    #[serde(flatten)]
    query: SessionSearchParams,
}
