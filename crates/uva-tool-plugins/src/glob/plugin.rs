//! The `glob` tool brick: the file finder, as a native kernel plugin.
//!
//! # One brick, one registration
//!
//! [`GlobToolPlugin`] holds the [`GlobTool`] value and files it in the `tools`
//! registry during [`apply`](uva_kernel::Plugin::apply). The
//! registration returns a
//! [`RegistrationToken`](uva_tool::RegistrationToken); the effect
//! `apply` returns withdraws exactly that token, so unloading the
//! brick removes its `glob` entry and leaves every other tool —
//! including a foreign entry filed under the same name — in place.
//!
//! # The registry is injected, not built
//!
//! The manifest injects `tools`, which the `tool-registry` plugin
//! provides ([`ToolRegistryPlugin`](uva_tool::ToolRegistryPlugin)).
//! Mounting this brick without that service is reported as a missing
//! dependency instead of silently mounting a tool nobody can call.

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

use crate::glob::GlobTool;

/// The plugin id the `tool-glob` manifest declares.
const GLOB_TOOL_ID: &str = "tool-glob";

/// Label of the effect that withdraws the `glob` tool registration.
const GLOB_TOOL_EFFECT: &str = "tool-glob";

/// The `glob` kernel plugin: registers the `glob` tool.
///
/// Mount it with [`uva_kernel::mount_all`] in the same group as (or
/// after) the `tool-registry` plugin. See the [module
/// docs](crate::glob::plugin) for the unload contract and the
/// refusal rule.
#[derive(Debug)]
pub struct GlobToolPlugin {
    manifest: PluginManifest,
    tool: Arc<GlobTool>,
}

impl GlobToolPlugin {
    /// Build the brick over a default [`GlobTool`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_owner(Arc::new(crate::files::LocalFileSystem))
    }

    /// Build the brick over the owner of the files it reads.
    ///
    /// A host whose client owns the workspace installs an owner that
    /// asks the client ✓.
    #[must_use]
    pub fn with_owner(owner: Arc<dyn crate::files::FileSystemOwner>) -> Self {
        let mut manifest =
            PluginManifest::new(GLOB_TOOL_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.inject.push(KEY_TOOLS.to_string());
        Self {
            manifest,
            tool: Arc::new(GlobTool::with_owner(owner)),
        }
    }
}

impl Default for GlobToolPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl KernelPlugin for GlobToolPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        let tool: Arc<dyn Tool> = self.tool.clone();
        let effect = register_tool_entries(
            ctx,
            vec![ToolEntry::new(tool)],
            ToolProvenance::Core,
            GLOB_TOOL_EFFECT,
        )?;
        Ok(vec![effect])
    }
}
