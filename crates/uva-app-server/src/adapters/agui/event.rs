//! The AG-UI event vocabulary.
//!
//! AG-UI (the Agent User Interaction Protocol) is the web
//! front-end's door: the browser renders a run as a stream of
//! protocol events, not as the core's own rows. The event types
//! here are the protocol's — the names on the wire are the
//! protocol's own (`RUN_STARTED`, `TEXT_MESSAGE_CONTENT`, …), so a
//! browser that decodes them decodes AG-UI.
//!
//! # The envelope
//!
//! Every event carries a `type` discriminator and the fields its
//! family defines. Optional fields are omitted rather than sent as
//! `null`: the protocol says an absent field means absent, and a
//! consumer that must accept both spellings carries that tolerance
//! forever.
//!
//! The fields the projection does not fill (a run's `result`, an
//! event's `metadata`) are simply not there: the adapter emits what
//! the core knows, not what the schema allows.

use serde::Serialize;

/// One AG-UI event.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(tag = "type")]
pub enum AguiEvent {
    /// The run began.
    #[serde(rename = "RUN_STARTED")]
    RunStarted {
        /// The conversation the run belongs to.
        #[serde(rename = "threadId")]
        thread_id: String,
        /// The run itself.
        #[serde(rename = "runId")]
        run_id: String,
    },
    /// The run ended.
    #[serde(rename = "RUN_FINISHED")]
    RunFinished {
        /// The conversation the run belongs to.
        #[serde(rename = "threadId")]
        thread_id: String,
        /// The run that ended.
        #[serde(rename = "runId")]
        run_id: String,
    },
    /// The run failed.
    #[serde(rename = "RUN_ERROR")]
    RunError {
        /// What went wrong.
        message: String,
        /// The machine code, when the failure carries one.
        #[serde(rename = "code", skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
    /// A text message began.
    #[serde(rename = "TEXT_MESSAGE_START")]
    TextMessageStart {
        /// The message the chunks belong to.
        #[serde(rename = "messageId")]
        message_id: String,
        /// Who speaks the message.
        role: String,
    },
    /// One chunk of a text message.
    #[serde(rename = "TEXT_MESSAGE_CONTENT")]
    TextMessageContent {
        /// The message the chunk belongs to.
        #[serde(rename = "messageId")]
        message_id: String,
        /// The chunk itself — never the accumulated text.
        delta: String,
    },
    /// The text message ended.
    #[serde(rename = "TEXT_MESSAGE_END")]
    TextMessageEnd {
        /// The message that ended.
        #[serde(rename = "messageId")]
        message_id: String,
    },
    /// The agent's reasoning began.
    #[serde(rename = "REASONING_START")]
    ReasoningStart,
    /// One chunk of the agent's reasoning.
    #[serde(rename = "REASONING_MESSAGE_CONTENT")]
    ReasoningMessageContent {
        /// The message the chunk belongs to.
        #[serde(rename = "messageId")]
        message_id: String,
        /// The chunk itself.
        delta: String,
    },
    /// The agent's reasoning ended.
    #[serde(rename = "REASONING_END")]
    ReasoningEnd,
    /// A tool call began.
    #[serde(rename = "TOOL_CALL_START")]
    ToolCallStart {
        /// The call's id — the same one its result carries.
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        /// The tool the call addresses.
        #[serde(rename = "toolCallName")]
        tool_call_name: String,
        /// The message the call belongs to.
        #[serde(
            rename = "parentMessageId",
            skip_serializing_if = "Option::is_none"
        )]
        parent_message_id: Option<String>,
    },
    /// One chunk of a tool call's arguments.
    #[serde(rename = "TOOL_CALL_ARGS")]
    ToolCallArgs {
        /// The call the chunk belongs to.
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        /// The chunk itself.
        delta: String,
    },
    /// The tool call's arguments are complete.
    #[serde(rename = "TOOL_CALL_END")]
    ToolCallEnd {
        /// The call that ended.
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
    },
    /// The tool call produced its result.
    #[serde(rename = "TOOL_CALL_RESULT")]
    ToolCallResult {
        /// The message the result belongs to.
        #[serde(rename = "messageId")]
        message_id: String,
        /// The call the result answers.
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        /// The result's content.
        content: String,
        /// Who produced the result.
        role: String,
    },
}
