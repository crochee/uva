//! The `write` tool brick: the writer made model-facing, as a native
//! kernel plugin.
//!
//! # One brick, one registration
//!
//! [`WriteToolPlugin`] holds the [`WriteTool`] value and files it in
//! the `tools` registry during
//! [`apply`](uva_kernel::Plugin::apply). The registration returns a
//! [`RegistrationToken`](uva_tool::RegistrationToken); the effect
//! `apply` returns withdraws exactly that token, so unloading the brick
//! removes its `write` entry and leaves every other tool — including a
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

use crate::write::WriteTool;

/// The plugin id the `tool-write` manifest declares.
const WRITE_TOOL_ID: &str = "tool-write";

/// Label of the effect that withdraws the `write` tool registration.
const WRITE_TOOL_EFFECT: &str = "tool-write";

/// The `write` kernel plugin: registers the `write` tool.
///
/// Mount it with [`uva_kernel::mount_all`] in the same group as (or
/// after) the `tool-registry` plugin. See the [module
/// docs](crate::write::plugin) for the unload contract and the refusal rule.
#[derive(Debug)]
pub struct WriteToolPlugin {
    manifest: PluginManifest,
    tool: Arc<WriteTool>,
}

impl WriteToolPlugin {
    /// Build the brick over a default [`WriteTool`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_owner(Arc::new(crate::files::LocalFileSystem))
    }

    /// Build the brick over the owner of the files it writes.
    ///
    /// A host whose client owns the workspace installs an owner that
    /// asks the client, so the tool writes the user's files.
    #[must_use]
    pub fn with_owner(owner: Arc<dyn crate::files::FileSystemOwner>) -> Self {
        let mut manifest =
            PluginManifest::new(WRITE_TOOL_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.inject.push(KEY_TOOLS.to_string());
        Self {
            manifest,
            tool: Arc::new(WriteTool::with_owner(owner)),
        }
    }
}

impl Default for WriteToolPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl KernelPlugin for WriteToolPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        let tool: Arc<dyn Tool> = self.tool.clone();
        let effect = register_tool_entries(
            ctx,
            vec![ToolEntry::new(tool)],
            ToolProvenance::Core,
            WRITE_TOOL_EFFECT,
        )?;
        Ok(vec![effect])
    }
}
