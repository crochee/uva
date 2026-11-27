//! The `session_message` tool brick: cross-session messaging made
//! model-facing, as a native kernel plugin.
//!
//! # One brick, one registration
//!
//! [`SessionMessageToolPlugin`] holds the [`SessionMessageTool`]
//! value and files it in the `tools` registry during
//! [`apply`](uva_kernel::Plugin::apply). The registration returns a
//! [`RegistrationToken`](uva_tool::RegistrationToken); the effect
//! `apply` returns withdraws exactly that token, so unloading the
//! brick removes its `session_send_message` entry and leaves every
//! other tool in place.
//!
//! # The messenger is installed, not invented
//!
//! The brick is built with the [`SessionMessenger`] the host
//! resolved — the default is [`NoSessions`](super::NoSessions), which
//! refuses every delivery, so a deployment that mounts the brick
//! without a messenger fails closed instead of talking to sessions it
//! cannot find.

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

use super::{NoSessions, SessionMessageTool, SessionMessenger};

/// The plugin id the `tool-session-message` manifest declares.
const SESSION_MESSAGE_TOOL_ID: &str = "tool-session-message";

/// Label of the effect that withdraws the registration.
const SESSION_MESSAGE_TOOL_EFFECT: &str = "tool-session-message";

/// The session-message kernel plugin: registers the
/// `session_send_message` tool.
///
/// Mount it with [`uva_kernel::mount_all`] in the same group as (or
/// after) the `tool-registry` plugin.
#[derive(Debug)]
pub struct SessionMessageToolPlugin {
    manifest: PluginManifest,
    tool: Arc<SessionMessageTool>,
}

impl SessionMessageToolPlugin {
    /// Build the brick whose deliveries fail closed: no messenger is
    /// attached, so every call says this deployment cannot deliver.
    #[must_use]
    pub fn new() -> Self {
        Self::with_messenger(Arc::new(NoSessions))
    }

    /// Build the brick that delivers through `messenger`.
    #[must_use]
    pub fn with_messenger(messenger: Arc<dyn SessionMessenger>) -> Self {
        let mut manifest = PluginManifest::new(
            SESSION_MESSAGE_TOOL_ID,
            PLUGIN_PROTOCOL_VERSION,
        );
        manifest.runtime = RuntimeKind::Native;
        manifest.inject.push(KEY_TOOLS.to_string());
        Self {
            manifest,
            tool: Arc::new(SessionMessageTool::new(messenger)),
        }
    }
}

impl Default for SessionMessageToolPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl KernelPlugin for SessionMessageToolPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        let tool: Arc<dyn Tool> = self.tool.clone();
        let effect = register_tool_entries(
            ctx,
            vec![ToolEntry::new(tool)],
            ToolProvenance::Core,
            SESSION_MESSAGE_TOOL_EFFECT,
        )?;
        Ok(vec![effect])
    }
}
