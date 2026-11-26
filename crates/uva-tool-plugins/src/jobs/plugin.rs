//! The `job` tool brick: the background runner made model-facing, as
//! a native kernel plugin.
//!
//! # One brick, one registration
//!
//! [`JobToolPlugin`] holds the [`JobTool`] value and files it in the
//! `tools` registry during
//! [`apply`](uva_kernel::Plugin::apply). The registration returns a
//! [`RegistrationToken`](uva_tool::RegistrationToken); the effect
//! `apply` returns withdraws exactly that token, so unloading the
//! brick removes its `job` entry and leaves every other tool —
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

use crate::jobs::JobTool;

/// The plugin id the `tool-job` manifest declares.
const JOB_TOOL_ID: &str = "tool-job";

/// Label of the effect that withdraws the `job` tool registration.
const JOB_TOOL_EFFECT: &str = "tool-job";

/// The `job` kernel plugin: registers the `job` tool.
///
/// Mount it with [`uva_kernel::mount_all`] in the same group as (or
/// after) the `tool-registry` plugin. See the [module
/// docs](crate::jobs::plugin) for the unload contract and the refusal
/// rule.
#[derive(Debug)]
pub struct JobToolPlugin {
    manifest: PluginManifest,
    tool: Arc<JobTool>,
}

impl JobToolPlugin {
    /// Build the brick over a default [`JobTool`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_tool(Arc::new(JobTool::new()))
    }

    /// Build the brick over the tool it files.
    ///
    /// A host that keeps its own registry hands the tool over, so the
    /// jobs it started are the jobs the host can see.
    #[must_use]
    pub fn with_tool(tool: Arc<JobTool>) -> Self {
        let mut manifest =
            PluginManifest::new(JOB_TOOL_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.inject.push(KEY_TOOLS.to_string());
        Self { manifest, tool }
    }
}

impl Default for JobToolPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl KernelPlugin for JobToolPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        let tool: Arc<dyn Tool> = self.tool.clone();
        let effect = register_tool_entries(
            ctx,
            vec![ToolEntry::new(tool)],
            ToolProvenance::Core,
            JOB_TOOL_EFFECT,
        )?;
        Ok(vec![effect])
    }
}
