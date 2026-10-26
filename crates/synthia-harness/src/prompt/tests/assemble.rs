//! Tests for the public `PromptContext::assemble` contract.
//!
//! Cross-cutting assertions that exercise the full assembly
//! rather than a single renderer: the canonical XML order
//! of the default pack, byte-stability across calls,
//! absence of per-dispatch volatile facts in the system
//! prompt, multi-agent reuse of one builder, the no-tools
//! invariant, and the trimming of whitespace-only
//! instructions.

use super::{
    super::PromptContext,
    support::{descriptor_with_instructions, peer_descriptor},
};

// Section order below is a prompt-cache wire contract: a reorder invalidates
// every cached prefix, and this is the only test pinning *place*, not presence.
#[test]
fn default_pack_renders_canonical_xml_order() {
    let mut d = descriptor_with_instructions("BASE");
    d.output_schema = Some(r#"{"type":"object"}"#.to_string());
    let out = PromptContext::default()
        .with_skill("transcribe", "Transcribe audio files.")
        .with_agent(&peer_descriptor("planner"))
        .assemble(&d);

    let order = [
        "BASE",
        "<identity>",
        "</identity>",
        // The mandate sits with the identity block (an
        // instruction), ahead of the skills / agent menus
        // (catalogues). See module docs § "Assembly order".
        "<structured_output>",
        "</structured_output>",
        "<available_skills>",
        "</available_skills>",
        "<available_agents>",
        "</available_agents>",
    ];
    let mut cursor = 0usize;
    for needle in order {
        let hit = out[cursor..].find(needle).unwrap_or_else(|| {
            panic!("missing `{needle}` after position {cursor}")
        });
        cursor = hit + needle.len();
    }

    assert!(out.contains("<name>transcribe</name>"));
    assert!(out.contains("`planner`"));

    assert!(
        !out.contains("<available_tools>"),
        "tool schemas must not be assembled into the system prompt; got:\n{out}"
    );
    assert!(
        !out.contains("Use only these tools:"),
        "tool grounding belongs on the runtime, not in the prompt; got:\n{out}"
    );

    // No `<rules>` block is rendered — the closing rules
    // section was intentionally removed. This is a hard
    // regression guard: any future section that adds an
    // `<rules>` (or `</rules>`) tag must update this
    // assertion and the section list above together.
    assert!(
        !out.contains("<rules>") && !out.contains("</rules>"),
        "the system prompt must not contain a <rules> block; got:\n{out}"
    );
    assert!(
        !out.contains("Apply only these skills:"),
        "skill grounding moved to <available_skills>; got:\n{out}"
    );
    assert!(
        !out.contains("Hand off only to these agents:"),
        "agent grounding moved to <available_agents>; got:\n{out}"
    );
}

#[test]
fn assemble_is_pure_and_deterministic() {
    let d = descriptor_with_instructions("BASE");
    let ctx = PromptContext::default()
        .with_skill("transcribe", "Transcribe audio files.")
        .with_agent(&peer_descriptor("planner"));
    let a = ctx.assemble(&d);
    let b = ctx.assemble(&d);
    assert_eq!(a, b);
}

#[test]
fn assemble_supports_multiple_agents_via_one_builder() {
    let d1 = descriptor_with_instructions("AGENT_ONE");
    let d2 = descriptor_with_instructions("AGENT_TWO");
    let ctx = PromptContext::default()
        .with_skill("transcribe", "Transcribe audio files.")
        .with_agent(&peer_descriptor("planner"));
    let a = ctx.assemble(&d1);
    let b = ctx.assemble(&d2);
    assert!(a.starts_with("AGENT_ONE"));
    assert!(b.starts_with("AGENT_TWO"));
    assert!(a.contains("<name>transcribe</name>"));
    assert!(b.contains("<name>transcribe</name>"));
}

#[test]
fn assemble_is_byte_stable_across_calls() {
    let d = descriptor_with_instructions("BASE");
    let ctx = PromptContext::default()
        .with_skill("transcribe", "Transcribe audio files.")
        .with_agent(&peer_descriptor("planner"));
    let a = ctx.assemble(&d);
    let b = ctx.assemble(&d);
    assert_eq!(a, b);
}

// -- System-prompt stability ---------------------------------------

#[test]
fn assemble_never_emits_runtime_facts() {
    let d = descriptor_with_instructions("BASE");
    let out = PromptContext::default()
        .with_skill("transcribe", "Transcribe audio files.")
        .with_agent(&peer_descriptor("planner"))
        .assemble(&d);
    assert!(!out.contains("<env>") && !out.contains("</env>"));
    assert!(
        !out.contains("Working directory:"),
        "cwd must live in the snapshot, not the system prompt"
    );
    assert!(
        !out.contains("Workspace root:"),
        "worktree must live in the snapshot, not the system prompt"
    );
    assert!(
        !out.contains("Platform:"),
        "platform must live in the snapshot, not the system prompt"
    );
    assert!(
        !out.contains("Today:"),
        "today must live in the snapshot, not the system prompt"
    );
    assert!(
        !out.contains("Model: "),
        "model_id must live in the snapshot, not the system prompt"
    );
}

#[test]
fn whitespace_only_descriptor_instructions_are_dropped() {
    let d = descriptor_with_instructions("   \n\t  ");
    let out = PromptContext::default().assemble(&d);
    let first_tag = out.find('<').expect("identity tag present");
    assert!(out[..first_tag].is_empty());
}

#[test]
fn descriptor_instructions_are_the_base_prompt() {
    let d = descriptor_with_instructions("FIRST_AGENT_INSTRUCTIONS");
    let out = PromptContext::default().assemble(&d);
    assert!(out.starts_with("FIRST_AGENT_INSTRUCTIONS"));
}

#[test]
fn prompt_context_has_no_tools_field() {
    let ctx = PromptContext::default();
    let _ = (&ctx.skills, &ctx.agents);
}

#[test]
fn empty_manifests_drop_their_section() {
    let d = descriptor_with_instructions("BASE");
    let out = PromptContext::default().assemble(&d);
    assert!(out.contains("<identity>"));
    assert!(!out.contains("<available_skills>"));
    assert!(!out.contains("<available_agents>"));
}

#[test]
fn output_schema_adds_the_structured_output_requirement() {
    let mut descriptor =
        super::support::descriptor_with_instructions("Be helpful.");
    descriptor.output_schema = Some(r#"{"type":"object"}"#.to_string());

    let prompt = PromptContext::default().assemble(&descriptor);

    assert!(
        prompt.contains("<structured_output>"),
        "a declared schema must put the requirement in the prompt; got:\n{prompt}"
    );
    assert!(
        prompt.contains("structured_output"),
        "the section must name the tool the model has to call"
    );
}

#[test]
fn without_output_schema_no_structured_output_section() {
    let descriptor =
        super::support::descriptor_with_instructions("Be helpful.");
    let prompt = PromptContext::default().assemble(&descriptor);
    assert!(
        !prompt.contains("<structured_output>"),
        "no schema declared, so there is nothing to require; got:\n{prompt}"
    );
}

#[test]
fn output_schema_text_is_not_inlined_in_the_prompt() {
    let mut descriptor =
        super::support::descriptor_with_instructions("Be helpful.");
    descriptor.output_schema =
        Some(r#"{"type":"object","title":"UNIQUE_SCHEMA_MARKER"}"#.to_string());

    let prompt = PromptContext::default().assemble(&descriptor);

    assert!(
        !prompt.contains("UNIQUE_SCHEMA_MARKER"),
        "the schema belongs on the tools channel, not in the prompt; got:\n{prompt}"
    );
}
