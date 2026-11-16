//! The Anthropic Messages-compatible surface: the `anthropic/messages`
//! method.
//!
//! An Anthropic-protocol client — the Anthropic SDKs, Claude Code,
//! editor plugins — can point its `base_url` at this server and talk to
//! it. The method is additive: every `chat/*` method keeps its
//! existing wire shape, and this module shares their run machinery
//! rather than growing a second one.
//!
//! # Contract
//!
//! - `anthropic/messages`
//!   Params: the Messages request (`model`, `max_tokens`, `messages`,
//!   optional `system` / `tools` / `stream`). Unknown fields are
//!   ignored, exactly as an Anthropic-compatible endpoint should.
//!   Result: the complete response body
//!   (`{id, type:"message", role:"assistant", model, content:[blocks],
//!   stop_reason, stop_sequence, usage}`), or — with `"stream": true`
//!   — `{accepted:true}`, after which the run's events arrive as
//!   [`MESSAGES_EVENT`] notifications.
//! - Every failure carries the **protocol's** error type, not the
//!   application's: `invalid_request_error` for a rejected request,
//!   `api_error` for a run that failed after the request was accepted,
//!   `overloaded_error` for a model the deployment cannot serve right
//!   now. The type travels as the machine code of the JSON-RPC error
//!   (`data.code`), so a client that branches on the Anthropic
//!   vocabulary still finds it where it expects to.
//!
//! # The stream
//!
//! A streaming request is answered the moment the run starts; the
//! events follow as [`MESSAGES_EVENT`] notifications, one per event,
//! in the order the protocol sends them:
//!
//! ```text
//! message_start → content_block_start → content_block_delta* →
//! content_block_stop (per block) → message_delta → message_stop
//! ```
//!
//! Each notification's params are `{session_id, event, data}`: the
//! session the run belongs to, the protocol's event name, and the
//! object that event carries. A run that fails mid-stream ends with
//! the protocol's `error` event instead of `message_delta` +
//! `message_stop`, so a client never reads a failed run as a finished
//! answer.
//!
//! # One request, one session — or a pinned one
//!
//! The protocol is stateless — the client resends the whole
//! conversation — while the runtime is a session whose transcript is
//! durable. How the two are bridged depends on whether the caller names
//! a session, which is what `metadata.uva_session_id` is for.
//!
//! ## No session named: one request, one session
//!
//! 1. A fresh session is minted per request
//!    (`anthropic-<uuid>`), so the client's history is the *only*
//!    history the run sees and a client that edits its own history is
//!    honoured rather than shadowed by a stale transcript;
//! 2. the request's earlier messages are written into that session's
//!    sink as the typed row each role names — `user_message` /
//!    `assistant_message` / `tool_result`, carrying the fold's lossless
//!    `Message` envelope (`uva::session::fold_log_surface`) — which the
//!    run task projects back into `AgentInput::history` with their real
//!    roles, including tool calls and results;
//! 3. the final user turn is dispatched through the same
//!    [`SessionOp::PromptMulti`] the chat surface uses, and the response
//!    is folded from the same event broadcast that
//!    `routes::follow` projects for the chat surface;
//! 4. when the turn ends the session is closed and evicted, so a
//!    stateless client cannot grow the controller cache (nothing else
//!    ever evicts it).
//!
//! ## `metadata.uva_session_id`: a pinned session
//!
//! Uva's own web client is not a stateless Anthropic client: it
//! shows one conversation, persists its session id, and addresses that
//! id for its sessions list, its feedback, its cancel and its
//! regenerate. Minting a session per request would give every turn a new
//! one and break all of that. So a request may name the session it is
//! continuing, in `metadata` — a field the protocol defines as a
//! free-form object with exactly one reserved key (`user_id`), which
//! makes a namespaced `uva_session_id` an extension no real
//! Anthropic client can collide with, and one none of them ever sends.
//! Its sibling `uva_agent_name` names the agent that answers, so the
//! client's agent choice is a dispatch input rather than a label.
//!
//! A pinned request differs in exactly three ways, each forced by "this
//! session outlives the request":
//!
//! - the id is **validated** with the same [`validate_resource_name`]
//!   gate every path parameter passes — it reaches the sink's
//!   filesystem path (`<root>/<user_id>/<session_id>/events.jsonl`), and
//!   an unvalidated id there is a path traversal. A malformed id is
//!   refused as an `invalid_request_error`; it is never silently
//!   replaced by a minted one, because a caller that names a session
//!   and gets a different one cannot tell.
//! - an id the server has not seen is **adopted**: it becomes a session
//!   under the request's own resolved user, exactly as
//!   `chat/session/create` auto-creates one. Requiring a pre-created
//!   session would break the first turn of every new conversation.
//! - history is **not re-seeded** once the session holds rows: the
//!   transcript *is* the conversation, and replaying the client's view
//!   of it on every turn would duplicate it — see [`seed_history`].
//! - the session is **not closed or evicted** when the turn ends. It is
//!   left to the same `DEFAULT_IDLE_TIMEOUT` reclaim every
//!   `chat/*` session gets, so the next turn finds a live controller,
//!   and so do feedback, cancel, regenerate and the session-detail
//!   page.
//!
//! The same gate turns on tool blocks: a pinned request is one of ours,
//! and one of ours renders `工具 · <name>` blocks with 请求 / 结果
//! halves. An external client must never be handed a call to answer, so
//! it keeps seeing none — see [`blocks`].
//!
//! # Deliberate limitations
//!
//! - **`tools` are converted, not injected.** The run's tool surface is
//!   the server's own (registry + `[tools]` config); the
//!   session-controller seam has no per-dispatch tool injection point,
//!   so a tool a client defines is parsed and validated but the model
//!   can only call the tools this server has. It is also why the
//!   response never contains an executed `tool_use` block: the agent
//!   runs its own tools, so a client is never asked to answer a call.
//! - **Sampling parameters are not plumbed.** `max_tokens` is validated
//!   and otherwise ignored; `temperature` / `top_p` / `stop_sequences`
//!   are not even parsed. The harness deliberately leaves sampling
//!   parameters to the deployment's model configuration, and it has no
//!   per-dispatch override.
//! - **`model` is echoed, not switched.** The run resolves its
//!   provider/model the same way `chat/message/send` does.
//! - **Approvals are not answerable here.** A run that suspends on a
//!   high-risk call terminates the request with the protocol's error
//!   type — an `error` event in the stream, and an
//!   `invalid_request_error` for a non-streaming request. A later
//!   request against that session is refused up front instead of
//!   queueing behind a run that cannot start. The session must be
//!   resumed through the session API (`sessions/resume`), which is the
//!   only wire that carries a decision.
//! - **Audio has no block** in this protocol, so an `audio/*`
//!   attachment cannot ride this method at all. `chat/*` is the wire
//!   that carries one (`kind: "audio"`); here the client is the only
//!   party that can keep one out of a request.
//! - **`count_tokens` is not implemented** (no such method is
//!   registered; it falls through the router's "unknown method").
//! - **Assistant prefills are rejected**: the run always needs a user
//!   turn, so a final `assistant` message is an
//!   `invalid_request_error`.
//!
//! # File attachments
//!
//! A `document` block is the protocol's file block — a PDF, or a plain
//! text or Markdown file. The canonical model has no document part, so
//! the block projects onto the same generic `ContentPart::Resource` the
//! old `chat/*` wire's `kind: "file"` attachment produced: an
//! inline source becomes a `data:<media_type>;base64,<payload>` URI and
//! a `url` source keeps its URL. A transcript replayed through either
//! endpoint is therefore identical — see [`convert::document_part`].
use std::sync::Arc;

use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::broadcast;
use uuid::Uuid;
use uva::{
    core::Error,
    harness::{AgentEvent, SessionEndReason, SystemEvent},
    provider::{Message, traits::ModelProvider},
};

use self::{
    blocks::Turn,
    convert::CanonicalRequest,
    wire::{AnthropicRequest, Metadata, error_type},
};
use crate::{
    api::validate_resource_name,
    rpc::{Outbound, RpcError},
    session::{
        EPHEMERAL_SESSION_PREFIX,
        controller::{
            PinnedProvider,
            SessionController,
            SessionOp,
            SessionState,
        },
    },
    state::{AppState, ModelSelectionError},
};
mod blocks;
mod convert;
pub(crate) mod wire;
/// The notification the streamed Anthropic events travel on.
///
/// The params are `{session_id, event, data}`: the session the run
/// belongs to, the protocol's event name, and the object that event
/// carries. The whole params object is built by this module — the
/// follow registry's `session/event` helper injects a `session_id` of
/// its own, and this notification is not that one.
pub const MESSAGES_EVENT: &str = "anthropic/messages/event";
/// Label on the leading turn that carries the client's `system` text.
///
/// The text cannot become a `Role::System` message (the harness
/// assembles the agent's own system prompt ahead of `history`, and the
/// Anthropic adapter promotes only the first one — see
/// [`convert`]), so it travels as the first user turn, labelled so the
/// model can tell it apart from the user's question.
const SYSTEM_LABEL: &str = "System instructions from the client:";
/// The machine code of a failure that is the server's own fault.
///
/// The Anthropic error type it maps onto is the protocol's `api_error`.
const INTERNAL_ERROR_CODE: &str = "internal_server_error";
// ---------------------------------------------------------------------------
// anthropic/messages
// ---------------------------------------------------------------------------
/// What the Anthropic-compatible surface answered.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum MessagesOutcome {
    /// The complete Anthropic response body.
    Body(Value),
    /// The stream was accepted; its events follow as notifications.
    Streaming { accepted: bool },
}
/// `anthropic/messages` — the Anthropic Messages endpoint, over RPC.
///
/// A non-streaming request answers with the protocol's complete
/// response body; a streaming one answers `{accepted:true}` and
/// publishes the run's events as [`MESSAGES_EVENT`] notifications.
///
/// # Errors
///
/// A rejected request, a model the deployment cannot serve, and a run
/// that failed are all failures of the *Anthropic* protocol, so each
/// one answers with the protocol's own error type as the machine code
/// (`invalid_request_error`, `api_error`, `overloaded_error`) — the
/// same vocabulary the HTTP boundary put in the envelope's
/// `error.type`.
pub async fn messages(
    state: Arc<AppState>,
    request: AnthropicRequest,
) -> Result<MessagesOutcome, RpcError> {
    let canonical = convert::to_canonical(&request).map_err(|error| {
        anthropic_failure(error_type::INVALID_REQUEST, error.to_string())
    })?;
    // The deployment is single-user: the session namespace is the
    // server's own default user, exactly as it is for every other
    // surface.
    let user_id = state.default_user_id().to_string();
    // Which session this turn belongs to and which agent answers it —
    // resolved (and, for a named id, validated) before anything touches
    // the sink.
    let target =
        resolve_target(request.metadata.as_ref()).map_err(|message| {
            anthropic_failure(error_type::INVALID_REQUEST, message)
        })?;
    // The turn's provider: the client names a model on every request
    // (the protocol requires it), so the selection is resolved before
    // anything else happens and a selection this deployment cannot serve
    // is refused up front — never silently answered by a different
    // model. The same `Arc` runs the turn (it is pinned to the dispatch
    // below) *and* names it in the response.
    let provider = state
        .resolve_provider(Some(&request.model))
        .map_err(model_selection_failure)?;
    let streaming = request.stream.unwrap_or(false);
    let turn = begin_turn(
        &state,
        &user_id,
        &request.model,
        canonical,
        target,
        provider,
    )
    .await
    .map_err(begin_turn_failure)?;
    if streaming {
        // The run is under way; its events are published as they
        // happen, so the answer is the acceptance and nothing else.
        let outbound = state.rpc.outbound().clone();
        let session_id = turn.session_id.clone();
        tokio::spawn(async move {
            publish_turn(outbound, session_id, turn).await;
        });
        return Ok(MessagesOutcome::Streaming { accepted: true });
    }
    Ok(MessagesOutcome::Body(complete_response(turn).await?))
}
/// Why a turn could not be started.
enum BeginTurnError {
    /// The session has an undecided approval interrupt. This protocol
    /// has no frame that answers one, so a request that would only
    /// queue a turn behind a run that can never start is refused
    /// instead of hanging until the client disconnects.
    AwaitingApproval,
    /// Anything else setup rejected (a sink write, a closed
    /// controller).
    Setup(anyhow::Error),
}
impl From<anyhow::Error> for BeginTurnError {
    fn from(error: anyhow::Error) -> Self {
        Self::Setup(error)
    }
}
/// The terminal answer for a run that suspended.
///
/// A suspended run is not a completed one: the session holds an open
/// interrupt and a later request would wait on a run this endpoint
/// cannot start. Both answer shapes say so — an `error` event
/// mid-stream, and an `invalid_request_error` for the non-streaming
/// request.
const APPROVAL_REQUIRED: &str = "the agent suspended awaiting approval; \
     /v1/messages cannot answer an approval prompt. Decide the open \
     interrupt through the session API (POST /api/v1/sessions/{id}/resume), \
     then continue the conversation.";
/// Which Anthropic error type a failure carries.
///
/// The old code keyed this off the HTTP status the failure answered
/// with; over RPC there is no status, and the failure's **machine
/// code** is what says which fault it is:
///
/// - a failure the server owns (`internal_server_error`) is the
///   protocol's `api_error`;
/// - a model the deployment cannot serve right now
///   (`model_unavailable`) is the protocol's `overloaded_error`;
/// - everything else is the caller's mistake, and the protocol's
///   `invalid_request_error`.
fn anthropic_error_type(code: &str) -> &'static str {
    match code {
        INTERNAL_ERROR_CODE => error_type::API,
        "model_unavailable" => error_type::OVERLOADED,
        _ => error_type::INVALID_REQUEST,
    }
}
/// A failure in the Anthropic protocol's own vocabulary.
///
/// The machine code on the wire is the protocol's error type — the
/// same value the HTTP boundary put in the envelope's `error.type`.
fn anthropic_failure(
    kind: &'static str,
    message: impl Into<String>,
) -> RpcError {
    RpcError::application(kind, message)
}
/// The failure a model selection produces.
///
/// The selection is resolved into a domain error first, so the
/// machine code says which fault it is — a provider the deployment
/// configured but could not build is `model_unavailable`, a selection
/// nobody can resolve is `model_not_found` — and the Anthropic error
/// type follows from that code.
fn model_selection_failure(error: ModelSelectionError) -> RpcError {
    let message = error.to_string();
    let source = match &error {
        ModelSelectionError::Unavailable { .. } => Error::ModelUnavailable {
            message: message.clone(),
        },
        ModelSelectionError::UnknownProvider { .. }
        | ModelSelectionError::UnknownModel { .. }
        | ModelSelectionError::AmbiguousModel { .. } => Error::ModelNotFound {
            message: message.clone(),
        },
    };
    let kind = anthropic_error_type(RpcError::from(source).code);
    anthropic_failure(kind, message)
}
/// The failure a turn's start produces.
fn begin_turn_failure(error: BeginTurnError) -> RpcError {
    match error {
        BeginTurnError::AwaitingApproval => {
            anthropic_failure(error_type::INVALID_REQUEST, APPROVAL_REQUIRED)
        }
        BeginTurnError::Setup(source) => {
            let message = format!("could not start the agent run: {source}");
            let kind = anthropic_error_type(INTERNAL_ERROR_CODE);
            RpcError::new(source).with_code(kind).with_message(message)
        }
    }
}
/// How a request's turn is dispatched: the session it runs in, and the
/// agent that answers it.
struct Target {
    /// `None` when the client named no session — mint one, and own its
    /// whole lifetime. `Some` for a pinned id, which has already passed
    /// [`validate_resource_name`] by the time it is here.
    session: Option<String>,
    /// The client's `metadata.uva_agent_name`, handed to the same
    /// `explicit > configured default > first registered` ladder every
    /// dispatch resolves through. `None` resolves the default, exactly
    /// as a request with no `metadata` does.
    agent_name: Option<String>,
}
impl Target {
    /// Whether the request's client is one of ours — the gate for the
    /// tool blocks an external client must never be handed (see
    /// [`blocks`]).
    fn is_pinned(&self) -> bool {
        self.session.is_some()
    }
}
/// Whether a converted part carries image or audio bytes.
fn is_media_part(part: &uva::provider::ContentPart) -> bool {
    matches!(
        part,
        uva::provider::ContentPart::Image(_)
            | uva::provider::ContentPart::Audio(_)
    )
}

/// Decide which session the request's turn runs in, and which agent
/// answers it.
///
/// The only things this reads out of `metadata` are the two namespaced
/// keys [`Metadata`] models; everything else in the object (the
/// protocol's own `user_id` included) is ignored, as it always has been.
///
/// A named session is **adopted**: an id this server has never seen
/// becomes a session under the request's own user, which is what makes
/// the web client's first turn of a new conversation work without a
/// separate create call. What is never done is substituting a *different*
/// id — a caller that names a session and silently gets another would
/// keep appending to a conversation nobody reads.
///
/// # Errors
///
/// The reason a named id was refused, for the `invalid_request_error`
/// the handler answers with: a malformed one is the one case, and it
/// is refused because it reaches the sink's filesystem path.
fn resolve_target(metadata: Option<&Metadata>) -> Result<Target, String> {
    let Some(metadata) = metadata else {
        return Ok(Target {
            session: None,
            agent_name: None,
        });
    };
    let session = match metadata.uva_session_id.as_deref() {
        Some(session_id) => {
            validate_resource_name(session_id).map_err(|error| {
                format!(
                    "metadata.uva_session_id is not a usable session \
                     id: {error}"
                )
            })?;
            Some(session_id.to_string())
        }
        None => None,
    };
    Ok(Target {
        session,
        agent_name: metadata.uva_agent_name.clone(),
    })
}
/// A run that has been dispatched but has not finished.
struct StartedTurn {
    controller: Arc<SessionController>,
    events: broadcast::Receiver<AgentEvent>,
    accumulator: Turn,
    /// The session the run belongs to — the notification's
    /// `session_id`.
    session_id: String,
    /// `Some` only for a session this request minted. Dropped with the
    /// turn: evicts the session when the response stops being polled,
    /// including a client that vanishes mid-stream.
    ///
    /// `None` for a pinned session, which outlives the request — see
    /// [`close_turn`].
    ephemeral: Option<SessionSlot>,
}
/// Mint a session or continue the pinned one, replay the request's
/// history into it, and dispatch the final user turn through the shared
/// controller path.
async fn begin_turn(
    state: &Arc<AppState>,
    user_id: &str,
    model: &str,
    canonical: CanonicalRequest,
    target: Target,
    provider: Arc<dyn ModelProvider>,
) -> Result<StartedTurn, BeginTurnError> {
    let CanonicalRequest {
        system,
        history,
        turn,
        tools,
    } = canonical;
    tracing::debug!(
        target: "uva.server",
        tool_definitions = tools.len(),
        "anthropic request: tools are parsed but the run uses the server's own tool surface"
    );
    // Multimodal blocks need the mounted `attachments` service: an
    // unloaded plugin withdraws the store, and accepting the image /
    // audio turn anyway would run it without the capability the
    // caller asked for.
    if turn.iter().any(is_media_part) && state.attachments().is_none() {
        return Err(BeginTurnError::Setup(anyhow::anyhow!(
            "multimodal input is unavailable: the `attachments` service \
             is not mounted"
        )));
    }
    let ephemeral = !target.is_pinned();
    let session_id = match target.session {
        None => {
            let session_id = format!(
                "{EPHEMERAL_SESSION_PREFIX}{}",
                Uuid::new_v4().simple()
            );
            validate_resource_name(&session_id).map_err(|error| {
                anyhow::anyhow!("generated session id rejected: {error}")
            })?;
            session_id
        }
        Some(session_id) => session_id,
    };
    // A pinned session's transcript is the conversation: seeding the
    // client's view of it again would duplicate every earlier turn. Its
    // first turn is the exception — the session has no rows yet, and the
    // `system` and history that arrived with it belong in the log.
    if ephemeral || !session_holds_rows(state, user_id, &session_id).await {
        seed_history(state, user_id, &session_id, system, history).await?;
    }
    let controller = state
        .get_or_create_session_controller_with_parent(
            user_id,
            &session_id,
            None,
            None,
        )
        .await
        .map_err(|error| BeginTurnError::Setup(error.into()))?;
    // An open interrupt is a log fact. A fresh run would be queued and
    // never started (the controller refuses one while paused), so this
    // request would wait forever for a terminal event that cannot
    // come. Refuse it now, before anything is queued.
    if controller.session_state().await == SessionState::AwaitingApproval {
        return Err(BeginTurnError::AwaitingApproval);
    }
    // Subscribe before dispatching: the broadcaster only replays
    // nothing, so a subscriber that arrives after the run starts would
    // miss the opening frames.
    let events = controller.subscribe();
    controller
        .submit_durable_with_provider(
            SessionOp::PromptMulti {
                parts: turn,
                agent_name: state
                    .resolve_agent_name_for(target.agent_name.as_deref()),
                priority: 1,
            },
            // The pin is consumed by the run this op starts, so the
            // model this request named is the model that answers it.
            Some(
                PinnedProvider::new(provider).with_id(
                    state.provider_id(Some(model)).unwrap_or_default(),
                ),
            ),
        )
        .await?;
    Ok(StartedTurn {
        controller,
        events,
        accumulator: Turn::new(
            format!("msg_{}", Uuid::new_v4().simple()),
            model,
            !ephemeral,
        ),
        session_id: session_id.clone(),
        // `then`, not `then_some`: dropping a slot evicts the key, so a
        // pinned turn must never build one in the first place.
        ephemeral: ephemeral.then(|| SessionSlot {
            state: Arc::clone(state),
            key: (user_id.to_string(), session_id),
        }),
    })
}
/// Whether a session's durable log already holds rows.
///
/// A read failure counts as "no rows": the caller only uses this to
/// decide whether to seed, and seeding a log that cannot be read is the
/// same as seeding an empty one (the run's own sink read will surface
/// the failure).
async fn session_holds_rows(
    state: &AppState,
    user_id: &str,
    session_id: &str,
) -> bool {
    state
        .session_manager
        .sink(user_id, session_id)
        .read()
        .await
        .is_ok_and(|rows| !rows.is_empty())
}
/// Write the request's `system` and earlier turns into a session's sink,
/// so the run projects them back as real history.
///
/// Two sessions are the same conversation only if the client says so,
/// and a stateless client says so by resending the transcript — which
/// is why the rows are written on every request for a session this request
/// minted. A **pinned** session is the other case: its transcript is the
/// conversation, so it is seeded only while it has no rows yet (its
/// first turn, where the client's `system` and any history it sent
/// belong in the log). Seeding it on every turn would append the
/// conversation to itself — the same earlier turns on every request, and
/// a duplicate of each in the model's context. The caller decides; see
/// [`begin_turn`].
async fn seed_history(
    state: &Arc<AppState>,
    user_id: &str,
    session_id: &str,
    system: Option<String>,
    history: Vec<Message>,
) -> anyhow::Result<()> {
    // The session's one writer: the seeded rows draw from the same
    // ordinal sequence the run's rows will, and one flush at the
    // tail lands them together before the turn starts.
    let writer = state.session_manager.writer(user_id, session_id);
    if let Some(system) = system {
        append_message(
            &writer,
            Message::user(format!("{SYSTEM_LABEL}\n{system}")),
        )
        .await?;
    }
    for message in &history {
        append_message(&writer, message.clone()).await?;
    }
    writer.flush().await.map_err(|error| {
        anyhow::anyhow!("flush seeded session history: {error}")
    })
}
/// Append one canonical message as the typed row its role names.
///
/// The client's earlier turns are a full role-bearing list, so the row
/// tag has to carry the role: a tag-keyed consumer (the web UI's
/// projection) turns every `user_message` row into a user bubble, and
/// wrapping an assistant turn in one rendered the client's answer as a
/// user turn. `data` stays the same lossless `Message` envelope the
/// role-aware fold reads, exactly as before.
///
/// User and System both land as `user_message` — that is the role the
/// model and the fold see for an inline system prompt, and the existing
/// seeding path already writes the `system` field as one.
async fn append_message(
    writer: &Arc<uva::session::LogWriter>,
    message: Message,
) -> anyhow::Result<()> {
    use uva::{provider::Role, session::SessionEvent};

    let role = message.role;
    let data = serde_json::to_value(&message)
        .map_err(|error| anyhow::anyhow!("seed session history: {error}"))?;
    let event = match role {
        Role::User | Role::System => uva::session::user_message(data, None),
        Role::Assistant => SessionEvent::AssistantMessage {
            seq: 0,
            ts: String::new(),
            data,
            surface_op: None,
        },
        Role::Tool => SessionEvent::ToolResult {
            seq: 0,
            ts: String::new(),
            data,
            is_error: false,
            tool_call_id: message.tool_call_id,
            surface_op: None,
        },
    };
    writer
        .append_event(&event)
        .await
        .map(|_| ())
        .map_err(|error| anyhow::anyhow!("seed session history: {error}"))
}
/// Publish a run's frames as notifications, then close its session.
///
/// The stream the HTTP surface used to answer with is the same
/// sequence of frames, in the same order, with the same names and
/// payloads — only the carrier changed.
async fn publish_turn(
    outbound: Outbound,
    session_id: String,
    mut turn: StartedTurn,
) {
    publish_frames(
        &outbound,
        &session_id,
        &mut turn.accumulator,
        &mut turn.events,
    )
    .await;
    close_turn(&turn).await;
}
/// Pump the turn's frames until the run's terminal event, publishing
/// each one.
async fn publish_frames(
    outbound: &Outbound,
    session_id: &str,
    accumulator: &mut Turn,
    events: &mut broadcast::Receiver<AgentEvent>,
) {
    let (name, data) = accumulator.start().event();
    emit_event(outbound, session_id, name, data);
    loop {
        match events.recv().await {
            Ok(event) => {
                let terminal = is_terminal(&event);
                for frame in accumulator.on_event(&event) {
                    let (name, data) = frame.event();
                    emit_event(outbound, session_id, name, data);
                }
                if terminal {
                    break;
                }
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!(
                    target: "uva.server",
                    skipped,
                    "anthropic stream lagged behind the session broadcast"
                );
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
    for frame in accumulator.finish() {
        let (name, data) = frame.event();
        emit_event(outbound, session_id, name, data);
    }
}
/// Publish one Anthropic event as a notification.
///
/// The params object is built whole: this notification does not go
/// through the follow registry, which injects a `session_id` only for
/// the `session/event` method.
fn emit_event(
    outbound: &Outbound,
    session_id: &str,
    event: &'static str,
    data: Value,
) {
    outbound.notify(
        MESSAGES_EVENT,
        json!({
            "session_id": session_id,
            "event": event,
            "data": data,
        }),
    );
}
/// The non-streaming answer: fold the run to its terminal event and
/// serialise the assembled message.
///
/// A run that failed after the request was accepted answers with the
/// protocol's `api_error`, so a client never receives a finished
/// message for a turn the agent never completed.
async fn complete_response(turn: StartedTurn) -> Result<Value, RpcError> {
    let mut turn = turn;
    loop {
        match turn.events.recv().await {
            Ok(event) => {
                let terminal = is_terminal(&event);
                turn.accumulator.on_event(&event);
                if terminal {
                    break;
                }
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!(
                    target: "uva.server",
                    skipped,
                    "anthropic run lagged behind the session broadcast"
                );
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
    turn.accumulator.finish();
    let failure = match turn.accumulator.end_reason() {
        Some(SessionEndReason::Error(message)) => {
            Some((anthropic_error_type(INTERNAL_ERROR_CODE), message.clone()))
        }
        // A suspension is a terminal state for this protocol: the
        // client gets the protocol's error type, never a finished
        // message for a message that never finished.
        Some(SessionEndReason::Suspended) => {
            Some((error_type::INVALID_REQUEST, APPROVAL_REQUIRED.to_string()))
        }
        _ => None,
    };
    let message = turn.accumulator.message();
    close_turn(&turn).await;
    match failure {
        Some((kind, message)) => Err(anthropic_failure(kind, message)),
        None => serde_json::to_value(message).map_err(|error| {
            RpcError::from(Error::internal(format!(
                "could not serialise the anthropic message: {error}"
            )))
        }),
    }
}
/// End the run's session, for a session this request minted.
///
/// Closing finalises the durable log immediately instead of at the idle
/// timeout, and is the right thing for an `anthropic-*` session nothing
/// else will ever address again.
///
/// A **pinned** session gets the opposite treatment: it is left open, to
/// be reclaimed by the same `DEFAULT_IDLE_TIMEOUT` every `chat/*`
/// session is. Closing it would leave the next turn — and feedback,
/// cancel, regenerate and the session detail — addressing a controller
/// whose sink no longer accepts appends.
async fn close_turn(turn: &StartedTurn) {
    if turn.ephemeral.is_none() {
        return;
    }
    if let Err(error) = turn.controller.close().await {
        tracing::warn!(
            target: "uva.server",
            error = %error,
            "anthropic session close failed; the idle timeout will reclaim it"
        );
    }
}
/// The terminal event of a run.
///
/// A suspension ends the run (`SessionEnded { Suspended }`), and the
/// stream and the non-streaming body both turn that into the
/// protocol's error type: this endpoint can report the pause but
/// cannot answer it.
fn is_terminal(event: &AgentEvent) -> bool {
    matches!(event, AgentEvent::System(SystemEvent::SessionEnded { .. }))
}
/// Evicts a minted session from the controller cache when the response
/// stops being polled.
///
/// The normal path closes the controller first; this covers the
/// abandoned path — a client that disconnects mid-turn drops the
/// response future and no handler code runs again. Nothing else ever
/// evicts `AppState::active_sessions`, so without this every abandoned
/// request would pin a dead controller for the process's lifetime.
///
/// Only ever built for an ephemeral session: a pinned one is reachable
/// between turns, so [`close_turn`] never closes it and this is never
/// constructed for it.
struct SessionSlot {
    state: Arc<AppState>,
    key: (String, String),
}
impl Drop for SessionSlot {
    fn drop(&mut self) {
        self.state.active_sessions.remove(&self.key);
    }
}
#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    /// The model id every request below names.
    pub(crate) const MODEL: &str = "claude-test";
    /// A state whose provider answers, and whose outbound channel has
    /// a reader.
    ///
    /// [`AppState::for_test`] installs a **detached** RPC runtime — the
    /// envelope channel has no reader, so every send is dropped — and a
    /// streamed run publishes its events there. The fixture therefore
    /// replaces the runtime with a live one and keeps the receiving
    /// half, which is what makes those events observable.
    pub(crate) struct Fixture {
        /// The state the handler runs against.
        pub(crate) state: Arc<AppState>,
        /// The receiving half of the envelope channel.
        pub(crate) envelopes: tokio::sync::mpsc::UnboundedReceiver<Value>,
        /// Held so the on-disk session sinks outlive the state.
        pub(crate) _temp: tempfile::TempDir,
    }
    impl Fixture {
        /// Build the fixture.
        pub(crate) async fn new() -> Self {
            let temp = tempfile::TempDir::new().expect("a temp workspace");
            let registry = uva::session::manager::SessionRegistry::new(
                temp.path().join("sessions"),
            );
            let mut state =
                AppState::for_test(registry, temp.path().to_path_buf()).await;
            let provider: Arc<dyn ModelProvider> =
                Arc::new(uva::test_support::FakeProvider::text("hello"));
            state.override_default_provider_for_test(Arc::clone(&provider));
            state.override_catalogue_for_test(Arc::new(
                crate::state::ProviderCatalogue::default().with_handle(
                    "anthropic",
                    MODEL,
                    Arc::clone(&provider),
                ),
            ));
            let runtime = Arc::new(crate::rpc::RpcRuntime::new());
            let envelopes = runtime
                .take_receiver()
                .await
                .expect("the live runtime hands over its receiver");
            state.rpc = Arc::clone(&runtime);
            Fixture {
                state: Arc::new(state),
                envelopes,
                _temp: temp,
            }
        }

        /// Run the handler against this fixture.
        pub(crate) async fn call(
            &self,
            request: AnthropicRequest,
        ) -> Result<MessagesOutcome, RpcError> {
            messages(Arc::clone(&self.state), request).await
        }

        /// The next envelope the process emitted.
        pub(crate) async fn next_envelope(&mut self) -> Value {
            tokio::time::timeout(Duration::from_secs(10), self.envelopes.recv())
                .await
                .expect("an envelope before the deadline")
                .expect("the envelope channel is open")
        }
    }
    /// A Messages request whose only user turn is `text`.
    pub(crate) fn request(stream: bool) -> AnthropicRequest {
        serde_json::from_value(json!({
            "model": MODEL,
            "max_tokens": 64,
            "messages": [{ "role": "user", "content": "hi" }],
            "stream": stream,
        }))
        .expect("the request body must parse")
    }
    /// The generated session ids must satisfy the resource-name rule
    /// the sink's path construction enforces, or every request would
    /// fail at `validate_resource_name`.
    #[test]
    fn generated_session_ids_are_valid_resource_names() {
        for _ in 0..8 {
            let id = format!(
                "{EPHEMERAL_SESSION_PREFIX}{}",
                Uuid::new_v4().simple()
            );
            validate_resource_name(&id).expect("generated id must validate");
        }
    }
    /// The error envelope is the protocol's, not the application's.
    #[test]
    fn error_envelope_uses_the_anthropic_shape() {
        let envelope =
            wire::ErrorEnvelope::new(error_type::INVALID_REQUEST, "bad body");
        let json = envelope.value();
        assert_eq!(json["type"], "error");
        assert_eq!(json["error"]["type"], "invalid_request_error");
        assert_eq!(json["error"]["message"], "bad body");
        assert!(
            json.get("code").is_none(),
            "the application envelope must not leak: {json}"
        );
    }
    /// Only `SessionEnded` terminates a run's stream.
    #[test]
    fn only_session_ended_is_terminal() {
        assert!(is_terminal(&AgentEvent::System(
            SystemEvent::SessionEnded {
                reason: uva::harness::SessionEndReason::Completed,
            }
        )));
        assert!(!is_terminal(&AgentEvent::text_delta("more")));
        assert!(!is_terminal(&AgentEvent::System(
            SystemEvent::SessionStarted {
                session_id: "s".to_string(),
            }
        )));
    }
    /// The failure's machine code says which Anthropic error type it
    /// carries — the mapping the HTTP boundary keyed off the status.
    #[test]
    fn the_machine_code_picks_the_anthropic_error_type() {
        assert_eq!(anthropic_error_type(INTERNAL_ERROR_CODE), error_type::API);
        assert_eq!(
            anthropic_error_type("model_unavailable"),
            error_type::OVERLOADED
        );
        assert_eq!(
            anthropic_error_type("model_not_found"),
            error_type::INVALID_REQUEST
        );
        assert_eq!(
            anthropic_error_type("invalid_request"),
            error_type::INVALID_REQUEST
        );
    }
    /// Seeded history keeps each message's role in the row *tag*, not
    /// only inside the payload: the model-context fold is role-aware, but
    /// tag-keyed readers are not. The web UI turns every `user_message`
    /// row into a user bubble, so wrapping an assistant turn in one
    /// rendered the client's answer as a user turn (and could make it
    /// "the user's prompt").
    #[tokio::test]
    async fn seeded_history_keeps_each_messages_role() {
        use uva::session::{LogWriter, SessionSink};

        let sink = Arc::new(uva::session::in_memory::InMemorySessionSink::new(
            "seed-roles",
        ));
        let writer = Arc::new(LogWriter::new(
            sink.clone(),
            Arc::new(uva::core::SystemClock),
        ));
        append_message(&writer, Message::user("hello"))
            .await
            .expect("the user turn seeds");
        append_message(&writer, Message::assistant("hi"))
            .await
            .expect("the assistant turn seeds");
        append_message(&writer, Message::tool("ok", "call-1"))
            .await
            .expect("the tool result seeds");
        writer.flush().await.expect("the seeded rows flush");

        let rows = sink.read().await.expect("the rows read back");
        let tags: Vec<&str> =
            rows.iter().filter_map(|row| row["type"].as_str()).collect();
        assert_eq!(
            tags,
            ["user_message", "assistant_message", "tool_result"],
            "each role gets its own typed row: {rows:?}"
        );
        assert_eq!(rows[1]["data"]["role"], json!("assistant"));
        assert_eq!(rows[2]["data"]["role"], json!("tool"));
        assert_eq!(
            rows[2]["tool_call_id"],
            json!("call-1"),
            "the tool result still answers the call it names"
        );
    }
}
