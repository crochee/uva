//! REST + SSE chat interface.
//!
//! This module replaces the REST + SSE interaction surface for the
//! web frontend with a conventional REST + Server-Sent Events API.
//! The on-disk JSONL session log and the agent runtime are unchanged;
//! the only difference between this and the legacy executor was the
//! wire format that wraps them.
//!
//! Wire contract:
//!
//! - `POST /api/v1/chat/sessions`
//!   Body: `{ "session_id"?: string, "agent_name"?: string }`
//!   Response: `200 { "session_id": string, "agent_name": string | null }`
//!   - Creates a new session. If `session_id` is omitted the server
//!     mints a UUID; if `agent_name` is omitted the server resolves
//!     a default via `AppState::resolve_agent_name`.
//!
//! - `POST /api/v1/chat/sessions/{id}/messages`
//!   Body: `{ "text": string, "attachments": [...], "agent_name"?: string,
//!            "model"?: string, "delivery"?: "queue" | "steer" }`
//!   Response: `200 { "message_id": string, "queued": bool,
//!                    "delivery": "queue" | "steer" }`
//!   - Queues a turn. There is no SSE reply on this surface: the
//!     transcript — durable rows and the in-progress turn's live
//!     projection — is read from `GET /api/v1/sessions/{id}/follow`
//!     (`routes::follow`), which one connection serves for the
//!     session's whole life.
//!   - The `200` is answered **after the turn is on disk**: the
//!     controller parks the turn as a durable `parked_prompt` row and
//!     flushes it before the route answers, so a crash between the
//!     answer and the run cannot lose the message. (When another node
//!     owns the session, the `200` means the turn was forwarded to
//!     that owner, which parks it there.)
//!   - `delivery: "steer"` (text only) injects the message into the
//!     run that is live **now**: the handler appends a durable
//!     `user_message` row with `source: "steer"` and flushes it
//!     before answering, and the loop's next step boundary carries
//!     it into the model's next request. When no run is live the
//!     message becomes an ordinary queued turn instead, and the
//!     response reports `"delivery": "queue"`.
//!
//! - `POST /api/v1/chat/sessions/{id}/cancel`
//!   Body: empty. Response: `204 No Content`.
//!
//! - `POST /api/v1/chat/sessions/{id}/regenerate`
//!   Body: empty. Response: `202 Accepted`.
//!   - Re-queues the most recent user turn, recovered from the
//!     session log. Only the turn's **text** is recoverable: image
//!     and audio bytes are deliberately not serialised into the sink
//!     (see `elysper-server/src/session/controller.rs`), so a
//!     regenerated multimodal turn replays its prompt text without
//!     the attachment.
//!
//! - `POST /api/v1/chat/messages/{message_id}/feedback`
//!   Body: `{ "thumbs_up": boolean, "session_id": string }`
//!   Response: `204 No Content`.
//!   - Persists a feedback record into the *named session's* log so
//!     future analytics endpoints can aggregate it. `session_id` is
//!     required: the message id is a client-side label and cannot
//!     identify a session on its own.
//!
//! - `GET /api/v1/chat/usage`
//!   Response: `200 { "tokens_in": ..., "tokens_out": ..., ... }`.
//!
//! Session listing and detail live in the management surface
//! (`routes::sessions` at `/api/v1/sessions` and
//! `/api/v1/sessions/{id}`) so there is a single canonical
//! SessionSummary / SessionDetail shape on the wire.
//!
//! All handlers fail with [`crate::rpc::RpcError`], which the
//! dispatcher serialises as a JSON-RPC error object carrying the
//! machine code in `error.data.code`.

use std::sync::Arc;

use base64::Engine as _;
use elysper::{
    core::Error,
    provider::{ContentPart, Message, Role},
    session::manager::SessionRegistry,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    api::{OperationRequest, validate_resource_name},
    rpc::RpcError,
    session::controller::{PinnedProvider, SessionOp, SteerDelivery},
    state::{AppState, ModelSelectionError},
};

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, validator::Validate)]
pub struct CreateSessionRequest {
    #[serde(default)]
    #[validate(length(min = 1, message = "must not be empty"))]
    pub session_id: Option<String>,
    #[serde(default)]
    #[validate(length(min = 1, message = "must not be empty"))]
    pub agent_name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CreateSessionResponse {
    pub session_id: String,
    pub agent_name: Option<String>,
}

/// How a submitted message should reach its run.
///
/// `Queue` is the ordinary path: the turn waits for the session and
/// the run that starts it logs the user row. `Steer` asks for the
/// message to be injected into the run that is live *now*, through
/// the session log — and reports `queue` back when no run was live
/// to receive it.
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// An ordinary turn: queued, and run when the session is free.
    #[default]
    Queue,
    /// Injected into the live run; it reaches the model at that run's
    /// next step boundary and never rewrites the request in flight.
    Steer,
}

#[derive(Debug, Default, Deserialize, validator::Validate)]
pub struct SendMessageRequest {
    /// The prompt text. Empty is allowed when attachments carry the
    /// turn; `send_message` rejects a request with neither.
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub attachments: Vec<WireAttachment>,
    #[serde(default)]
    #[validate(length(min = 1, message = "must not be empty"))]
    pub agent_name: Option<String>,
    /// Optional explicit model selection (wire format:
    /// `"<provider>/<model>"`, e.g. `"anthropic/claude-opus"`).
    /// When `None`, the agent's configured default is used.
    #[serde(default)]
    #[validate(length(min = 1, message = "must not be empty"))]
    pub model: Option<String>,
    /// `"queue"` (default) or `"steer"`.
    ///
    /// A steer is text-only: the log is its only carrier, so a
    /// request that also carries attachment bytes is rejected rather
    /// than silently dropped.
    #[serde(default)]
    pub delivery: Delivery,
}

#[derive(Debug, Default, Deserialize, Serialize, validator::Validate)]
pub struct WireAttachment {
    /// `"image" | "audio" | "file" | "url"`
    #[validate(length(min = 1, message = "must not be empty"))]
    pub kind: String,
    /// Base64 payload for binary attachments.
    #[serde(default)]
    pub data_base64: Option<String>,
    /// Remote URL for `url` kind attachments.
    #[serde(default)]
    pub url: Option<String>,
    /// MIME type (`image/png`, `audio/wav`, ...).
    #[serde(default)]
    pub mime_type: Option<String>,
    pub filename: Option<String>,
    /// Attachment hash for `kind = "attachment_ref"` — the
    /// client references a previously-uploaded attachment by
    /// hash; the chat pipeline resolves it through
    /// `AppState::attachment_store` at the agent-runtime
    /// boundary (replay-safe via content-addressing).
    #[serde(default)]
    pub attachment_hash: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SendMessageResponse {
    pub message_id: String,
    /// `true` when the turn waits for the session's next run.
    ///
    /// A queued turn is **on disk** when this answer goes out: the
    /// controller parks it as a durable `parked_prompt` row and
    /// flushes it before the route answers. The answer therefore
    /// promises what the log holds — a crash between the answer and
    /// the run cannot lose the message.
    pub queued: bool,
    /// The delivery that actually happened: `"queue"` when no run was
    /// live to steer, `"steer"` when the message was injected.
    pub delivery: Delivery,
}

#[derive(Debug, Deserialize, Default, validator::Validate)]
pub struct FeedbackRequest {
    pub thumbs_up: bool,
    /// The session the rated message belongs to.
    ///
    /// `message_id` alone cannot resolve it — the id the chat UI
    /// sends is a client-side label, and the sink has no
    /// message-id index. Without this the handler resolved a
    /// hardcoded `"default"` session, so every 👍/👎 landed in the
    /// wrong log (and created that session on first click).
    #[serde(default)]
    #[validate(length(min = 1, message = "must not be empty"))]
    pub session_id: String,
}
#[derive(Debug, Serialize)]
pub struct UsageResponse {
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub turns: u64,
    pub sessions_total: usize,
}

// ---------------------------------------------------------------------------
// POST /api/v1/chat/sessions
// ---------------------------------------------------------------------------

pub async fn create_session(
    state: Arc<AppState>,
    req: CreateSessionRequest,
) -> Result<CreateSessionResponse, RpcError> {
    let user_id = resolve_user_id(&state);
    let session_id =
        req.session_id.unwrap_or_else(|| Uuid::new_v4().to_string());
    validate_resource_name(&session_id)?;
    let agent_name = state.resolve_agent_name_for(req.agent_name.as_deref());
    state
        .get_or_create_session_controller(&user_id, &session_id)
        .await?;
    Ok(CreateSessionResponse {
        session_id,
        agent_name,
    })
}

// ---------------------------------------------------------------------------
// POST /api/v1/chat/sessions/{id}/messages
// ---------------------------------------------------------------------------

pub async fn send_message(
    state: Arc<AppState>,
    session_id: String,
    req: SendMessageRequest,
) -> Result<SendMessageResponse, RpcError> {
    // Resolve the model selector BEFORE anything is queued: a
    // selection the server cannot honour is the caller's to see, and
    // discovering it only after the turn was accepted would leave the
    // user watching a stream that already ran the wrong model.
    //
    // Only an *explicit* selection becomes a provider pin. A pin
    // cannot cross nodes (it holds a live provider handle), so
    // pinning the default would make every submission to a
    // remote-owned session fail; an unpinned op runs the owner's own
    // configured default, which is the same provider when the
    // deployment config is shared (it is — the config file is).
    let pinned = match req.model.as_deref() {
        Some(selection) => {
            let provider = state
                .resolve_provider(Some(selection))
                .map_err(selection_error)?;
            let id = state.provider_id(Some(selection)).unwrap_or_default();
            Some(PinnedProvider::new(provider).with_id(id))
        }
        None => None,
    };
    let priority = 1u8;
    if req.text.trim().is_empty() && req.attachments.is_empty() {
        let message =
            "a message needs text or at least one attachment".to_string();
        return Err(RpcError::from(Error::validation(message.clone()))
            .with_code("empty_message")
            .with_message(message));
    }
    if req.delivery == Delivery::Steer {
        // The log is a steer's only carrier, so a payload it cannot
        // hold is the caller's mistake to see — not a steer that
        // silently loses its image. The check reads the request
        // itself and comes BEFORE the parts are built, so the
        // attachment bytes of a request that cannot carry them are
        // never decoded.
        if !req.attachments.is_empty() {
            let message = "a steer carries text only; attachments cannot \
                           be injected into a live run"
                .to_string();
            return Err(RpcError::from(Error::validation(message.clone()))
                .with_code("invalid_steer")
                .with_message(message));
        }
        return steer_session_op(&state, &session_id, req.text, pinned).await;
    }
    // Multimodal input is a capability of the mounted `attachments`
    // service. When the plugin is unloaded the store is withdrawn and
    // an image / audio / stored-ref part is refused, never silently
    // dropped: the caller asked for a modality the deployment no
    // longer serves.
    let attachments = state.attachments();
    admit_attachments(&req, attachments.is_some()).map_err(|message| {
        RpcError::from(Error::validation(message.to_string()))
            .with_code("attachments_unavailable")
            .with_message(message)
    })?;
    let parts = build_parts(&req, attachments.as_deref());
    // `resolve_agent_name_for(None)` answers the deployment default, so
    // the "was an agent named?" question must come from the request
    // itself. Treating the resolved default as explicit routed every
    // plain-text turn through `PromptMulti` — and when its park was a
    // single slot, two queued turns overwrote each other and the first
    // was never logged. `PromptMulti` is therefore reserved for a
    // request that actually carries attachments or an explicit agent
    // override; everything else parks a plain-text turn.
    let explicit_agent = req.agent_name.is_some();
    let explicit_agent_name =
        state.resolve_agent_name_for(req.agent_name.as_deref());
    // A named agent rides the multimodal op because that is the only
    // prompt variant that carries one; a plain `Prompt` would silently
    // run the session's configured default instead of the selection
    // the composer shows.
    let op = if parts.len() > 1
        || parts.iter().any(|p| !matches!(p, ContentPart::Text(_)))
        || explicit_agent
    {
        SessionOp::PromptMulti {
            parts,
            agent_name: explicit_agent_name.clone(),
            priority,
        }
    } else {
        // Synthesise a plain text prompt from the only part.
        let text = match parts.into_iter().next() {
            Some(ContentPart::Text(t)) => t.text,
            _ => String::new(),
        };
        SessionOp::Prompt {
            content: text,
            priority,
        }
    };
    submit_session_op(&state, &session_id, op, pinned).await
}

/// Deliver a text message to the session as a steer, reporting the
/// delivery that actually happened.
///
/// The controller owns the check-and-append: a live run receives a
/// durable `user_message` row before this returns, and a settled one
/// receives an ordinary queued turn with no row of its own (the run
/// logs it on start). Either way the caller learns which, because an
/// answer that promised an injection it did not make would be a lie.
///
/// The route answers `200` for both deliveries: the durability
/// promise is the flush that already happened, not the status code.
async fn steer_session_op(
    state: &Arc<AppState>,
    session_id: &str,
    content: String,
    provider: Option<PinnedProvider>,
) -> Result<SendMessageResponse, RpcError> {
    validate_resource_name(session_id)?;
    let user_id = resolve_user_id(state);
    let controller = state
        .get_or_create_session_controller(&user_id, session_id)
        .await?;
    let delivery = controller
        .steer(content, provider)
        .await
        .map_err(|e| Error::internal(format!("{e}")))?;
    Ok(SendMessageResponse {
        message_id: Uuid::new_v4().to_string(),
        queued: delivery == SteerDelivery::Queue,
        delivery: match delivery {
            SteerDelivery::Steer => Delivery::Steer,
            SteerDelivery::Queue => Delivery::Queue,
        },
    })
}

/// Map a failed `model` resolution onto the unified error envelope.
///
/// The status comes from the variant — a value no configured
/// provider resolves is the caller's `400`, a provider the
/// deployment configures but could not build is the server's `503`
/// (the Anthropic wire maps the same variants the same way). The
/// message is the error's own `Display`, which names the value the
/// caller sent.
fn selection_error(error: ModelSelectionError) -> RpcError {
    let message = error.to_string();
    RpcError::from(Error::validation(message.clone()))
        .with_code("invalid_model")
        .with_message(message)
}

/// Resolve the session's controller and submit `op`, returning
/// the standard queued-turn envelope.
///
/// Shared by [`send_message`] and [`operation`] so both routes
/// go through one validation + controller-resolution path.
/// `provider` is the submitting route's `model` selection, already
/// resolved; `None` runs the session's configured default.
///
/// The submit waits for the turn's `parked_prompt` row to reach the
/// log, so the `200` this answers with means the turn is **on disk**:
/// a crash between the answer and the run cannot lose it. The wait
/// is bounded (see [`SessionController::submit_durable`]) and a
/// failure to make the row durable is an error answer, never a
/// promise the log cannot keep.
async fn submit_session_op(
    state: &Arc<AppState>,
    session_id: &str,
    op: SessionOp,
    provider: Option<PinnedProvider>,
) -> Result<SendMessageResponse, RpcError> {
    validate_resource_name(session_id)?;
    let user_id = resolve_user_id(state);
    let controller = state
        .get_or_create_session_controller(&user_id, session_id)
        .await?;
    controller
        .submit_durable_with_provider(op, provider)
        .await
        .map_err(|e| Error::internal(format!("{e}")))?;
    Ok(SendMessageResponse {
        message_id: Uuid::new_v4().to_string(),
        queued: true,
        delivery: Delivery::Queue,
    })
}

// ---------------------------------------------------------------------------
// POST /api/v1/chat/sessions/{id}/operation  (R29, gated)
// ---------------------------------------------------------------------------

/// `POST /api/v1/chat/sessions/{id}/operation` — the
/// discriminated-union operation endpoint.
///
/// Registered only when `[operations] enabled = true` in
/// `config.toml` (see [`crate::config::OperationEndpointConfig`]).
///
/// [`OperationRequest::Prompt`] is dispatched to the same code
/// path [`send_message`] uses. The other variants are declared for
/// wire compatibility and answer `501 Not Implemented` with the
/// standard error envelope until the backing features land.
pub async fn operation(
    state: Arc<AppState>,
    session_id: String,
    req: OperationRequest,
) -> Result<SendMessageResponse, RpcError> {
    match req {
        OperationRequest::Prompt { text } => {
            let op = SessionOp::Prompt {
                content: text,
                priority: 1,
            };
            // The operation wire carries no model selection, so the
            // turn runs the session's configured default.
            submit_session_op(&state, &session_id, op, None).await
        }
        other => {
            let kind = other.kind();
            Err(RpcError::from(Error::internal(format!(
                "operation kind `{kind}` not implemented"
            )))
            .with_code("not_implemented")
            .with_message(format!(
                "operation kind `{kind}` is not implemented"
            )))
        }
    }
}

// ---------------------------------------------------------------------------
// POST /api/v1/chat/sessions/{id}/cancel
// ---------------------------------------------------------------------------

pub async fn cancel_session(
    state: Arc<AppState>,
    session_id: String,
) -> Result<(), RpcError> {
    validate_resource_name(&session_id)?;
    let user_id = resolve_user_id(&state);
    let controller = state
        .get_or_create_session_controller(&user_id, &session_id)
        .await?;
    controller
        .submit(SessionOp::Cancel { reason: None })
        .await
        .map_err(|e| Error::internal(format!("{e}")))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// POST /api/v1/chat/sessions/{id}/regenerate
// ---------------------------------------------------------------------------

pub async fn regenerate(
    state: Arc<AppState>,
    session_id: String,
) -> Result<(), RpcError> {
    validate_resource_name(&session_id)?;
    let user_id = resolve_user_id(&state);
    // Recover the turn BEFORE resolving the controller.
    // `get_or_create_session_controller` eagerly creates the session
    // when it is unknown, so resolving first would make a miss —
    // the one case that must not have side effects — the very path
    // that creates a session.
    //
    // A miss is also NOT a cancel. `Cancel` is the same op the
    // `/cancel` route submits, so answering "nothing to replay"
    // with it would abort the session — a destructive side effect
    // from a read that simply found no user turn (a log whose
    // prompt was shadowed by a compaction `Replace`, a session
    // driven only by programmatic input, or a session created but
    // never written, whose log file does not exist yet). Report the
    // miss instead.
    let Some(parts) =
        read_last_user_turn(&state.session_manager, &user_id, &session_id)
            .await
    else {
        return Err(Error::not_found(
            "no user turn to regenerate in this session",
        )
        .into());
    };
    let controller = state
        .get_or_create_session_controller(&user_id, &session_id)
        .await?;
    let priority = 1u8;
    let agent_name = state.resolve_agent_name_for(None);
    // The `202` promises a queued turn: the rerun's `parked_prompt`
    // row is on disk before the answer goes out.
    controller
        .submit_durable(SessionOp::Rerun {
            parts,
            agent_name,
            priority,
        })
        .await
        .map_err(|e| Error::internal(format!("{e}")))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// POST /api/v1/chat/messages/{message_id}/feedback
// ---------------------------------------------------------------------------

pub async fn feedback(
    state: Arc<AppState>,
    message_id: String,
    req: FeedbackRequest,
) -> Result<(), RpcError> {
    validate_resource_name(&message_id)?;
    // The session arrives in the body, so it is caller-supplied and
    // reaches the sink's filesystem path — validate it exactly like a
    // path parameter.
    validate_resource_name(&req.session_id)?;
    let session_id = req.session_id;
    let user_id = resolve_user_id(&state);
    let controller = state
        .get_or_create_session_controller(&user_id, &session_id)
        .await?;
    // The `204` promises the record: the feedback row is on disk
    // before the answer goes out.
    controller
        .submit_durable(SessionOp::Feedback {
            message_id,
            thumbs_up: req.thumbs_up,
        })
        .await
        .map_err(|e| Error::internal(format!("{e}")))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// GET /api/v1/chat/usage
// ---------------------------------------------------------------------------

pub async fn usage(state: Arc<AppState>) -> UsageResponse {
    let usage = state.usage_metrics().snapshot();
    UsageResponse {
        tokens_in: usage.tokens_in,
        tokens_out: usage.tokens_out,
        turns: usage.turns,
        sessions_total: state.active_sessions.len(),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) fn resolve_user_id(state: &AppState) -> String {
    // The legacy single-tenant deployments use the constant
    // `SERVER_DEFAULT_USER_ID`. A real auth middleware would
    // thread the resolved user_id through the request headers.
    state.default_user_id().to_string()
}

/// Whether `req` carries an image, audio, or stored-ref part.
///
/// These are the parts whose bytes or hash the `attachments` service
/// serves; text and `file` / `url` parts do not need it.
fn has_multimodal_input(req: &SendMessageRequest) -> bool {
    req.attachments.iter().any(|attachment| {
        matches!(
            attachment.kind.as_str(),
            "image" | "audio" | "attachment_ref"
        )
    })
}

/// Refuse multimodal input when the `attachments` service is absent.
///
/// A `None` store does not degrade the turn silently — an image the
/// caller supplied disappearing from the prompt is a wrong answer, not
/// a smaller one.
fn admit_attachments(
    req: &SendMessageRequest,
    available: bool,
) -> Result<(), &'static str> {
    if available || !has_multimodal_input(req) {
        return Ok(());
    }
    Err(
        "multimodal input is unavailable: the `attachments` service is \
         not mounted",
    )
}

/// Convert the wire `SendMessageRequest` body into a list of
/// `ContentPart`s the agent runtime understands. Text parts are
/// always emitted first; binary attachments follow in input order.
///
/// `attachment_store` enables the `kind = "attachment_ref"`
/// resolution path: the hash is looked up through the R11 store
/// and the bytes are inlined as an `Image` part (replay-safe via
/// content addressing). Pass `None` in tests / when the store is
/// unavailable; unresolved refs are skipped with a warn log.
fn build_parts(
    req: &SendMessageRequest,
    attachment_store: Option<&elysper::attachment::AttachmentStore>,
) -> Vec<ContentPart> {
    let mut parts: Vec<ContentPart> = Vec::new();
    if !req.text.is_empty() {
        parts.push(text_part(&req.text));
    }
    for a in &req.attachments {
        let part = match a.kind.as_str() {
            "image" => image_part(a),
            "attachment_ref" => attachment_ref_part(a, attachment_store),
            "audio" => audio_part(a),
            "file" => file_part(a),
            "url" => url_part(a),
            _ => None,
        };
        if let Some(part) = part {
            parts.push(part);
        }
    }
    parts
}

/// Wrap plain prompt text in the wire-neutral `Text` variant.
fn text_part(s: &str) -> ContentPart {
    ContentPart::Text(elysper::provider::TextContent {
        text: s.to_string(),
        cache_control: None,
    })
}

/// `kind = "image"` — inline base64 or remote URL, both end up
/// as an `Image` part with `detail = Auto`.
fn image_part(a: &WireAttachment) -> Option<ContentPart> {
    let data = a.data_base64.as_ref().or(a.url.as_ref())?;
    Some(ContentPart::Image(elysper::provider::ImageContent {
        mime_type: a.mime_type.clone().unwrap_or_default(),
        data: data.clone(),
        detail: Some(elysper::provider::ImageDetail::Auto),
    }))
}

/// `kind = "attachment_ref"` — resolve the hash through the R11
/// store and inline the bytes as a base64 `Image` part. Returns
/// `None` (after logging) when the hash is missing, the store
/// is unavailable, or the lookup fails — the turn ships without
/// the attachment rather than failing the request.
fn attachment_ref_part(
    a: &WireAttachment,
    attachment_store: Option<&elysper::attachment::AttachmentStore>,
) -> Option<ContentPart> {
    let (Some(hash), Some(store)) = (&a.attachment_hash, attachment_store)
    else {
        tracing::warn!(
            hash = ?a.attachment_hash,
            "attachment_ref missing hash or store; skipping"
        );
        return None;
    };
    match store.read_base64_raw(hash) {
        Ok(bytes) => {
            Some(ContentPart::Image(elysper::provider::ImageContent {
                mime_type: a
                    .mime_type
                    .clone()
                    .unwrap_or_else(|| "image/png".to_string()),
                data: base64::engine::general_purpose::STANDARD.encode(&bytes),
                detail: Some(elysper::provider::ImageDetail::Auto),
            }))
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                hash,
                "attachment_ref resolve failed; skipping"
            );
            None
        }
    }
}

/// `kind = "audio"` — inline base64 audio bytes. The container is left
/// unset so it is derived from the media type at the wire boundary
/// (see `AudioContent::format_label`): the composer accepts any
/// `audio/*`, so hardcoding `Wav` here would declare non-WAV bytes as a
/// WAV container and the provider rejects the mismatch.
fn audio_part(a: &WireAttachment) -> Option<ContentPart> {
    let b64 = a.data_base64.as_ref()?;
    Some(ContentPart::Audio(elysper::provider::types::AudioContent {
        mime_type: a.mime_type.clone().unwrap_or_default(),
        data: b64.clone(),
        format: None,
    }))
}

/// `kind = "file"` — generic binary file. The provider does not
/// expose a dedicated `RawContent`; binary files arrive as a
/// generic `ResourceLink` carrying the mime type and a base64
/// payload (wrapped in a `data:` URI). The agent runtime reads
/// the `mime_type` to decide how to consume the `data` field.
fn file_part(a: &WireAttachment) -> Option<ContentPart> {
    let b64 = a.data_base64.as_ref()?;
    Some(ContentPart::Resource(elysper::provider::ResourceLink {
        mime_type: a.mime_type.clone(),
        uri: format!(
            "data:{};base64,{}",
            a.mime_type.clone().unwrap_or_default(),
            b64
        ),
        name: a.filename.clone().unwrap_or_default(),
        title: None,
        description: None,
    }))
}

/// `kind = "url"` — reference an external resource by URL.
fn url_part(a: &WireAttachment) -> Option<ContentPart> {
    let url = a.url.as_ref()?;
    Some(ContentPart::Resource(elysper::provider::ResourceLink {
        mime_type: a.mime_type.clone(),
        uri: url.clone(),
        name: a.filename.clone().unwrap_or_default(),
        title: None,
        description: None,
    }))
}

/// Read the most recent user turn's parts from the on-disk session
/// log so the `regenerate` endpoint can replay the same payload.
///
/// Returns `None` for every "nothing to replay" condition, including
/// an unreadable log — a session whose `events.jsonl` does not exist
/// yet (created but never written) is a miss, not a server fault, and
/// letting the read error through would surface it as a `500`.
async fn read_last_user_turn(
    registry: &SessionRegistry,
    user_id: &str,
    session_id: &str,
) -> Option<Vec<ContentPart>> {
    let sink = registry.sink(user_id, session_id);
    match sink.read().await {
        Ok(events) => extract_last_user_parts(&events),
        Err(e) => {
            tracing::debug!(
                target: "elysper.session",
                session_id = %session_id,
                error = %e,
                "regenerate: session log unreadable; treating as no user turn"
            );
            None
        }
    }
}

fn extract_last_user_parts(events: &[Value]) -> Option<Vec<ContentPart>> {
    // Project through the shared raw-log fold rather than matching a
    // row shape by hand. That fold is the single decoder for every
    // envelope the sink has ever carried — the controller's
    // `UserInput` rows, the typed `user_message` family, the
    // lossless `Message` envelope, and the legacy `Model` parts —
    // and it honours a durable compaction checkpoint. A hand-rolled
    // matcher rots the moment the writer changes, which is exactly
    // what happened here: the previous version looked for
    // `{"role": …, "parts": […]}`, a shape no writer emits, so
    // `regenerate` found nothing, never submitted `Rerun`, and
    // fell through to cancelling the run.
    //
    // Only the textual component is recoverable (image and audio
    // bytes are deliberately not serialised into the sink), so a
    // regenerated multimodal turn replays its prompt text.
    for row in elysper::session::fold_log_surface(events)
        .messages
        .iter()
        .rev()
    {
        let Ok(message) = serde_json::from_value::<Message>(row.clone()) else {
            continue;
        };
        if message.role != Role::User {
            continue;
        }
        let parts: Vec<ContentPart> = message.content.iter().cloned().collect();
        if !parts.is_empty() {
            return Some(parts);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
