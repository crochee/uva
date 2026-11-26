//! The `lsp` tool brick: the language server, as a native kernel
//! plugin.
//!
//! # One brick, one registration
//!
//! [`LspToolPlugin`] holds the [`LspTool`] value and files it in the
//! `tools` registry during [`apply`](uva_kernel::Plugin::apply). The
//! registration returns a
//! [`RegistrationToken`](uva_tool::RegistrationToken); the effect
//! `apply` returns withdraws exactly that token, so unloading the
//! brick removes its `lsp` entry and leaves every other tool in
//! place.
//!
//! # The servers are injected
//!
//! The host implements [`LspServers`] over its
//! configuration ✓, so the tool asks for the server a language names
//! and the deployment decides which servers exist ✓.

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

use crate::lsp::{LspServers, LspTool};

/// The plugin id the `tool-lsp` manifest declares.
const LSP_TOOL_ID: &str = "tool-lsp";

/// Label of the effect that withdraws the `lsp` tool registration.
const LSP_TOOL_EFFECT: &str = "tool-lsp";

/// The `lsp` kernel plugin: registers the `lsp` tool.
pub struct LspToolPlugin {
    manifest: PluginManifest,
    servers: Arc<dyn LspServers>,
    owner: Arc<dyn crate::files::FileSystemOwner>,
}

impl std::fmt::Debug for LspToolPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LspToolPlugin").finish_non_exhaustive()
    }
}

impl LspToolPlugin {
    /// Build the brick over the servers the deployment knows ✓.
    #[must_use]
    pub fn new(servers: Arc<dyn LspServers>) -> Self {
        Self::with_owner(servers, Arc::new(crate::files::LocalFileSystem))
    }

    /// Build the brick over the servers and the owner of the files it
    /// reads ✓.
    #[must_use]
    pub fn with_owner(
        servers: Arc<dyn LspServers>,
        owner: Arc<dyn crate::files::FileSystemOwner>,
    ) -> Self {
        let mut manifest =
            PluginManifest::new(LSP_TOOL_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.inject.push(KEY_TOOLS.to_string());
        Self {
            manifest,
            servers,
            owner,
        }
    }
}

impl KernelPlugin for LspToolPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        let tool: Arc<dyn Tool> = Arc::new(
            LspTool::new(Arc::clone(&self.servers))
                .with_owner(Arc::clone(&self.owner)),
        );
        let effect = register_tool_entries(
            ctx,
            vec![ToolEntry::new(tool)],
            ToolProvenance::Core,
            LSP_TOOL_EFFECT,
        )?;
        Ok(vec![effect])
    }
}
