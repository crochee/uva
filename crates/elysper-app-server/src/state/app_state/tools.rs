//! The tool registry's container and the `AppliedToolSurface` record
//! of what the `[tools]` config section did to it at boot.
//!
//! The built-in bricks register themselves: each is a profile row
//! (`read` / `write` / `shell` / `todo` / `web`) whose plugin
//! registers its tool as **core** in `apply`. The boot only creates
//! the empty registry the rows share.

use elysper::tool::ToolRegistry;

/// The empty registry the tool-plugin rows register into.
///
/// The bricks are profile rows now: dropping a row drops its tool,
/// and each brick registers as **core**, so the name cannot be taken
/// by a later registration — a `POST`, a host tool or a plugin that
/// claims `shell` is refused instead of shadowing the binary's own
/// implementation.
pub(crate) fn plugin_tool_registry() -> ToolRegistry {
    ToolRegistry::new()
}

/// The empty registry with the five bricks registered as core, for
/// tests that exercise the `[tools]` surface or MCP registration
/// without mounting the kernel. The rows do the same in `apply`.
#[cfg(test)]
pub(crate) fn seeded_tool_registry() -> ToolRegistry {
    use std::sync::Arc;

    use elysper::tool::ToolEntry;

    let registry = ToolRegistry::new();
    for entry in [
        ToolEntry::new(Arc::new(elysper::tool_read::ReadTool::new())),
        ToolEntry::new(Arc::new(elysper::tool_write::WriteTool::new())),
        ToolEntry::new(Arc::new(elysper::tool_shell::ShellTool::new())),
        ToolEntry::new(Arc::new(elysper::tool_todo::TodoWriteTool::new())),
        ToolEntry::new(Arc::new(elysper::tool_web::WebFetchTool::new())),
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
    fn a_brick_row_registers_core_so_a_plugin_cannot_shadow_it() {
        // The rows are the built-ins' home: mount the brick plugin
        // exactly as the kernel does, then prove the core guard.
        use std::sync::Arc;

        use elysper::kernel::{Context, mount_all};
        let registry = Arc::new(plugin_tool_registry());
        let registry_plugin =
            elysper::tool::ToolRegistryPlugin::new(Arc::clone(&registry));
        let ctx = Context::new();
        let bricks: Vec<Arc<dyn elysper::kernel::Plugin>> = vec![
            Arc::new(registry_plugin),
            Arc::new(elysper::tool_read::ReadToolPlugin::new()),
            Arc::new(elysper::tool_write::WriteToolPlugin::new()),
            Arc::new(elysper::tool_shell::ShellToolPlugin::new()),
            Arc::new(elysper::tool_todo::TodoWriteToolPlugin::new()),
            Arc::new(elysper::tool_web::WebFetchToolPlugin::new()),
        ];
        let _mounted =
            mount_all(&ctx, &bricks).expect("the registry + bricks mount");
        assert!(
            registry.core_owns("read"),
            "read must be registered as core by its row, or the guard \
             is unreachable and a plugin shadows the built-in"
        );
        assert!(
            !registry.register_entry(ToolEntry::new(Arc::new(Intruder))),
            "a registration claiming a built-in name must be refused"
        );
    }
}
