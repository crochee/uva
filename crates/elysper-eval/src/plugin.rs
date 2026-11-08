//! The `eval` service: the host's evaluation runner as a native kernel
//! plugin.
//!
//! # The service is the runner
//!
//! [`EvalPlugin`] publishes the host-built [`EvalRunner`] under
//! [`KEY_EVAL`]. The runner already carries the configured metrics,
//! threshold, and clock; the plugin adds no evaluation logic of its
//! own. A consumer resolves the service and calls
//! [`EvalRunner::run`](crate::runner::EvalRunner::run) (or the
//! `run_trials` variant) with its suite and the agent under test.
//!
//! # The JSON-RPC surface is the app-server's
//!
//! The capability spec talks about an eval *method*. A kernel service
//! is not a JSON-RPC method, and this plugin deliberately claims none:
//! the app-server owns its method table, and a method registered here
//! would be a promise the transport never routes. The wire method is
//! the app-server's read of this service, wired with the rest of the
//! boot assembly.
//!
//! # The service value's shape
//!
//! [`EvalRunner`] is `Sized`, so `apply` calls
//! `ctx.provide_arc(KEY_EVAL, Arc<EvalRunner>)` and a consumer writes
//! `ctx.require::<EvalRunner>(KEY_EVAL)`, which yields an `Arc`
//! pointing at the exact runner the plugin was built with.
//!
//! # Lifecycle
//!
//! The plugin returns its provide handle from
//! [`apply`](elysper_kernel::Plugin::apply), so unloading it revokes
//! the service. The runner arrives with the plugin value, so the
//! manifest injects nothing.

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

use crate::runner::EvalRunner;

/// The service key the `eval` service is registered under.
///
/// The kernel's manifest `provides` list carries the same string, so
/// the key a consumer passes to `Context::require` and the key the
/// kernel validates are one constant.
pub const KEY_EVAL: &str = "eval";

/// The plugin id the `eval` manifest declares.
const EVAL_ID: &str = "eval";

/// The `eval` kernel plugin: provides [`KEY_EVAL`].
///
/// Mount it with [`elysper_kernel::mount_all`]. See the [module
/// docs](crate::plugin) for the service's shape, the method-table
/// boundary, and the unload contract.
pub struct EvalPlugin {
    manifest: PluginManifest,
    /// The runner the service hands out.
    ///
    /// Public because the host keeps the same handle for its own
    /// callers; the service publishes exactly this value.
    pub runner: Arc<EvalRunner>,
}

impl EvalPlugin {
    /// Publish `runner` as the `eval` service.
    ///
    /// The host builds the runner from its configuration; the plugin
    /// only publishes it.
    pub fn new(runner: Arc<EvalRunner>) -> Self {
        let mut manifest =
            PluginManifest::new(EVAL_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.provides.push(KEY_EVAL.to_string());
        Self { manifest, runner }
    }
}

impl KernelPlugin for EvalPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        Ok(vec![ctx.provide_arc(KEY_EVAL, Arc::clone(&self.runner))?])
    }
}
