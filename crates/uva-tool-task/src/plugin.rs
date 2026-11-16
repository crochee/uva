//! The `tool-task` kernel plugin: the `task` tool and the control
//! surface's three tools as ordinary tool bricks.
//!
//! [`TaskToolPlugin`] holds a [`TaskTool`] value and files it in the
//! `tools` registry during [`apply`](uva_kernel::Plugin::apply). The
//! registration returns a `RegistrationToken`; the effect `apply`
//! returns withdraws exactly that token, so unloading the brick removes
//! the `task` entry and leaves every other tool in place. Everything a
//! tool brick normally gets — the schema projection, exposure,
//! restriction, execution mode, the model's visible list — is the
//! registry's, because `task` is now a plain tool and not a separate
//! delegation channel.
//!
//! The control surface's three tools (`agents_list` / `agents_message`
//! / `agents_interrupt`) ride the same brick: they read the handle
//! table the `task` tool fills, so they belong to the same lifetime
//! as the children they drive.
//!
//! # The registry is injected, not built
//!
//! The manifest injects `tools`, which the `tool-registry` row
//! provides. Mounting this brick without that service is reported as a
//! missing dependency instead of silently mounting a tool nobody can
//! call.
//!
//! # A refused registration fails loudly
//!
//! [`register_core_entry_with_token`](uva_tool::ToolRegistry::register_core_entry_with_token)
//! returns `None` when a Core tool already owns the name. `apply` turns
//! that into a [`PluginError`](uva_kernel::PluginError) naming the tool,
//! so the mount rolls back rather than shipping a brick whose tool never
//! appeared.

use std::sync::Arc;

use uva_core::{sleeper::Sleeper, spawn::SharedSpawner};
use uva_kernel::{
    Context,
    EffectHandle,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    PluginError,
    PluginManifest,
    RuntimeKind,
};
use uva_tool::{KEY_TOOLS, Tool, ToolEntry, ToolRegistry};

use crate::tool::TaskTool;

/// The plugin id the `tool-task` manifest declares.
pub const TASK_TOOL_PLUGIN_ID: &str = "tool-task";

/// Label of the effect that withdraws the `task` registration.
const TASK_TOOL_EFFECT: &str = "tool-task";

/// The `tool-task` kernel plugin: registers the `task` tool and the
/// control surface's three tools.
///
/// Mount it with [`uva_kernel::mount_all`] in the same group as (or
/// after) the `tool-registry` plugin. See the crate docs for the unload
/// contract and the refusal rule.
pub struct TaskToolPlugin {
    manifest: PluginManifest,
    tool: Arc<TaskTool>,
    park: Option<(SharedSpawner, Arc<dyn Sleeper>)>,
}

impl TaskToolPlugin {
    /// Wrap `tool` as a brick.
    #[must_use]
    pub fn new(tool: Arc<TaskTool>) -> Self {
        let mut manifest =
            PluginManifest::new(TASK_TOOL_PLUGIN_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.inject.push(KEY_TOOLS.to_string());
        Self {
            manifest,
            tool,
            park: None,
        }
    }

    /// Install the runtime seams the lifecycle's parking loop runs on.
    ///
    /// The loop itself starts on the first dispatch (or control call),
    /// so installing the seams is safe in any context; a host that
    /// does not install them still ages children whenever a control
    /// tool runs.
    #[must_use]
    pub fn with_park_loop(
        mut self,
        spawner: SharedSpawner,
        sleeper: Arc<dyn Sleeper>,
    ) -> Self {
        self.park = Some((spawner, sleeper));
        self
    }

    /// The tool this brick files.
    #[must_use]
    pub fn tool(&self) -> &Arc<TaskTool> {
        &self.tool
    }
}

impl std::fmt::Debug for TaskToolPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskToolPlugin")
            .field("manifest", &self.manifest.id)
            .finish_non_exhaustive()
    }
}

impl KernelPlugin for TaskToolPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        let tools = ctx.require::<ToolRegistry>(KEY_TOOLS)?;
        let surface = self.tool.control();
        if let Some((spawner, sleeper)) = self.park.clone() {
            surface.set_park_loop(spawner, sleeper);
        }
        let entries: Vec<(Arc<dyn Tool>, &str)> = vec![
            (Arc::clone(&self.tool) as Arc<dyn Tool>, "task"),
            (
                surface.list_tool() as Arc<dyn Tool>,
                crate::control::AGENTS_LIST_TOOL_NAME,
            ),
            (
                surface.message_tool() as Arc<dyn Tool>,
                crate::control::AGENTS_MESSAGE_TOOL_NAME,
            ),
            (
                surface.interrupt_tool() as Arc<dyn Tool>,
                crate::control::AGENTS_INTERRUPT_TOOL_NAME,
            ),
        ];
        let mut tokens: Vec<uva_tool::RegistrationToken> =
            Vec::with_capacity(entries.len());
        for (tool, name) in entries {
            let Some(token) =
                tools.register_core_entry_with_token(ToolEntry::new(tool))
            else {
                // Roll the brick back: a half-mounted control surface
                // would leave the model with a tool that answers for a
                // table nobody fills.
                for token in &tokens {
                    tools.unregister_by_token(token.clone());
                }
                return Err(PluginError::new(format!(
                    "the `{KEY_TOOLS}` registry refused to register the \
                     `{name}` tool: a core tool already owns the name `{name}`"
                )));
            };
            tokens.push(token);
        }
        let registry = Arc::clone(&tools);
        let rollback = tokens.clone();
        let unregister = ctx.effect(TASK_TOOL_EFFECT, move || {
            for token in &tokens {
                registry.unregister_by_token(token.clone());
            }
            Ok(())
        });
        let effect = match unregister {
            Ok(effect) => effect,
            Err(error) => {
                for token in &rollback {
                    tools.unregister_by_token(token.clone());
                }
                return Err(error);
            }
        };
        Ok(vec![effect])
    }
}
