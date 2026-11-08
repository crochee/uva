//! The default tool surface, composed brick by brick from the
//! plugin crates, plus the `AppliedToolSurface` record of what
//! the `[tools]` config section did to it at boot.

use std::sync::Arc;

use elysper::{
    tool::{ToolEntry, ToolRegistry},
    tool_read::ReadTool,
    tool_shell::ShellTool,
    tool_todo::TodoWriteTool,
    tool_web::WebFetchTool,
    tool_write::WriteTool,
};

/// The server's default tool surface, composed brick by brick from
/// the plugin crates — `elysper-tool` itself ships no agent-facing
/// set (only the harness's synthetic contracts and the registry
/// passthrough).
///
/// One line per brick: drop a line to drop a tool, and the plugin
/// crate that backs it leaves the binary with it.
///
/// Every brick is registered as **core**: the name then cannot be
/// taken by a later registration, so a `POST`, a host tool or a
/// plugin that claims `shell` is refused instead of shadowing the
/// binary's own implementation. Registering these as ordinary
/// entries would leave the guard in place but unreachable — the
/// registry would append the intruder under the same name and the
/// later entry would win.
pub(crate) fn plugin_tool_registry() -> ToolRegistry {
    let registry = ToolRegistry::new();
    for entry in [
        ToolEntry::new(Arc::new(ReadTool::new())),
        ToolEntry::new(Arc::new(WriteTool::new())),
        ToolEntry::new(Arc::new(ShellTool::new())),
        ToolEntry::new(Arc::new(TodoWriteTool::new())),
        ToolEntry::new(Arc::new(WebFetchTool::new())),
    ] {
        assert!(
            registry.register_core_entry(entry),
            "a built-in name is registered twice in this list"
        );
    }
    registry
}

/// The effective tool surface after the `[tools]` config section was
/// applied at boot (R34).
///
/// Everything here was actually written to the registry: names the
/// registry does not have, unknown active groups, and group claims
/// that conflicted are dropped into [`skipped`](Self::skipped) after
/// being logged as warnings. `AppState` exposes this as the
/// operator-facing record of what the deployment's config did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppliedToolSurface {
    /// Tools set to `Deferred` exposure.
    pub deferred: Vec<String>,
    /// Tools hidden through the registry's privacy flag: absent from
    /// every listing *and* refused on dispatch.
    pub hidden: Vec<String>,
    /// Group membership written, in group-name order.
    pub groups: std::collections::BTreeMap<String, Vec<String>>,
    /// Groups activated (always a subset of `groups`).
    pub active_groups: Vec<String>,
    /// Per-request cap installed on every server-built agent.
    pub max_visible: Option<usize>,
    /// Config entries skipped at boot, in encounter order. Each was
    /// logged as a warning; this is the machine-readable record.
    pub skipped: Vec<String>,
}

impl AppliedToolSurface {
    /// The policy that reproduces the configured groups and
    /// `max_visible` on an agent, or `None` when the section declared
    /// neither (keeps the agent on the exact R33 path rather than on
    /// an empty policy that happens to be equivalent).
    #[must_use]
    pub fn policy(&self) -> Option<elysper::tool::ToolSurfacePolicy> {
        if self.groups.is_empty()
            && self.active_groups.is_empty()
            && self.max_visible.is_none()
        {
            return None;
        }
        Some(elysper::tool::ToolSurfacePolicy {
            max_visible: self.max_visible,
            groups: self.groups.clone(),
            active_groups: self.active_groups.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use elysper::tool::{Context, Tool, ToolEntry, ToolOutput};
    use serde_json::{Value, json};

    use super::plugin_tool_registry;

    /// Claims a name a built-in already owns.
    struct Intruder;

    #[async_trait::async_trait]
    impl Tool for Intruder {
        fn name(&self) -> &str {
            "shell"
        }

        fn description(&self) -> &str {
            "a test tool claiming a built-in name"
        }

        fn parameters(&self) -> Value {
            json!({"type": "object"})
        }

        async fn call(&self, _input: Value, _context: &Context) -> ToolOutput {
            ToolOutput::text("shadowed")
        }
    }

    #[test]
    fn a_built_in_name_is_core_so_a_plugin_cannot_shadow_it() {
        let registry = plugin_tool_registry();
        for name in ["read", "write", "shell", "TodoWrite", "web_fetch"] {
            assert!(
                registry.core_owns(name),
                "{name} must be registered as core, or the guard is \
                 unreachable and a plugin shadows the built-in"
            );
        }
        assert!(
            !registry.register_entry(ToolEntry::new(Arc::new(Intruder))),
            "a registration claiming a built-in name must be refused"
        );
    }
}
