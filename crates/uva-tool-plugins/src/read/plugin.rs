//! The `read` tool brick: the reader made model-facing, as a native
//! kernel plugin.
//!
//! # One brick, one registration
//!
//! [`ReadToolPlugin`] holds the [`ReadTool`] value and files it in the
//! `tools` registry during [`apply`](uva_kernel::Plugin::apply).
//! The registration returns a
//! [`RegistrationToken`](uva_tool::RegistrationToken); the effect
//! `apply` returns withdraws exactly that token, so unloading the brick
//! removes its `read` entry and leaves every other tool — including a
//! foreign entry filed under the same name — in place.
//!
//! # The registry is injected, not built
//!
//! The manifest injects `tools`, which the `tool-registry` plugin
//! provides ([`ToolRegistryPlugin`](uva_tool::ToolRegistryPlugin)).
//! Mounting this brick without that service is reported as a missing
//! dependency instead of silently mounting a tool nobody can call.
//!
//! # A refused registration fails loudly
//!
//! The lifecycle lives in
//! [`register_tool_entries`](uva_tool::register_tool_entries): the
//! registry refuses a name a Core tool already owns, and the helper
//! turns that into a [`PluginError`](uva_kernel::PluginError) naming
//! the tool and the core owner, so the mount rolls back rather than
//! shipping a brick whose tool never appeared.

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

use crate::read::ReadTool;

/// The plugin id the `tool-read` manifest declares.
const READ_TOOL_ID: &str = "tool-read";

/// Label of the effect that withdraws the `read` tool registration.
const READ_TOOL_EFFECT: &str = "tool-read";

/// The `read` kernel plugin: registers the `read` tool.
///
/// Mount it with [`uva_kernel::mount_all`] in the same group as (or
/// after) the `tool-registry` plugin. See the [module
/// docs](crate::read::plugin) for the unload contract and the refusal rule.
#[derive(Debug)]
pub struct ReadToolPlugin {
    manifest: PluginManifest,
    tool: Arc<ReadTool>,
}

impl ReadToolPlugin {
    /// Build the brick over a default [`ReadTool`].
    #[must_use]
    pub fn new() -> Self {
        let mut manifest =
            PluginManifest::new(READ_TOOL_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.inject.push(KEY_TOOLS.to_string());
        Self {
            manifest,
            tool: Arc::new(ReadTool::new()),
        }
    }
}

impl Default for ReadToolPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl KernelPlugin for ReadToolPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        let tool: Arc<dyn Tool> = self.tool.clone();
        let effect = register_tool_entries(
            ctx,
            vec![ToolEntry::new(tool)],
            ToolProvenance::Core,
            READ_TOOL_EFFECT,
        )?;
        Ok(vec![effect])
    }
}
