//! The `session_send_message` tool: the model talks to **another**
//! session.
//!
//! dsh ships this surface (`packages/session/tool-session-control`,
//! the `session_send_message` tool) and gates it hard: "Use this tool
//! only when the user explicitly asks you to send a message to
//! another session." The capability it buys is **cross-session
//! coordination** — a long-running research session that must be
//! told the plan changed, a colleague session whose answer the next
//! step needs — without the caller owning the callee's run.
//!
//! # The delivery
//!
//! The message rides the same path a host's steer rides: a live run
//! takes it at its next step boundary (the durable row lands first,
//! the inbox second); an idle session queues it as its next turn. The
//! recipient sees who sent it — the message arrives **attributed**,
//! exactly the way dsh attributes it:
//!
//! ```text
//! Session <caller> sent a message: <the message>
//! ```
//!
//! # The seam
//!
//! Who resolves a session and how a message is delivered is the
//! host's business, not the tool's: the tool holds one
//! [`SessionMessenger`] — the same seam `goal` holds for its store.
//! The shipped [`NoSessions`] answers nothing, so a deployment that
//! mounts the brick without a messenger fails closed instead of
//! talking to sessions it cannot find.

use std::sync::Arc;

use async_trait::async_trait;
use schemars_derive::JsonSchema;
use serde::Deserialize;
use uva_tool::{Context, ExecutionMode, Tool, ToolOutput};

/// The `session_message` tool brick as a native kernel plugin.
pub mod plugin;

pub use plugin::SessionMessageToolPlugin;

#[cfg(test)]
mod tests;

/// Where a delivered message landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// The recipient has a live run: the message joins it at the
    /// next step boundary.
    Steer,
    /// The recipient was idle: the message became its next turn.
    Queued,
}

impl Delivery {
    /// What the model reads.
    #[must_use]
    pub fn answer(&self) -> &'static str {
        match self {
            Delivery::Steer => {
                "delivered into the session's running turn; it joins \
                 the model's next request"
            }
            Delivery::Queued => {
                "queued as the session's next turn; it runs when the \
                 session next starts one"
            }
        }
    }
}

/// Who delivers a message to another session.
///
/// The host installs the implementation; the tool never speaks a
/// registry. The messenger decides what "another session" is — this
/// deployment's registry, a cluster peer's — and hands back where the
/// message landed.
#[async_trait]
pub trait SessionMessenger: Send + Sync {
    /// Deliver `message` from `caller` to `target`.
    ///
    /// The message is delivered **attributed**: the recipient is told
    /// who sent it, in the text itself.
    ///
    /// # Errors
    ///
    /// The messenger's own failure — an unknown target, a refused
    /// delivery; the string says why.
    async fn send(
        &self,
        caller: &str,
        target: &str,
        message: &str,
    ) -> Result<Delivery, String>;
}

/// The messenger that answers nothing: the default a deployment has
/// when it mounts the brick without a messenger.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSessions;

#[async_trait]
impl SessionMessenger for NoSessions {
    async fn send(
        &self,
        _caller: &str,
        target: &str,
        _message: &str,
    ) -> Result<Delivery, String> {
        Err(format!(
            "this deployment cannot deliver messages to other \
             sessions; '{target}' was not reached"
        ))
    }
}

/// What the tool is asked.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(extend("additionalProperties" = false))]
struct SessionMessageRequest {
    #[schemars(
        length(min = 1),
        description = "The session to deliver to. Not this session."
    )]
    session_id: String,
    #[schemars(
        length(min = 1),
        description = "The message. The recipient is told which \
                       session sent it."
    )]
    message: String,
}

/// `session_send_message` — deliver a message to another session.
///
/// dsh's own gate applies: use it when the user asks for it, not on
/// the model's own initiative — one session's agent deciding what
/// another session should hear is a decision that belongs to the
/// user.
pub struct SessionMessageTool {
    messenger: Arc<dyn SessionMessenger>,
}

impl std::fmt::Debug for SessionMessageTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionMessageTool").finish_non_exhaustive()
    }
}

impl SessionMessageTool {
    /// The tool that delivers through `messenger`.
    #[must_use]
    pub fn new(messenger: Arc<dyn SessionMessenger>) -> Self {
        Self { messenger }
    }
}

#[async_trait]
impl Tool for SessionMessageTool {
    fn name(&self) -> &str {
        "session_send_message"
    }

    fn description(&self) -> &str {
        "Send a message to another session. Use this only when the \
         user explicitly asks you to contact another session. The \
         recipient is told which session sent the message; a session \
         with a running turn takes it at its next step, an idle one \
         queues it as its next turn."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::to_value(schemars::schema_for!(SessionMessageRequest))
            .expect("SessionMessageRequest schema is always serializable")
    }

    /// The delivery reaches another session's queue: it runs alone.
    fn mode(&self) -> ExecutionMode {
        ExecutionMode::Sequential
    }

    async fn call(
        &self,
        input: serde_json::Value,
        context: &Context,
    ) -> ToolOutput {
        let request: SessionMessageRequest = match serde_json::from_value(input)
        {
            Ok(request) => request,
            Err(error) => {
                return ToolOutput::error(format!(
                    "Invalid arguments: {error}"
                ));
            }
        };
        // A session talking to itself is a prompt to itself: the
        // model already has that power, and a message that arrives
        // from "another session" but is its own would be a lie the
        // next request reads.
        if request.session_id == context.session_id {
            return ToolOutput::error(
                "this is the session you are running in; say it \
                 directly instead of sending it a message",
            );
        }
        if request.message.trim().is_empty() {
            return ToolOutput::error(
                "Invalid arguments: the message must not be blank",
            );
        }
        match self
            .messenger
            .send(&context.session_id, &request.session_id, &request.message)
            .await
        {
            Ok(delivery) => ToolOutput::text(format!(
                "Sent to session '{}'. {}.",
                request.session_id,
                delivery.answer()
            )),
            Err(message) => ToolOutput::error(message),
        }
    }
}
