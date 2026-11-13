//! Blank-agent descriptor factory: the `AgentDescriptor` a caller gets when it has a system prompt but no identity of its own.
/// Capabilities every `ReActAgent` advertises when no caller
/// overrode the descriptor.
pub(super) const DEFAULT_CAPABILITIES: &[&str] =
    &["tools", "streaming", "cancellation"];

/// Build a fresh descriptor with `system_prompt` baked in and
/// every other field at its "blank agent" default. The
/// fill-the-blanks descriptor `with_descriptor` skips.
pub fn default_descriptor(
    system_prompt: String,
) -> crate::agent::AgentDescriptor {
    crate::agent::AgentDescriptor {
        name: "agent".to_string(),
        display_name: Some("Uva".to_string()),
        description: String::new(),
        kind: "react".to_string(),
        version: "1.0.0".to_string(),
        instructions: system_prompt,
        capabilities: DEFAULT_CAPABILITIES
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        tools: Vec::new(),
        model_hint: None,
        handoffs: Vec::new(),
        handoff_hint: None,
        output_schema: None,
        owner: None,
        domain: None,
        persona: None,
        max_iterations: None,
    }
}
