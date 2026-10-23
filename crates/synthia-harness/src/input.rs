//! What a caller hands to one [`Agent::run`](crate::agent::Agent::run): multimodal content, the prior transcript, an optional prompt-manifest override, and the sub-agent depth the delegation plugin stamps on.
use std::sync::Arc;

use synthia_provider::{Content, ContentPart, Message, Role, TextContent};

use crate::prompt::PromptContext;

/// Input to the agent run loop. Can contain text, images, or a mix of content parts.
///
/// Construct with [`AgentInput::new`] (text or multimodal payload) or
/// [`AgentInput::multi_with_history`] (continues a prior conversation).
/// Chain optional fields through [`AgentInput::prompt_context`] and
/// [`AgentInput::thread_id`] builder-style setters.
#[derive(Clone, Debug)]
pub struct AgentInput {
    pub content: Vec<ContentPart>,
    pub history: Vec<Message>,
    /// Optional per-dispatch prompt manifest (skills + peer
    /// agents). When `Some`,
    /// [`crate::agent::Agent::run`] uses this in `prepare()`
    /// instead of the agent's own `prompt_context`. `None` uses the
    /// agent's manifest. Behind an `Arc` so a fan-out shares one
    /// manifest without cloning it.
    pub prompt_context: Option<Arc<PromptContext>>,
    /// Child-run depth (0 = a top-level session). The delegation
    /// plugin (`synthia-tool-task`) stamps it on the inputs it
    /// hands to child agents and refuses to spawn past its depth
    /// cap.
    pub subagent_depth: usize,
    /// Durable-resume key for this run. When `Some` and the
    /// agent was built with `ReActAgent::with_checkpointer`,
    /// the loop loads the snapshot stored under
    /// `<thread_id>:<agent_name>` at setup and resumes from it;
    /// on a non-`Completed` end it saves a fresh snapshot under
    /// the same key. `None` (the default) keeps the run one-shot.
    pub thread_id: Option<String>,
}

impl AgentInput {
    /// Create an input carrying a fresh user message.
    pub fn new(content: impl Into<Vec<ContentPart>>) -> Self {
        Self {
            content: content.into(),
            history: Vec::new(),
            prompt_context: None,
            subagent_depth: 0,
            thread_id: None,
        }
    }

    /// Attach a per-dispatch prompt manifest. The next call to
    /// [`crate::agent::Agent::run`] will see this manifest in
    /// `prepare()` and use it to assemble the system prompt.
    #[must_use]
    pub fn prompt_context(mut self, ctx: Arc<PromptContext>) -> Self {
        self.prompt_context = Some(ctx);
        self
    }

    /// Attach a durable-resume thread id. Requires the agent
    /// to be built with `ReActAgent::with_checkpointer` for
    /// the load / save to happen; without one the field is
    /// inert.
    #[must_use]
    pub fn thread_id(mut self, thread_id: impl Into<String>) -> Self {
        self.thread_id = Some(thread_id.into());
        self
    }

    /// Convenience: a plain-text input. Equivalent to
    /// `AgentInput::new(vec![text_part(text.into())])`.
    pub fn text(text: impl Into<String>) -> Self {
        Self::new(vec![ContentPart::Text(TextContent {
            text: text.into(),
            cache_control: None,
        })])
    }

    /// Resume a session: replay the given prior conversation
    /// messages and end with the given fresh parts.
    pub fn multi_with_history(
        history: Vec<Message>,
        parts: Vec<ContentPart>,
    ) -> Self {
        Self {
            content: parts,
            history,
            prompt_context: None,
            subagent_depth: 0,
            thread_id: None,
        }
    }

    /// Convenience: a single-text input that resumes a session.
    pub fn history(history: Vec<Message>, prompt: impl Into<String>) -> Self {
        Self::multi_with_history(
            history,
            vec![ContentPart::Text(TextContent {
                text: prompt.into(),
                cache_control: None,
            })],
        )
    }

    /// Convert this input into a [`Message`] with [`Role::User`].
    pub(crate) fn to_message(&self) -> Message {
        let content = if self.content.len() == 1 {
            Content::Single(self.content[0].clone())
        } else {
            Content::Multi(self.content.clone())
        };
        Message {
            role: Role::User,
            content,
            tool_call_id: None,
            name: None,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_input() {
        let input = AgentInput::text("hello");
        assert_eq!(input.content.len(), 1);
        match &input.content[0] {
            ContentPart::Text(tc) => assert_eq!(tc.text, "hello"),
            _ => panic!("expected text content"),
        }
    }

    #[test]
    fn test_multi_input() {
        let parts = vec![
            ContentPart::Text(TextContent {
                text: "hello".to_string(),
                cache_control: None,
            }),
            ContentPart::Image(synthia_provider::ImageContent {
                data: "img".to_string(),
                mime_type: "image/png".to_string(),
                detail: None,
            }),
        ];
        let input = AgentInput::new(parts);
        assert_eq!(input.content.len(), 2);
    }

    #[test]
    fn test_to_message_single() {
        let input = AgentInput::text("hello");
        let msg = input.to_message();
        assert_eq!(msg.role, Role::User);
        assert!(matches!(msg.content, Content::Single(_)));
    }

    #[test]
    fn test_to_message_multi() {
        let input = AgentInput::new(vec![
            ContentPart::Text(TextContent {
                text: "describe".to_string(),
                cache_control: None,
            }),
            ContentPart::Image(synthia_provider::ImageContent {
                data: "img".to_string(),
                mime_type: "image/png".to_string(),
                detail: None,
            }),
        ]);
        let msg = input.to_message();
        assert!(matches!(msg.content, Content::Multi(_)));
    }

    #[test]
    fn builder_chain_attaches_optional_fields() {
        let pc = Arc::new(PromptContext::default());
        let input = AgentInput::text("hi")
            .prompt_context(Arc::clone(&pc))
            .thread_id("conv-1");
        assert!(input.prompt_context.is_some());
        assert_eq!(input.thread_id.as_deref(), Some("conv-1"));
    }
}
