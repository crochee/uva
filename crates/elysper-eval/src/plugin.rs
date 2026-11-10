//! The `eval` service: the host's evaluation runner as a native kernel
//! plugin.
//!
//! # The service is the runner
//!
//! [`EvalPlugin`] constructs an [`EvalRunner`] from
//! [`EvalPluginConfig`] inside
//! [`apply`](elysper_kernel::Plugin::apply) and publishes it under
//! [`KEY_EVAL`]. A consumer resolves the service and calls
//! [`EvalRunner::run`](crate::runner::EvalRunner::run) (or the
//! `run_trials` variant) with its suite and the agent under test.
//!
//! The default configuration matches the boot's historical runner:
//! one [`KeywordMetric`] wrapped in [`SyncMetricAdapter`], the default
//! threshold, and the system clock. An explicit clock pins the
//! report's `generated_at`, which is the seam tests use for a
//! reproducible timestamp.
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
//! pointing at the runner the plugin constructed.
//!
//! # Lifecycle
//!
//! The plugin returns its provide handle from
//! [`apply`](elysper_kernel::Plugin::apply), so unloading it revokes
//! the service. The runner is built by `apply` from the plugin's own
//! configuration, so the manifest injects nothing.

use std::sync::Arc;

use elysper_core::SharedClock;
use elysper_kernel::{
    Context,
    EffectHandle,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    PluginError,
    PluginManifest,
    RuntimeKind,
};

use crate::{
    KeywordMetric,
    runner::{EvalRunner, SyncMetricAdapter},
};

/// The service key the `eval` service is registered under.
///
/// The kernel's manifest `provides` list carries the same string, so
/// the key a consumer passes to `Context::require` and the key the
/// kernel validates are one constant.
pub const KEY_EVAL: &str = "eval";

// The plugin id the `eval` manifest declares.
const EVAL_ID: &str = "eval";

/// The configuration [`EvalPlugin::apply`] builds its runner from.
///
/// The default reproduces the boot's historical runner: keyword
/// matching enabled and the system clock.
#[derive(Clone)]
pub struct EvalPluginConfig {
    /// Score suites with [`KeywordMetric`] (the default).
    ///
    /// `false` leaves the runner without metrics, which falls back to
    /// the same keyword match at scoring time; the flag exists so a
    /// profile can state the intent and so the metric list is exactly
    /// what the configuration asked for.
    pub keyword_metric: bool,
    /// The clock the report's `generated_at` reads from.
    ///
    /// `None` keeps the runner's system clock.
    pub clock: Option<SharedClock>,
}

impl Default for EvalPluginConfig {
    fn default() -> Self {
        Self {
            keyword_metric: true,
            clock: None,
        }
    }
}

/// The `eval` kernel plugin: provides [`KEY_EVAL`].
///
/// Mount it with [`elysper_kernel::mount_all`]. See the [module
/// docs](crate::plugin) for the service's shape, the method-table
/// boundary, and the unload contract.
pub struct EvalPlugin {
    manifest: PluginManifest,
    config: EvalPluginConfig,
}

impl EvalPlugin {
    #[must_use]
    /// Build the plugin over `config`; `apply` constructs the runner.
    pub fn new(config: EvalPluginConfig) -> Self {
        let mut manifest =
            PluginManifest::new(EVAL_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.provides.push(KEY_EVAL.to_string());
        Self { manifest, config }
    }
}

impl KernelPlugin for EvalPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        let mut runner = EvalRunner::new();
        if self.config.keyword_metric {
            runner = runner.metric(Box::new(SyncMetricAdapter(KeywordMetric)));
        }
        if let Some(clock) = self.config.clock.as_ref() {
            runner = runner.with_clock(clock.clone());
        }
        let handle = ctx.provide_arc(KEY_EVAL, Arc::new(runner))?;
        Ok(vec![handle])
    }
}
