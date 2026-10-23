//! Tests for the `<available_skills>` and
//! `<available_agents>` renderers.
//!
//! The XML envelope wraps every skill as a `<skill>`
//! element with `<name>` / `<description>` / `<location>`
//! children — the verbose-shape format models are
//! trained on. Peer agents with empty `handoff_hint`
//! must not render a stray `(use when: )` marker; the
//! assembler is the single source of truth for the
//! manifest set so disabled skills never reach the
//! prompt.

use super::{
    super::PromptContext,
    support::{descriptor_with_instructions, peer_descriptor},
};

#[test]
fn disabled_skills_are_never_pushed_by_caller() {
    let d = descriptor_with_instructions("BASE");
    let out = PromptContext::default()
        .with_skill("active", "on")
        // disabled-skill is omitted: caller is responsible
        // for filtering before pushing.
        .assemble(&d);
    assert!(out.contains("<name>active</name>"));
    assert!(!out.contains("<name>inactive</name>"));
}

/// The verbose XML envelope is a model-training wire format, not a style choice.
#[test]
fn available_skills_uses_opencode_xml_envelope() {
    let d = descriptor_with_instructions("BASE");
    let out = PromptContext::default()
        .with_skill("code-review", "Procedure for reviewing a change.")
        .assemble(&d);
    // Outer wrappers.
    assert!(
        out.contains("<available_skills>")
            && out.contains("</available_skills>"),
        "<available_skills> outer block missing; got:\n{out}"
    );
    assert!(
        out.contains("<skills>") && out.contains("</skills>"),
        "<skills> inner envelope missing; got:\n{out}"
    );
    // Per-skill envelope, pinned to industry shape.
    assert!(
        out.contains("<skill>"),
        "<skill> per-entry element missing; got:\n{out}"
    );
    assert!(
        out.contains("<name>code-review</name>"),
        "<name> child missing; got:\n{out}"
    );
    assert!(
        out.contains(
            "<description>Procedure for reviewing a change.</description>"
        ),
        "<description> child missing; got:\n{out}"
    );
    assert!(
        out.contains(
            "<location>.agents/skills/code-review/SKILL.md</location>"
        ),
        "<location> child missing; got:\n{out}"
    );
    // Legacy bullet format MUST be gone.
    assert!(
        !out.contains("- `code-review` —"),
        "legacy bullet-list format must be replaced by the XML \
         envelope; got:\n{out}"
    );
}

#[test]
fn agents_skip_empty_handoff_hint() {
    let d = descriptor_with_instructions("BASE");
    let mut p = peer_descriptor("p");
    p.handoff_hint = Some(String::new());
    let out = PromptContext::default().with_agent(&p).assemble(&d);
    assert!(!out.contains("(use when: )"));
    assert!(!out.contains("use when:"));
}
