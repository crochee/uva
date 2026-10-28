//! Tests for plugin-contributed prompt sections.
//!
//! A plugin registers a `PromptSection` on its `Host`; the run's
//! manifest picks the registrations up once at run start, and
//! `assemble` renders them right after the identity block. The tests
//! below pin the three halves of that link: the host's registrations
//! reach the run's manifest, the manifest's sections reach the
//! assembled prompt, and with no sections the prompt is what it always
//! was.

use std::sync::Arc;

use super::{
    super::PromptContext,
    support::{descriptor_with_instructions, peer_descriptor},
};
use crate::{
    AgentCaps,
    agent::host::{Host, PromptSection},
};

fn section(title: &str, body: &str) -> PromptSection {
    PromptSection {
        title: title.to_string(),
        body: body.to_string(),
    }
}

// An `AgentCaps` whose host holds `host`'s registrations: the shape the
// loop's constructor sees when it snapshots the run's manifest.
fn caps_with_host(host: Host) -> AgentCaps {
    let provider: Arc<dyn elysper_provider::ModelProvider> =
        Arc::new(elysper_test_support::FakeProvider::new(Vec::new()));
    let mut caps = AgentCaps::for_provider(provider);
    caps.control.host = host;
    caps
}

#[test]
fn host_sections_reach_the_assembled_prompt() {
    let mut host = Host::new();
    let _registration =
        host.prompt_section(section("Audit", "Always cite the source."));
    let caps = caps_with_host(host);

    let prompt = caps
        .run_prompt_context(None)
        .assemble(&descriptor_with_instructions("BASE"));

    assert!(
        prompt.contains("Always cite the source."),
        "the plugin's prompt section must reach the assembled prompt; \
         got:\n{prompt}"
    );
    assert!(
        prompt.contains("## Audit"),
        "the section's title must render as its heading; got:\n{prompt}"
    );
}

#[test]
fn sections_render_in_registration_order() {
    let mut host = Host::new();
    let _first = host.prompt_section(section("First heading", "alpha marker"));
    let _second = host.prompt_section(section("Second heading", "beta marker"));
    let _third = host.prompt_section(section("Third heading", "gamma marker"));
    let caps = caps_with_host(host);

    let prompt = caps
        .run_prompt_context(None)
        .assemble(&descriptor_with_instructions("BASE"));

    let mut cursor = 0usize;
    for needle in [
        "## First heading",
        "alpha marker",
        "## Second heading",
        "beta marker",
        "## Third heading",
        "gamma marker",
    ] {
        let hit = prompt[cursor..].find(needle).unwrap_or_else(|| {
            panic!(
                "`{needle}` must follow the previous section, in \
                 registration order; got:\n{prompt}"
            )
        });
        cursor += hit + needle.len();
    }
}

#[test]
fn sections_render_after_the_identity_block() {
    let mut descriptor = descriptor_with_instructions("BASE");
    descriptor.output_schema = Some(r#"{"type":"object"}"#.to_string());
    let mut host = Host::new();
    let _registration =
        host.prompt_section(section("Audit", "Always cite the source."));
    let caps = caps_with_host(host);

    let prompt = caps.run_prompt_context(None).assemble(&descriptor);

    let identity = prompt.find("</identity>").expect("identity block");
    let section_at = prompt.find("## Audit").expect("section heading");
    let mandate = prompt
        .find("<structured_output>")
        .expect("structured-output block");

    assert!(
        identity < section_at && section_at < mandate,
        "plugin sections belong after the identity block and before the \
         structured-output mandate; got:\n{prompt}"
    );
}

#[test]
fn an_empty_title_renders_the_body_alone() {
    let ctx = PromptContext::default()
        .with_sections(vec![section("", "Just a body.")]);
    let prompt = ctx.assemble(&descriptor_with_instructions("BASE"));

    assert!(
        prompt.contains("</identity>\n\nJust a body."),
        "a titleless section renders its body alone, right after the \
         identity block; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("## "),
        "an empty title must not render a heading; got:\n{prompt}"
    );
}

#[test]
fn a_title_only_section_renders_its_heading() {
    // Title set, body empty: the heading stands alone, with no trailing
    // blank line for `push_block` to double. Padded input pins the trim.
    let ctx = PromptContext::default()
        .with_sections(vec![section("  Only a title  ", "  \n ")]);
    let prompt = ctx.assemble(&descriptor_with_instructions("BASE"));

    assert!(
        prompt.ends_with("</identity>\n\n## Only a title"),
        "a body-less section is its heading alone, trimmed; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("## Only a title\n"),
        "no blank line may trail a heading-only section; got:\n{prompt}"
    );
}

#[test]
fn a_blank_section_renders_nothing() {
    let descriptor = descriptor_with_instructions("BASE");
    let ctx = PromptContext::default();
    let with_blanks = ctx
        .clone()
        .with_sections(vec![section("", ""), section("   ", "\n\t ")]);

    assert_eq!(
        with_blanks.assemble(&descriptor),
        ctx.assemble(&descriptor),
        "a section blank in both title and body must not change the prompt"
    );
    assert!(
        !with_blanks.has_renderable_sections(),
        "a blank registration must not count as renderable: the request \
         fingerprint consults the same predicate"
    );
    assert!(
        ctx.with_sections(vec![section("Audit", "")])
            .has_renderable_sections(),
        "a title alone is enough to render, so it is renderable"
    );
}

#[test]
fn no_sections_leave_the_prompt_byte_identical() {
    // A bare descriptor with only instructions, so the whole expected
    // prompt fits in one literal: this is the pre-section rendering,
    // pinned byte for byte.
    let mut descriptor = peer_descriptor("agent");
    descriptor.instructions = "BASE".to_string();
    let ctx = PromptContext::default();

    assert_eq!(
        ctx.clone().assemble(&descriptor),
        "BASE\n\n<identity>\nYou are `agent` (planner v1.0.0).\n</identity>",
        "with no sections registered the prompt must be exactly what it \
         was before sections existed"
    );
    assert_eq!(
        ctx.clone().assemble(&descriptor),
        ctx.with_sections(Vec::new()).assemble(&descriptor),
        "an empty section list must render nothing at all"
    );
}

#[test]
fn the_run_manifest_carries_the_host_registrations() {
    let mut host = Host::new();
    let _registration =
        host.prompt_section(section("Audit", "Always cite the source."));
    let caps = caps_with_host(host);

    let ctx = caps.run_prompt_context(None);

    assert_eq!(
        ctx.sections().len(),
        1,
        "the run manifest must carry the host's section"
    );
    assert_eq!(ctx.sections()[0].title, "Audit");
}

#[test]
fn a_section_registered_between_runs_appears_on_the_next_run() {
    let mut host = Host::new();
    let _first = host.prompt_section(section("First", "one"));
    let mut caps = caps_with_host(host);

    let run_one = caps.run_prompt_context(None);
    assert_eq!(run_one.sections().len(), 1);

    // Registered while the first run's manifest is already snapshotted.
    let _second = caps.control.host.prompt_section(section("Second", "two"));
    let run_two = caps.run_prompt_context(None);

    assert_eq!(
        run_one.sections().len(),
        1,
        "a section registered mid-run must not reach the run in flight"
    );
    assert_eq!(
        run_two.sections().len(),
        2,
        "the next run's manifest must pick the registration up"
    );
}

#[test]
fn a_dispatch_manifest_keeps_the_host_sections() {
    let mut host = Host::new();
    let _registration =
        host.prompt_section(section("Audit", "Always cite the source."));
    let caps = caps_with_host(host);
    let dispatch = Arc::new(
        PromptContext::default().with_skill("transcribe", "Transcribe."),
    );

    let ctx = caps.run_prompt_context(Some(&dispatch));
    let prompt = ctx.assemble(&descriptor_with_instructions("BASE"));

    assert!(
        prompt.contains("<name>transcribe</name>"),
        "the per-dispatch manifest still wins for skills; got:\n{prompt}"
    );
    assert!(
        prompt.contains("Always cite the source."),
        "a per-dispatch manifest must not drop the host's sections; \
         got:\n{prompt}"
    );
}

#[test]
fn a_host_without_sections_shares_the_agent_manifest() {
    let caps = caps_with_host(Host::new());

    let ctx = caps.run_prompt_context(None);

    assert!(
        Arc::ptr_eq(&ctx, &caps.model.prompt_context),
        "with nothing to add, the run must share the agent's manifest \
         instead of cloning its skills and peers"
    );
}
