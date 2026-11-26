//! The `goal` tool brick: the durable objective, as a native kernel
//! plugin.
//!
//! # One brick, one registration
//!
//! [`GoalToolPlugin`] holds the [`GoalTool`] value and files it in the
//! `tools` registry during [`apply`](uva_kernel::Plugin::apply). The
//! registration returns a
//! [`RegistrationToken`](uva_tool::RegistrationToken); the effect
//! `apply` returns withdraws exactly that token, so unloading the
//! brick removes its `goal` entry and leaves every other tool in
//! place.
//!
//! # The store is injected
//!
//! The host implements [`GoalStore`](crate::goal::GoalStore) over the
//! session's one writer, so the tool never spells a row: the log stays
//! the single truth for the goal's state.

use std::sync::Arc;

use uva_kernel::{
    Context,
    EffectHandle,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    PluginError,
    PluginManifest,
    RuntimeKind,
};
use uva_tool::{
    KEY_TOOLS,
    Tool,
    ToolEntry,
    ToolProvenance,
    register_tool_entries,
};

use crate::goal::{GoalStore, GoalTool};

/// The plugin id the `tool-goal` manifest declares.
const GOAL_TOOL_ID: &str = "tool-goal";

/// Label of the effect that withdraws the `goal` tool registration.
const GOAL_TOOL_EFFECT: &str = "tool-goal";

/// The `goal` kernel plugin: registers the `goal` tool.
#[derive(Debug)]
pub struct GoalToolPlugin {
    manifest: PluginManifest,
    tool: Arc<GoalTool>,
}

impl GoalToolPlugin {
    /// Build the brick over the store that owns the goal's rows.
    #[must_use]
    pub fn new(store: Arc<dyn GoalStore>) -> Self {
        let mut manifest =
            PluginManifest::new(GOAL_TOOL_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.inject.push(KEY_TOOLS.to_string());
        Self {
            manifest,
            tool: Arc::new(GoalTool::new(store)),
        }
    }
}

impl KernelPlugin for GoalToolPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        let tool: Arc<dyn Tool> = self.tool.clone();
        let effect = register_tool_entries(
            ctx,
            vec![ToolEntry::new(tool)],
            ToolProvenance::Core,
            GOAL_TOOL_EFFECT,
        )?;
        Ok(vec![effect])
    }
}
