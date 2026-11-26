//! The `grep` tool brick: the content searcher, as a native kernel plugin.
//!
//! # One brick, one registration
//!
//! [`GrepToolPlugin`] holds the [`GrepTool`] value and files it in the `tools`
//! registry during [`apply`](uva_kernel::Plugin::apply). The
//! registration returns a
//! [`RegistrationToken`](uva_tool::RegistrationToken); the effect
//! `apply` returns withdraws exactly that token, so unloading the
//! brick removes its `grep` entry and leaves every other tool —
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

use crate::grep::GrepTool;

/// The plugin id the `tool-grep` manifest declares.
const GREP_TOOL_ID: &str = "tool-grep";

/// Label of the effect that withdraws the `grep` tool registration.
const GREP_TOOL_EFFECT: &str = "tool-grep";

/// The `grep` kernel plugin: registers the `grep` tool.
///
/// Mount it with [`uva_kernel::mount_all`] in the same group as (or
/// after) the `tool-registry` plugin. See the [module
/// docs](crate::grep::plugin) for the unload contract and the
/// refusal rule.
#[derive(Debug)]
pub struct GrepToolPlugin {
    manifest: PluginManifest,
    tool: Arc<GrepTool>,
}

impl GrepToolPlugin {
    /// Build the brick over a default [`GrepTool`].
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
            PluginManifest::new(GREP_TOOL_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.inject.push(KEY_TOOLS.to_string());
        Self {
            manifest,
            tool: Arc::new(GrepTool::with_owner(owner)),
        }
    }
}

impl Default for GrepToolPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl KernelPlugin for GrepToolPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        let tool: Arc<dyn Tool> = self.tool.clone();
        let effect = register_tool_entries(
            ctx,
            vec![ToolEntry::new(tool)],
            ToolProvenance::Core,
            GREP_TOOL_EFFECT,
        )?;
        Ok(vec![effect])
    }
}
