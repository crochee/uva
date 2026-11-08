//! The shell crate's native kernel plugins: the `sandbox` service and
//! the `shell` tool brick.
//!
//! [`SandboxPlugin`] publishes the node's confinement backend and
//! execution policy under `sandbox`; [`ShellToolPlugin`] files the
//! `shell` tool in the `tools` registry. The two are independent
//! bricks: a deployment can mount the backend without exposing the
//! tool, or the tool without the service (the tool then runs under its
//! own policy value).
//!
//! # The service is a pair, not a backend
//!
//! [`SandboxPlugin`] publishes a [`SandboxService`] under
//! [`KEY_SANDBOX`]: the [`SandboxBackend`] that would enforce a policy
//! and the [`ExecutionPolicy`] a run executes under. One value cannot
//! answer both questions — a backend alone does not say how wide the
//! boundary is, and a policy alone cannot say whether this host can
//! enforce it — so the service carries the pair the shell brick
//! already holds together.
//!
//! # Fail-closed mounting
//!
//! A confined policy is only mountable when its backend reports a
//! usable runner ([`BackendAvailability::Available`]). `apply` refuses
//! otherwise and the kernel rolls the mount back, so a host that lost
//! its runner fails loudly instead of publishing a service that would
//! run commands unconfined. There is no unconfined fallback: the only
//! policy that needs no backend is
//! [`ExecutionPolicy::DangerFullAccess`], and a caller asks for it
//! explicitly.
//!
//! # The service value's shape
//!
//! [`SandboxService`] is `Sized`, so `apply` calls
//! `ctx.provide_arc(KEY_SANDBOX, Arc<SandboxService>)` and a consumer
//! writes `ctx.require::<SandboxService>(KEY_SANDBOX)`, which yields an
//! `Arc` pointing at the pair the plugin was built with.
//!
//! # Lifecycle
//!
//! The plugin returns its provide handle from
//! [`apply`](elysper_kernel::Plugin::apply), so unloading it revokes
//! the service. A consumer that needed confinement then has no policy
//! to run under; whether it refuses the call or falls back to a wider
//! policy is the consumer's decision, never a silent downgrade this
//! plugin performs. The backend and the policy arrive with the plugin
//! value, so the manifest injects nothing.

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
use elysper_tool::{KEY_TOOLS, Tool, ToolEntry, ToolRegistry};

use crate::{
    ShellTool,
    sandbox::{BackendAvailability, ExecutionPolicy, SandboxBackend},
};

/// The service key the `sandbox` service is registered under.
///
/// The kernel's manifest `provides` list carries the same string, so
/// the key a consumer passes to `Context::require` and the key the
/// kernel validates are one constant.
pub const KEY_SANDBOX: &str = "sandbox";

/// The plugin id the `sandbox` manifest declares.
const SANDBOX_ID: &str = "sandbox";

/// The plugin id the `tool-shell` manifest declares.
const SHELL_TOOL_ID: &str = "tool-shell";

/// Label of the effect that withdraws the `shell` tool registration.
const SHELL_TOOL_EFFECT: &str = "tool-shell";

/// The confinement pair one deployment runs under.
///
/// The backend enforces [`policy`](Self::policy); a consumer that needs
/// argv construction asks the backend directly
/// ([`SandboxBackend::confine`]) or goes through the shell tool, which
/// holds the same pair.
#[derive(Debug)]
pub struct SandboxService {
    /// The backend that enforces [`Self::policy`].
    pub backend: Arc<dyn SandboxBackend>,
    /// The file-effect boundary a run executes under.
    pub policy: ExecutionPolicy,
}

impl SandboxService {
    /// Pair one backend with the policy it is expected to enforce.
    #[must_use]
    pub fn new(
        backend: Arc<dyn SandboxBackend>,
        policy: ExecutionPolicy,
    ) -> Self {
        Self { backend, policy }
    }

    /// Whether this pair can actually guard its policy on this host.
    ///
    /// An unconfined policy needs nothing from the backend; a confined
    /// one needs a backend whose runner is usable.
    #[must_use]
    pub fn is_enforceable(&self) -> bool {
        !self.policy.is_confined() || self.backend.availability().is_available()
    }
}

/// The `sandbox` kernel plugin: provides [`KEY_SANDBOX`].
///
/// Mount it with [`elysper_kernel::mount_all`]; a confined policy
/// without a usable backend fails the mount. See the [module
/// docs](crate::plugin) for the service's shape and the unload
/// contract.
pub struct SandboxPlugin {
    manifest: PluginManifest,
    /// The confinement seam every consumer of the service receives.
    pub backend: Arc<dyn SandboxBackend>,
    /// The boundary a run executes under.
    pub policy: ExecutionPolicy,
}

impl SandboxPlugin {
    /// Publish `backend` / `policy` as the `sandbox` service.
    ///
    /// The host builds the backend (from its profile) and picks the
    /// deployment default; the plugin only validates and publishes the
    /// pair.
    pub fn new(
        backend: Arc<dyn SandboxBackend>,
        policy: ExecutionPolicy,
    ) -> Self {
        let mut manifest =
            PluginManifest::new(SANDBOX_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.provides.push(KEY_SANDBOX.to_string());
        Self {
            manifest,
            backend,
            policy,
        }
    }
}

impl KernelPlugin for SandboxPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        let service =
            SandboxService::new(Arc::clone(&self.backend), self.policy);
        if !service.is_enforceable() {
            return Err(PluginError::new(format!(
                "execution policy \"{}\" needs a sandbox but backend \
                 \"{}\" is unavailable ({}); refusing to publish \
                 `{KEY_SANDBOX}` and run commands unconfined",
                self.policy.as_str(),
                self.backend.name(),
                availability_detail(&self.backend.availability()),
            )));
        }
        Ok(vec![ctx.provide_arc(KEY_SANDBOX, Arc::new(service))?])
    }
}

/// The probe's own words, so the mount error names why confinement
/// failed rather than only that it did.
fn availability_detail(availability: &BackendAvailability) -> String {
    match availability {
        BackendAvailability::Available => "available".to_string(),
        BackendAvailability::Unavailable { reason } => reason.clone(),
    }
}

/// The `shell` kernel plugin: registers the `shell` tool.
///
/// Mount it with [`elysper_kernel::mount_all`] in the same group as (or
/// after) the `tool-registry` plugin. The registration is filed under
/// its own token, so unloading the brick withdraws exactly its `shell`
/// entry; a Core owner of the name fails `apply` loudly.
#[derive(Debug)]
pub struct ShellToolPlugin {
    manifest: PluginManifest,
    tool: Arc<ShellTool>,
}

impl ShellToolPlugin {
    /// Build the brick over a default [`ShellTool`].
    ///
    /// The tool keeps its own execution-policy pair; mounting the
    /// `sandbox` service does not rewire it.
    #[must_use]
    pub fn new() -> Self {
        let mut manifest =
            PluginManifest::new(SHELL_TOOL_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.inject.push(KEY_TOOLS.to_string());
        Self {
            manifest,
            tool: Arc::new(ShellTool::new()),
        }
    }
}

impl Default for ShellToolPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl KernelPlugin for ShellToolPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        let tools = ctx.require::<ToolRegistry>(KEY_TOOLS)?;
        let name = self.tool.name();
        let entry: Arc<dyn Tool> = self.tool.clone();
        let Some(token) =
            tools.register_entry_with_token(ToolEntry::new(entry))
        else {
            return Err(PluginError::new(format!(
                "the `{KEY_TOOLS}` registry refused to register the \
                 `{name}` tool: a core tool already owns the name `{name}`"
            )));
        };
        // The closure owns one token clone; `rollback` covers the case
        // where the framework refuses to file the effect at all.
        let rollback = token.clone();
        let registry = Arc::clone(&tools);
        let unregister = ctx.effect(SHELL_TOOL_EFFECT, move || {
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
