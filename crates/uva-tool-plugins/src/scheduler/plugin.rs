//! The `schedule` tool brick: the scheduler service made model-facing,
//! as a native kernel plugin.
//!
//! # Why the brick lives here, not in `uva-scheduler`
//!
//! The service and the tool sit on opposite sides of a dependency edge:
//! this crate depends on `uva-scheduler`, so the scheduler crate
//! cannot name [`SchedulerTool`] or the `tools` registry without a
//! cycle. [`SchedulerPlugin`](uva_scheduler::plugin::SchedulerPlugin)
//! therefore publishes the `scheduler` service, and this brick consumes
//! it: `apply` resolves `scheduler` and `tools`, builds the tool over
//! the service's store, and files it under its own registration token.
//!
//! # One store, two writers
//!
//! [`SchedulerTool::new`] preloads the store it is handed and every
//! action writes through it, so the tool's `create` / `pause` /
//! `resume` / `remove` and the host's `Scheduler::tick` see one
//! schedule. The tool never fires a job: delivery stays with the tick
//! loop the host arms.
//!
//! # Unload removes exactly the brick's entry
//!
//! The lifecycle lives in
//! [`register_tool_entries`](uva_tool::register_tool_entries): the
//! registration is filed under a token and the effect `apply` returns
//! calls `unregister_by_token`, so unloading removes only this brick's
//! `schedule` entry — never a same-named entry another registrant owns.
//! A refused registration (a core tool already owns the name) fails
//! `apply` loudly instead of mounting a brick whose tool silently never
//! appeared.

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
use uva_scheduler::{SchedulerService, plugin::KEY_SCHEDULER};
use uva_tool::{KEY_TOOLS, ToolEntry, ToolProvenance, register_tool_entries};

use crate::scheduler::SchedulerTool;

/// The plugin id the `schedule-tool` manifest declares.
const SCHEDULE_TOOL_ID: &str = "schedule-tool";

/// Label of the effect that withdraws the `schedule` tool registration.
const SCHEDULE_TOOL_EFFECT: &str = "schedule-tool";

/// The `schedule` kernel plugin: registers the `schedule` tool.
///
/// Mount it with [`uva_kernel::mount_all`] in the same group as (or
/// after) the `tool-registry` and `scheduler` plugins. See the [module
/// docs](crate::scheduler::plugin) for the dependency direction and the unload
/// contract.
#[derive(Debug)]
pub struct ScheduleToolPlugin {
    manifest: PluginManifest,
}

impl ScheduleToolPlugin {
    /// Build the brick. The tool itself is constructed in
    /// [`apply`](KernelPlugin::apply), once the scheduler service is
    /// resolvable.
    #[must_use]
    pub fn new() -> Self {
        let mut manifest =
            PluginManifest::new(SCHEDULE_TOOL_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.inject.push(KEY_SCHEDULER.to_string());
        manifest.inject.push(KEY_TOOLS.to_string());
        Self { manifest }
    }
}

impl Default for ScheduleToolPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl KernelPlugin for ScheduleToolPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        let service = ctx.require::<SchedulerService>(KEY_SCHEDULER)?;
        let tool = SchedulerTool::new(Arc::clone(service.store()));
        let effect = register_tool_entries(
            ctx,
            vec![ToolEntry::new(Arc::new(tool))],
            ToolProvenance::Dynamic,
            SCHEDULE_TOOL_EFFECT,
        )?;
        Ok(vec![effect])
    }
}
