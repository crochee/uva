//! System-prompt assembly: [`PromptContext::assemble`] builds the deterministic prompt (skills + peer agents + identity in XML-delimited order), while the volatile runtime facts ride the snapshot message instead so the provider cache stays hot.
mod snapshot;

pub(crate) use snapshot::RuntimeContext;
use synthia_provider::Message;

use crate::{AgentCaps, agent::AgentDescriptor, input::AgentInput};

/// The conversation every sampling path starts from: the assembled
/// system prompt (skipped when empty — a placeholder system message
/// would evict the provider's prefix cache for no content), the
/// caller's history, and the turn's user message.
pub(crate) fn base_transcript(
    assembled_system: &str,
    history: &[Message],
    user: Message,
) -> Vec<Message> {
    let mut messages = Vec::with_capacity(history.len() + 2);
    if !assembled_system.is_empty() {
        messages.push(Message::system(assembled_system));
    }
    messages.extend(history.iter().cloned());
    messages.push(user);
    messages
}

/// Per-dispatch manifest wins; the agent's default is the fallback.
pub(crate) fn resolve_prompt_context<'a>(
    input: &'a AgentInput,
    caps: &'a AgentCaps,
) -> &'a PromptContext {
    input
        .prompt_context
        .as_deref()
        .unwrap_or(&caps.model.prompt_context)
}

fn wrap(tag: &str, body: &str) -> String {
    format!("<{tag}>\n{body}\n</{tag}>")
}

fn trimmed_non_empty(s: &str) -> Option<&str> {
    let t = s.trim();
    if t.is_empty() { None } else { Some(t) }
}

fn push_block(out: &mut String, first: &mut bool, block: &str) {
    if !*first {
        out.push_str("\n\n");
    }
    out.push_str(block);
    *first = false;
}

// Renderers return `Option<String>`; `None` drops the section entirely,
// so the assembler never writes an empty `<tag/>`. All renderers are
// pure and take the descriptor per call, so one `PromptContext` can
// render for several agents.

pub const DEFAULT_SYSTEM_PROMPT: &str = "\
You are Synthia, an AI coding assistant. Your main goal is to complete \
the user's request thoroughly and accurately.

<action_safety>
Weigh each action by how easily it can be undone and how far its effects \
reach. Local, reversible work such as editing files and running tests is \
fine to do freely. Before executing any actions that are hard to reverse, \
reach shared external systems, or are otherwise risky or destructive, \
check with the user first.

Confirming is cheap; a mistaken action is not (such as lost work, messages \
you cannot unsend, deleted branches). For those cases, take the context, \
the action, and the user's instructions into account; by default, say \
what you plan to do and ask before doing it. Users can override that \
default — if they explicitly ask you to act more autonomously, you may \
proceed without confirmation, but still mind risks and consequences.

One approval is not a blank check. Approving something once (e.g. a git \
push) does not approve it in every later situation. Unless the user has \
authorized the action in advance, confirm with the user.

Examples of risky actions that warrant user confirmation:
- Destructive operations such as removing files or branches, dropping \
  database tables, killing processes, `rm -rf`, discarding uncommitted work
- Irreversible operations such as force-pushes (including overwriting \
  remote history), `git reset --hard`, amending commits already \
  published, removing or downgrading dependencies, changing CI/CD pipelines
- Actions others can see, or that change shared state: pushing code; \
  opening, closing, or commenting on PRs and issues; sending messages \
  (Slack, email, GitHub); posting to external services; changing shared \
  infrastructure or permissions

If you find unexpected state — unfamiliar files, branches, or \
configuration — investigate before deleting or overwriting; it may be \
the user's in-progress work.
</action_safety>

<tool_calling>
- Use specialized tools instead of bash commands whenever possible, as \
  this provides a better user experience. For file operations, prefer \
  dedicated file tools (read, edit, write) over `cat`/`sed`/`awk`/shell \
  echoes. Reserve shell tools exclusively for actual system commands \
  and terminal operations that require shell execution.
- NEVER use shell `echo`, `printf`, or other command-line tools to \
  communicate thoughts, explanations, or instructions to the user. \
  Output all communication directly in your response text instead.
- When multiple independent calls are safe to run in parallel, issue \
  them together in the same turn rather than serially.
</tool_calling>

<output_efficiency>
- Write like an excellent technical blog post — precise, well-structured, \
and clear, in complete sentences. Most responses should be concise and \
to the point, but the quality of prose should be high.
- Prefer simple, accessible language over dense technical jargon. Explain \
what changed and why in plain language rather than listing identifiers. \
Stay focused: avoid filler, repetition, over-the-top detail, and \
tangents the user did not ask for.
- Keep final responses proportional to task complexity.
- Do what has been asked; nothing more, nothing less. When the task is \
ambiguous, surface the ambiguity and ask one clarifying question \
rather than guessing.
</output_efficiency>

<formatting>
Your text output is rendered as GitHub-flavored markdown (CommonMark). \
Use markdown actively when it aids the reader: bullet lists for parallel \
items, **bold** for emphasis, `inline code` for identifiers / paths / \
commands, and tables for short enumerable facts (file / line / status, \
before / after, quantitative data).
</formatting>";

fn render_identity(descriptor: &AgentDescriptor) -> Option<String> {
    // The opening line names the agent for the model. We
    // prefer `descriptor.display_name()` (the human-readable
    // label — e.g. "Synthia") over the programmatic
    // `descriptor.name` slug ("agent") so the model
    // self-identifies with the persona the user sees in the
    // UI card, not with the internal routing id. The fallback
    // inside `display_name()` guarantees legacy descriptors
    // (no `display_name` set) still render the same identity
    // they did before the field was added.
    let mut body = format!(
        "You are `{}` ({} v{}).",
        descriptor.display_name(),
        descriptor.kind,
        descriptor.version
    );
    if let Some(persona) =
        descriptor.persona.as_deref().filter(|s| !s.is_empty())
    {
        body.push_str("\nPersona: ");
        body.push_str(persona);
    }
    if let Some(domain) = descriptor.domain.as_deref().filter(|s| !s.is_empty())
    {
        body.push_str("\nDomain: ");
        body.push_str(domain);
    }
    if !descriptor.capabilities.is_empty() {
        body.push_str("\nCapabilities: ");
        body.push_str(&descriptor.capabilities.join(", "));
    }
    Some(wrap("identity", &body))
}

fn render_structured_output(descriptor: &AgentDescriptor) -> Option<String> {
    let schema = descriptor.output_schema.as_deref()?;
    if schema.trim().is_empty() {
        return None;
    }
    Some(wrap(
        "structured_output",
        "This run must end by calling the `structured_output` tool with \
         every field filled exactly as its schema requires. Do not answer \
         in prose instead: the caller reads the tool's validated payload, \
         and a prose answer leaves it with no result. If validation fails, \
         correct the listed fields and call the tool again.",
    ))
}

fn render_skills(scope: &PromptContext) -> Option<String> {
    if scope.skills.is_empty() {
        return None;
    }
    // Each skill is a `<skill>` element with `<name>` /
    // `<description>` / `<location>` children, wrapped in a
    // `<skills>` envelope. The verbose envelope (one skill
    // per element, no JSON attributes) is what trained-on-
    // similar-SDK models parse natively.
    let mut skills = String::new();
    for (idx, (name, description)) in scope.skills.iter().enumerate() {
        let desc = if description.is_empty() {
            "(no description)"
        } else {
            description.as_str()
        };
        // `location` is the canonical relative path the
        // model uses to reason about where on disk the
        // skill lives. The agent's `workspace_root` is
        // surfaced in the runtime-context snapshot, so
        // this stays relative.
        if idx > 0 {
            skills.push('\n');
        }
        skills.push_str("  <skill>\n");
        skills.push_str(&format!("    <name>{name}</name>\n"));
        skills.push_str(&format!("    <description>{desc}</description>\n"));
        skills.push_str(&format!(
            "    <location>.agents/skills/{name}/SKILL.md</location>\n"
        ));
        skills.push_str("  </skill>");
    }
    let body = format!(
        "Load a skill with the `skill` tool when the task at hand \
         matches its description.\n{}",
        wrap("skills", &skills),
    );
    Some(wrap("available_skills", &body))
}

fn render_agents(scope: &PromptContext) -> Option<String> {
    if scope.agents.is_empty() {
        return None;
    }
    let mut body = String::from(
        "Hand off to one of these agents when their description fits:",
    );
    for a in &scope.agents {
        let desc = if a.description.is_empty() {
            "(no description)"
        } else {
            a.description.as_str()
        };
        let hint = a
            .handoff_hint
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|s| format!(" (use when: {s})"))
            .unwrap_or_default();
        body.push_str(&format!("\n- `{}` — {desc}{hint}", a.name));
    }
    Some(wrap("available_agents", &body))
}

/// Builder for the system-prompt manifest: feed stable facts via
/// [`PromptContext::with_skill`] / [`PromptContext::with_agent`], then render with
/// [`PromptContext::assemble`].
///
/// The descriptor is passed to `assemble` per call, so one builder serves every
/// agent in the process. Disabled skills are never pushed in. Of each
/// [`AgentDescriptor`] only `name`, `description`, and `handoff_hint` are read.
/// Tool schemas never appear here — they travel on the completion request's
/// `tools` field.
#[derive(Clone, Debug, Default)]
pub struct PromptContext {
    pub(crate) skills: Vec<(String, String)>,
    pub(crate) agents: Vec<AgentDescriptor>,
}

impl PromptContext {
    /// Append one skill.
    pub fn with_skill(
        mut self,
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        self.skills.push((name.into(), description.into()));
        self
    }

    /// Append one peer agent.
    pub fn with_agent(mut self, descriptor: &AgentDescriptor) -> Self {
        self.agents.push(descriptor.clone());
        self
    }

    /// Render the final system prompt: base instructions plus the XML-delimited sections, joined with `\n\n`.
    pub fn assemble(&self, descriptor: &AgentDescriptor) -> String {
        let mut out = String::new();
        let mut first = true;

        if let Some(base) = trimmed_non_empty(&descriptor.instructions) {
            push_block(&mut out, &mut first, base);
        }
        for section in [
            render_identity(descriptor),
            render_structured_output(descriptor),
            render_skills(self),
            render_agents(self),
        ]
        .into_iter()
        .flatten()
        {
            push_block(&mut out, &mut first, &section);
        }

        // Final-prompt trace. `debug` level (off by default)
        // so production logs stay clean; flip on via
        // `RUST_LOG=synthia_harness::prompt=debug` to inspect
        // exactly what the model sees. The `agent` field is
        // the descriptor name — same field name used
        // elsewhere in `re_act.rs::prepare` so log search
        // filters carry across modules.
        tracing::debug!(
            agent = %descriptor.name,
            bytes = out.len(),
            "assembled system prompt:\n{}",
            out
        );

        out
    }
}
#[cfg(test)]
mod tests;
