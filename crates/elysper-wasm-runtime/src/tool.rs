//! The two kernel seams a Wasm tool plugs into: the `tools` service and
//! the `sandbox` service.
//!
//! # [`WasmToolPlugin`]
//!
//! A kernel [`Plugin`](KernelPlugin) that registers one [`Tool`]. Its
//! manifest injects `tools`, so its `apply` runs only when the registry
//! is available. `apply` mounts the component (one `describe()` call),
//! registers the resulting [`WasmComponent`](crate::WasmComponent) under its own registration
//! token, and returns the effect handle that withdraws exactly that
//! entry — unloading the plugin removes the tool and drops the pooled
//! store/instance.
//!
//! The mounting cost is paid in `apply`, not on the first model call: a
//! component that fails to compile, imports an ungranted capability, or
//! returns an unusable schema fails the mount.
//!
//! # [`WasmSandbox`]
//!
//! The `sandbox` service's backend trait is about **OS process argv**
//! (`SandboxBackend::confine` wraps a `sh -c` payload). A Wasm instance
//! cannot confine a child process, and claiming it can would be a silent
//! boundary loss, so [`WasmSandbox`] reports `Unavailable` for confined
//! policies and passes `DangerFullAccess` through. Its confinement is the
//! capability sandbox the runtime enforces inside the component; a host
//! that also runs processes mounts an OS backend (`bwrap`) alongside it.

use std::{path::Path, sync::Arc};

use elysper_kernel::{
    Context,
    EffectHandle,
    Plugin as KernelPlugin,
    PluginError,
    PluginManifest,
    RuntimeKind,
};
use elysper_tool::{Tool, ToolEntry, ToolRegistry, plugin::KEY_TOOLS};
use elysper_tool_shell::{
    BackendAvailability,
    ConfinedCommand,
    ExecutionPolicy,
    SandboxBackend,
    SandboxError,
};

use crate::{WasmError, WasmErrorKind, WasmRuntime};

/// Label of the effect that withdraws the tool registration.
const WASM_TOOL_EFFECT: &str = "wasm-tool";

/// A Wasm component registered as one kernel tool.
///
/// Build it with [`WasmToolPlugin::new`], then mount it with
/// [`elysper_kernel::mount_all`] in the same group as (or after) the
/// `tool-registry` plugin.
pub struct WasmToolPlugin {
    manifest: PluginManifest,
    runtime: Arc<WasmRuntime>,
    wasm: Arc<[u8]>,
}

impl std::fmt::Debug for WasmToolPlugin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WasmToolPlugin")
            .field("manifest", &self.manifest)
            .field("bytes", &self.wasm.len())
            .finish_non_exhaustive()
    }
}

impl WasmToolPlugin {
    /// Build the plugin over `runtime` and the component bytes.
    ///
    /// The manifest must select [`RuntimeKind::Wasm`]; `tools` is added
    /// to its `inject` list (when absent) because the plugin cannot
    /// register anything without the registry. The bytes are compiled
    /// and validated in [`apply`](KernelPlugin::apply), not here.
    ///
    /// # Errors
    ///
    /// [`WasmErrorKind::InvalidManifest`] when the manifest does not
    /// select the Wasm runtime.
    pub fn new(
        runtime: Arc<WasmRuntime>,
        wasm: impl Into<Vec<u8>>,
        manifest: PluginManifest,
    ) -> Result<Self, WasmError> {
        if manifest.runtime != RuntimeKind::Wasm {
            return Err(WasmError::new(
                WasmErrorKind::InvalidManifest,
                format!(
                    "plugin `{}` must declare `runtime = Wasm`, not {:?}",
                    manifest.id, manifest.runtime
                ),
            ));
        }
        let mut manifest = manifest;
        if !manifest.inject.iter().any(|key| key == KEY_TOOLS) {
            manifest.inject.push(KEY_TOOLS.to_string());
        }
        Ok(Self {
            manifest,
            runtime,
            wasm: Arc::from(wasm.into().into_boxed_slice()),
        })
    }
}

impl KernelPlugin for WasmToolPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        let tools = ctx.require::<ToolRegistry>(KEY_TOOLS)?;
        let component = self
            .runtime
            .mount(&self.wasm, &self.manifest.capabilities)
            .map_err(|error| {
                PluginError::new(format!(
                    "plugin `{}` failed to mount its WebAssembly component ({}): {error}",
                    self.manifest.id,
                    error.code()
                ))
            })?;
        let name = component.name().to_string();
        let Some(token) = tools
            .register_entry_with_token(ToolEntry::new(Arc::new(component)))
        else {
            return Err(PluginError::new(format!(
                "the `{KEY_TOOLS}` registry refused to register `{name}`: \
                 a core tool already owns that name"
            )));
        };
        // The closure owns one token clone; `rollback` covers the case
        // where the framework refuses to file the effect at all.
        let rollback = token.clone();
        let registry = Arc::clone(&tools);
        let unregister = ctx.effect(WASM_TOOL_EFFECT, move || {
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

/// The Wasm runtime exposed as a [`SandboxBackend`].
///
/// It answers the trait's two questions honestly: it cannot wrap an OS
/// process (so confined policies are refused and `availability` says
/// unavailable), and it has no denial dialect of its own (the capability
/// sandbox denies *imports at instantiation*, not file effects at run
/// time). Mount it when a host wants the `sandbox` service to name the
/// Wasm boundary; pair it with an OS backend for process confinement.
#[derive(Debug)]
pub struct WasmSandbox {
    runtime: Arc<WasmRuntime>,
    name: String,
}

impl WasmSandbox {
    /// Wrap `runtime` as a sandbox backend named `wasm`.
    #[must_use]
    pub fn new(runtime: Arc<WasmRuntime>) -> Self {
        Self {
            runtime,
            name: "wasm".to_string(),
        }
    }

    /// The runtime whose capability boundary this backend names.
    #[must_use]
    pub fn runtime(&self) -> &Arc<WasmRuntime> {
        &self.runtime
    }
}

impl SandboxBackend for WasmSandbox {
    fn name(&self) -> &str {
        &self.name
    }

    fn availability(&self) -> BackendAvailability {
        BackendAvailability::Unavailable {
            reason: "a Wasm component cannot confine an OS process; \
                     the capability sandbox constrains guest imports, not \
                     a child command's file effects. Mount an OS backend \
                     (bwrap) for process confinement."
                .to_string(),
        }
    }

    fn confine(
        &self,
        policy: ExecutionPolicy,
        _workspace_root: &Path,
    ) -> Result<ConfinedCommand, SandboxError> {
        if policy.is_confined() {
            return Err(SandboxError::unavailable(
                policy,
                "the Wasm sandbox confines guest capabilities, not OS process argv",
            ));
        }
        // `DangerFullAccess` needs no confinement: the same explicit
        // bypass an OS backend's noop path takes.
        Ok(ConfinedCommand::unconfined())
    }

    fn denial_signatures(&self) -> &'static [&'static str] {
        &[]
    }
}
