//! The `schedule` tool brick: the scheduler service made model-facing,
//! as a native kernel plugin.
//!
//! # Why the brick lives here, not in `elysper-scheduler`
//!
//! The service and the tool sit on opposite sides of a dependency edge:
//! this crate depends on `elysper-scheduler`, so the scheduler crate
//! cannot name [`SchedulerTool`] or the `tools` registry without a
//! cycle. [`SchedulerPlugin`](elysper_scheduler::plugin::SchedulerPlugin)
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
//! `register_entry_with_token` returns the token the registration is
//! filed under; `apply` returns a [`ctx.effect`](elysper_kernel::Context::effect)
//! that calls `unregister_by_token`, so unloading removes only this
//! brick's `schedule` entry — never a same-named entry another
//! registrant owns. A refused registration (a core tool already owns
//! the name) fails `apply` loudly instead of mounting a brick whose
//! tool silently never appeared.

use std::sync::Arc;

use elysper_kernel::{
    Context,
    EffectHandle,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    PluginError,
    PluginManifest,
    RuntimeKind,
};
use elysper_scheduler::{SchedulerService, plugin::KEY_SCHEDULER};
use elysper_tool::{KEY_TOOLS, ToolEntry, ToolRegistry};

use crate::scheduler::{SCHEDULE_TOOL_NAME, SchedulerTool};

/// The plugin id the `schedule-tool` manifest declares.
const SCHEDULE_TOOL_ID: &str = "schedule-tool";

/// Label of the effect that withdraws the `schedule` tool registration.
const SCHEDULE_TOOL_EFFECT: &str = "schedule-tool";

/// The `schedule` kernel plugin: registers the `schedule` tool.
///
/// Mount it with [`elysper_kernel::mount_all`] in the same group as (or
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
        let tools = ctx.require::<ToolRegistry>(KEY_TOOLS)?;
        let tool = SchedulerTool::new(Arc::clone(service.store()));
        let Some(token) =
            tools.register_entry_with_token(ToolEntry::new(Arc::new(tool)))
        else {
            return Err(PluginError::new(format!(
                "the `{KEY_TOOLS}` registry refused to register the \
                 `{SCHEDULE_TOOL_NAME}` tool: a core tool already owns the \
                 name `{SCHEDULE_TOOL_NAME}`"
            )));
        };
        // The closure owns one token clone; `rollback` covers the case
        // where the framework refuses to file the effect at all.
        let rollback = token.clone();
        let registry = Arc::clone(&tools);
        let unregister = ctx.effect(SCHEDULE_TOOL_EFFECT, move || {
            registry.unregister_by_token(token);
            Ok(())
        });
        let effect = match unregister {
            Ok(effect) => effect,
            Err(error) => {
                tools.unregister_by_token(rollback);
                return Err(error);
            }
        };
        Ok(vec![effect])
    }
}
