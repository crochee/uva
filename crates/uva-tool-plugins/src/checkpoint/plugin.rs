//! The checkpoint bricks: the explore-then-cut-back pair made
//! model-facing, as a native kernel plugin.
//!
//! # One brick, two registrations
//!
//! [`CheckpointToolPlugin`] holds the [`CheckpointTool`] and the
//! [`RewindTool`] values and files both in the `tools` registry during
//! [`apply`](uva_kernel::Plugin::apply). The registrations return one
//! [`RegistrationToken`](uva_tool::RegistrationToken) each; the effect
//! `apply` returns withdraws exactly those tokens, so unloading the
//! brick removes its `checkpoint` and `rewind` entries and leaves
//! every other tool in place.
//!
//! The pair is one brick because it is one capability: a checkpoint
//! nothing can rewind is a promise the deployment cannot keep, and a
//! rewind with no checkpoint to cut back to is an error. Mounting one
//! without the other is not a composition anyone wants.
//!
//! # The store is installed, not invented
//!
//! The brick is built with the [`RewindStore`] the host resolved —
//! the default is [`NoRewinds`](super::NoRewinds), which answers "no
//! checkpoint" and refuses every rewind, so a deployment that mounts
//! the brick without a store fails closed on every call instead of
//! marking boundaries nothing will honour.

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

use super::{CheckpointTool, NoRewinds, RewindStore, RewindTool};

/// The plugin id the `tool-checkpoint` manifest declares.
const CHECKPOINT_TOOL_ID: &str = "tool-checkpoint";

/// Label of the effect that withdraws the pair's registrations.
const CHECKPOINT_TOOL_EFFECT: &str = "tool-checkpoint";

/// The checkpoint kernel plugin: registers `checkpoint` and `rewind`.
///
/// Mount it with [`uva_kernel::mount_all`] in the same group as (or
/// after) the `tool-registry` plugin. See the [module
/// docs](crate::checkpoint) for the pair's contract.
#[derive(Debug)]
pub struct CheckpointToolPlugin {
    manifest: PluginManifest,
    checkpoint: Arc<CheckpointTool>,
    rewind: Arc<RewindTool>,
}

impl CheckpointToolPlugin {
    /// Build the brick whose pair fails closed: no store is attached,
    /// so every `checkpoint` is refused and every `rewind` says there
    /// is no boundary.
    #[must_use]
    pub fn new() -> Self {
        Self::with_store(Arc::new(NoRewinds))
    }

    /// Build the brick whose pair goes through `store`.
    #[must_use]
    pub fn with_store(store: Arc<dyn RewindStore>) -> Self {
        let mut manifest =
            PluginManifest::new(CHECKPOINT_TOOL_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.inject.push(KEY_TOOLS.to_string());
        Self {
            manifest,
            checkpoint: Arc::new(CheckpointTool::new(Arc::clone(&store))),
            rewind: Arc::new(RewindTool::new(store)),
        }
    }
}

impl Default for CheckpointToolPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl KernelPlugin for CheckpointToolPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        let checkpoint: Arc<dyn Tool> = self.checkpoint.clone();
        let rewind: Arc<dyn Tool> = self.rewind.clone();
        let effect = register_tool_entries(
            ctx,
            vec![ToolEntry::new(checkpoint), ToolEntry::new(rewind)],
            ToolProvenance::Core,
            CHECKPOINT_TOOL_EFFECT,
        )?;
        Ok(vec![effect])
    }
}
