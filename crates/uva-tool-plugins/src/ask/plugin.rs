//! The `ask` tool brick: the operator question made model-facing, as
//! a native kernel plugin.
//!
//! # One brick, one registration
//!
//! [`AskToolPlugin`] holds the [`AskTool`] value and files it in the
//! `tools` registry during [`apply`](uva_kernel::Plugin::apply). The
//! registration returns a [`RegistrationToken`](uva_tool::RegistrationToken);
//! the effect `apply` returns withdraws exactly that token, so
//! unloading the brick removes its `ask` entry and leaves every
//! other tool — including a foreign entry filed under the same name —
//! in place.
//!
//! # The registry is injected, not built
//!
//! The manifest injects `tools`, which the `tool-registry` plugin
//! provides. Mounting this brick without that service is reported as
//! a missing dependency instead of silently mounting a tool nobody
//! can call.
//!
//! # The owner is installed, not invented
//!
//! The plugin is built with the [`QuestionOwner`] the host resolved —
//! the default is [`NoOperator`](super::NoOperator), which answers
//! nothing, so a deployment that mounts the brick without installing
//! an owner fails closed on every call rather than hanging a turn.

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

use super::{AskTool, NoOperator, QuestionOwner};

/// The plugin id the `tool-ask` manifest declares.
const ASK_TOOL_ID: &str = "tool-ask";

/// Label of the effect that withdraws the `ask` tool registration.
const ASK_TOOL_EFFECT: &str = "tool-ask";

/// The `ask` kernel plugin: registers the `ask` tool.
///
/// Mount it with [`uva_kernel::mount_all`] in the same group as (or
/// after) the `tool-registry` plugin. See the [module
/// docs](crate::ask) for the unload contract and the owner rule.
#[derive(Debug)]
pub struct AskToolPlugin {
    manifest: PluginManifest,
    tool: Arc<AskTool>,
}

impl AskToolPlugin {
    /// Build the brick whose `ask` fails closed: no operator is
    /// attached, so every call answers `NoOperator`.
    #[must_use]
    pub fn new() -> Self {
        Self::with_owner(Arc::new(NoOperator))
    }

    /// Build the brick whose `ask` goes to `owner`.
    #[must_use]
    pub fn with_owner(owner: Arc<dyn QuestionOwner>) -> Self {
        let mut manifest =
            PluginManifest::new(ASK_TOOL_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.inject.push(KEY_TOOLS.to_string());
        Self {
            manifest,
            tool: Arc::new(AskTool::new(owner)),
        }
    }
}

impl Default for AskToolPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl KernelPlugin for AskToolPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        let tool: Arc<dyn Tool> = self.tool.clone();
        let effect = register_tool_entries(
            ctx,
            vec![ToolEntry::new(tool)],
            ToolProvenance::Core,
            ASK_TOOL_EFFECT,
        )?;
        Ok(vec![effect])
    }
}
