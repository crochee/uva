//! The three model tools of the control surface.
//!
//! | tool | what it does |
//! |---|---|
//! | [`AgentsListTool`] | the roster of this session's sub-agents |
//! | [`AgentsMessageTool`] | a message, dispatched by the target's state |
//! | [`AgentsInterruptTool`] | the current round of a running child stops |
//!
//! They are ordinary tools: their definitions, exposure, restriction
//! and concurrency mode are the tool registry's mechanisms, exactly
//! like the `task` tool's.

use std::sync::Arc;

use serde_json::{Value, json};
use uva_core::CancelToken;
use uva_provider::{Message, ToolDefinition};
use uva_session::user_message;
use uva_tool::{Context, ExecutionMode, Tool, ToolOutput};

use super::{
    ControlSurface,
    registry::{
        ColdWake,
        ControlError,
        FollowUpTarget,
        MessageTarget,
        SteerTarget,
    },
    scan,
};
use crate::{
    child::ChildCancel,
    round::{ChildRound, RoundInput, depth},
};

/// The tool the model calls to list this session's sub-agents.
pub const AGENTS_LIST_TOOL_NAME: &str = "agents_list";

/// The tool the model calls to message one of this session's
/// sub-agents.
pub const AGENTS_MESSAGE_TOOL_NAME: &str = "agents_message";

/// The tool the model calls to interrupt one of this session's
/// sub-agents.
pub const AGENTS_INTERRUPT_TOOL_NAME: &str = "agents_interrupt";

/// The roster of this session's sub-agents.
pub struct AgentsListTool {
    surface: Arc<ControlSurface>,
    definition: ToolDefinition,
}

impl AgentsListTool {
    /// The tool over `surface`.
    #[must_use]
    pub fn new(surface: Arc<ControlSurface>) -> Self {
        Self {
            surface,
            definition: list_definition(),
        }
    }
}

impl std::fmt::Debug for AgentsListTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentsListTool").finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl Tool for AgentsListTool {
    fn name(&self) -> &str {
        &self.definition.name
    }

    fn description(&self) -> &str {
        &self.definition.description
    }

    fn parameters(&self) -> Value {
        self.definition.input_schema.clone()
    }

    async fn call(&self, _input: Value, context: &Context) -> ToolOutput {
        let parent = context.session_id.as_str();
        // The lifecycle is advanced before the roster is read: an idle
        // child that crossed the TTL is parked — its memory released —
        // by the time the model sees it.
        self.surface.tick();
        // The table is the roster's source. It reconciles itself with
        // the durable sessions first, so a child a previous process
        // dispatched shows up as `parked` instead of vanishing.
        self.surface.refresh_roster(parent).await;
        let rows = self.surface.registry().list(parent);
        let children: Vec<Value> = rows
            .iter()
            .map(|row| {
                json!({
                    "id": row.id,
                    "name": row.name,
                    "status": row.status.as_str(),
                    "activity": row.activity,
                })
            })
            .collect();
        ToolOutput::text(json!({ "children": children }).to_string())
    }
}

/// A message to one of this session's sub-agents.
pub struct AgentsMessageTool {
    surface: Arc<ControlSurface>,
    definition: ToolDefinition,
}

impl AgentsMessageTool {
    /// The tool over `surface`.
    #[must_use]
    pub fn new(surface: Arc<ControlSurface>) -> Self {
        Self {
            surface,
            definition: message_definition(),
        }
    }
}

impl std::fmt::Debug for AgentsMessageTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentsMessageTool").finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl Tool for AgentsMessageTool {
    fn name(&self) -> &str {
        &self.definition.name
    }

    fn description(&self) -> &str {
        &self.definition.description
    }

    fn parameters(&self) -> Value {
        self.definition.input_schema.clone()
    }

    fn mode(&self) -> ExecutionMode {
        // A message can start a whole round of a child: the call owns
        // the child's session while it runs.
        ExecutionMode::Sequential
    }

    async fn call(&self, input: Value, context: &Context) -> ToolOutput {
        let Some(id) = input
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
        else {
            return ToolOutput::error(
                "`id` must name one of this session's sub-agents",
            );
        };
        let Some(text) = input
            .get("text")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
        else {
            return ToolOutput::error("`text` must carry the message");
        };
        let parent = context.session_id.as_str();
        // Age the table first: a child idle past the TTL is parked by
        // now, so the message takes the cold-wake branch instead of
        // following up a round whose handles are gone.
        self.surface.tick();
        // The depth rule: the session that starts the round may not
        // sit at the cap — the same judgment the `task` tool asks
        // before it delegates, about the same session.
        //
        // The rule is about **rounds**, so it is asked for the
        // branches that start one: a follow-up and a cold wake. A
        // steer starts none, so it is not refused — the refusal would
        // say "cannot start another round" about a message that
        // starts no round.
        //
        // The rule is asked before the roster is touched: the target
        // is read first, and `message_target` moves the row to
        // `running` itself. A refused message therefore leaves the
        // child's row where it was.
        let branch = self.surface.registry().message_branch(parent, id);
        if branch.starts_round()
            && let Err(refusal) =
                depth::admit_round(&self.surface.depth_ledger(), parent)
        {
            return ToolOutput::error(refusal.wake());
        }
        // The round a message starts is cancelled by the run that
        // starts it: the token the interrupted round left behind is
        // already fired.
        let cancel = ChildCancel::new(Arc::clone(&context.cancel));
        match self.surface.registry().message_target(parent, id, cancel) {
            Ok(MessageTarget::Steer(target)) => steer(target, text).await,
            Ok(MessageTarget::FollowUp(target)) => {
                run_round(&self.surface, parent, id, target, text).await
            }
            Ok(MessageTarget::ColdWake) => {
                self.cold_wake(parent, id, text, context).await
            }
            // A child this process does not hold is a child that
            // ended: the message wakes it from a cold start.
            Err(ControlError::UnknownChild(_)) => {
                self.cold_wake(parent, id, text, context).await
            }
            Err(error) => ToolOutput::error(error.to_string()),
        }
    }
}

impl AgentsMessageTool {
    /// Wake a child that is not in this process's memory.
    ///
    /// The wake reads the child's own log: the header names the agent
    /// it ran as, and the session's writer is where the new round's
    /// rows go. The child is running again when the round starts.
    async fn cold_wake(
        &self,
        parent: &str,
        child: &str,
        text: &str,
        context: &Context,
    ) -> ToolOutput {
        let Some(index) = self.surface.index() else {
            return ToolOutput::error(format!(
                "no session index is installed: the control surface \
                 cannot look up sub-agent `{child}`"
            ));
        };
        let records = scan::discover(
            index.as_ref(),
            scan::Scope::Parent(parent.to_string()),
        )
        .await
        .into_iter()
        // A mirror child has no readable log, so there is nothing to
        // wake: the wake would only fail on the writer it cannot get.
        .filter(|record| record.source == scan::RecordSource::OwnLog)
        .collect::<Vec<_>>();
        let Some(record) = records.iter().find(|record| record.id == child)
        else {
            return ToolOutput::error(format!(
                "no sub-agent `{child}` in this session"
            ));
        };
        let Some(peer) = self.surface.peers().resolve_sync(&record.agent_name)
        else {
            return ToolOutput::error(
                ControlError::UnknownPeer(record.agent_name.clone())
                    .to_string(),
            );
        };
        let Some(writer) = index.writer(child) else {
            return ToolOutput::error(ControlError::NoSession.to_string());
        };
        let cancel = ChildCancel::new(Arc::clone(&context.cancel));
        let target = match self.surface.registry().cold_wake(
            parent,
            child,
            ColdWake {
                name: record.agent_name.clone(),
                peer,
                writer: Some(writer),
                cancel,
                depth: record.depth as usize,
            },
        ) {
            Ok(target) => target,
            Err(error) => return ToolOutput::error(error.to_string()),
        };
        run_round(&self.surface, parent, child, target, text).await
    }
}

/// Stop the current round of a running sub-agent.
pub struct AgentsInterruptTool {
    surface: Arc<ControlSurface>,
    definition: ToolDefinition,
}

impl AgentsInterruptTool {
    /// The tool over `surface`.
    #[must_use]
    pub fn new(surface: Arc<ControlSurface>) -> Self {
        Self {
            surface,
            definition: interrupt_definition(),
        }
    }
}

impl std::fmt::Debug for AgentsInterruptTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentsInterruptTool")
            .finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl Tool for AgentsInterruptTool {
    fn name(&self) -> &str {
        &self.definition.name
    }

    fn description(&self) -> &str {
        &self.definition.description
    }

    fn parameters(&self) -> Value {
        self.definition.input_schema.clone()
    }

    fn mode(&self) -> ExecutionMode {
        // The interrupt ends a round that another call may be running:
        // it runs alone.
        ExecutionMode::Sequential
    }

    async fn call(&self, input: Value, context: &Context) -> ToolOutput {
        let Some(id) = input
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
        else {
            return ToolOutput::error(
                "`id` must name one of this session's sub-agents",
            );
        };
        let parent = context.session_id.as_str();
        self.surface.tick();
        match self.surface.registry().interrupt(parent, id) {
            Ok(token) => {
                token.cancel();
                ToolOutput::text(format!(
                    "the current round of sub-agent `{id}` was stopped; \
                     it can be asked for another one"
                ))
            }
            Err(error) => ToolOutput::error(error.to_string()),
        }
    }
}

/// Steer a running child: the row is the fact, the channel is how the
/// live round learns about it.
async fn steer(target: SteerTarget, text: &str) -> ToolOutput {
    if let Some(writer) = target.writer.as_ref() {
        let row = match serde_json::to_value(Message::user(text)) {
            Ok(data) => user_message(data, Some("steer")),
            Err(error) => {
                return ToolOutput::error(format!(
                    "the steer message could not be encoded: {error}"
                ));
            }
        };
        if let Err(error) = writer.append_event(&row).await {
            return ToolOutput::error(format!(
                "the steer row could not be written: {error}"
            ));
        }
        if let Err(error) = writer.flush().await {
            return ToolOutput::error(format!(
                "the steer row could not be flushed: {error}"
            ));
        }
    }
    if target.steer.unbounded_send(Message::user(text)).is_err() {
        return ToolOutput::error(
            "the sub-agent's run is no longer draining its inbox",
        );
    }
    ToolOutput::text("the running sub-agent was steered")
}

/// Run another round of a child and report its answer.
///
/// The round is the same one the `task` tool drives: the wake hands
/// it the child it already holds and reads back what it reports. What
/// the wake does not do is the delegation's business — it opens no
/// session, isolates nothing, runs no gate and files no child again.
async fn run_round(
    surface: &Arc<ControlSurface>,
    parent: &str,
    child: &str,
    target: FollowUpTarget,
    text: &str,
) -> ToolOutput {
    let agent = target.peer.descriptor().name.clone();
    // The host logs the turn before the run: the loop derives the
    // turn's text from the log, so the row has to be there first.
    if let Some(writer) = target.writer.as_ref() {
        let row = match serde_json::to_value(Message::user(text)) {
            Ok(data) => user_message(data, None),
            Err(error) => {
                return ToolOutput::error(format!(
                    "the message could not be encoded: {error}"
                ));
            }
        };
        if let Err(error) = writer.append_event(&row).await {
            return ToolOutput::error(format!(
                "the message could not be written: {error}"
            ));
        }
        if let Err(error) = writer.flush().await {
            return ToolOutput::error(format!(
                "the message could not be flushed: {error}"
            ));
        }
    }

    // The round runs at the depth the handle table remembers. The
    // depth table is what nested `task` calls consult, so the round
    // records it back before the child starts.
    depth::record_child(&surface.depth_ledger(), child, target.depth);

    // The round itself: the module that assembles the child's input,
    // drains the peer, maps the end reason into the roster's
    // vocabulary, lands the tail and reports what the parent is told.
    // A follow-up or a wake does not run in a worktree, so it leaves
    // the set's `.patch` (if any) untouched.
    let result = ChildRound::new(Arc::clone(surface))
        .run(RoundInput {
            peer: target.peer,
            prompt: text.to_string(),
            parent_id: parent.to_string(),
            child_id: child.to_string(),
            agent,
            depth: target.depth,
            writer: target.writer.clone(),
            inbox: target.inbox,
            cancel: target.cancel,
            isolation: None,
            feed: None,
        })
        .await;

    match result.answer(None) {
        Some(answer) => ToolOutput::text(answer),
        None => ToolOutput::error(result.failure()),
    }
}

/// The definition of the roster tool.
fn list_definition() -> ToolDefinition {
    ToolDefinition {
        name: AGENTS_LIST_TOOL_NAME.to_string(),
        description: "List this session's sub-agents: each one's id, \
                      name, state (running, idle, parked or aborted) \
                      and what it is doing now. Use the id to message \
                      or interrupt one."
            .to_string(),
        input_schema: schema(json!({}), json!([])),
        cache_control: None,
        annotations: None,
    }
}

/// The definition of the message tool.
fn message_definition() -> ToolDefinition {
    ToolDefinition {
        name: AGENTS_MESSAGE_TOOL_NAME.to_string(),
        description: "Send a message to one of this session's \
                      sub-agents. A running sub-agent is steered, an \
                      idle one is asked for another round, and one that \
                      already ended is woken up again."
            .to_string(),
        input_schema: schema(
            json!({
                "id": {
                    "type": "string",
                    "description": "The sub-agent's id, as \
                                    `agents_list` reported it."
                },
                "text": {
                    "type": "string",
                    "description": "The message to send."
                }
            }),
            json!(["id", "text"]),
        ),
        cache_control: None,
        annotations: None,
    }
}

/// The definition of the interrupt tool.
fn interrupt_definition() -> ToolDefinition {
    ToolDefinition {
        name: AGENTS_INTERRUPT_TOOL_NAME.to_string(),
        description: "Stop the current round of a running sub-agent. \
                      The sub-agent is not killed: it can be asked for \
                      another round afterwards."
            .to_string(),
        input_schema: schema(
            json!({
                "id": {
                    "type": "string",
                    "description": "The sub-agent's id, as \
                                    `agents_list` reported it."
                }
            }),
            json!(["id"]),
        ),
        cache_control: None,
        annotations: None,
    }
}

/// The JSON schema of a control tool's arguments.
fn schema(properties: Value, required: Value) -> Value {
    let mut builder = uva_tool::ToolSchemaBuilder::new();
    builder = builder.with("type", json!("object"));
    builder = builder.with("properties", properties);
    builder.with("required", required).build()
}
