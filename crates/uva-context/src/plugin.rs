//! The `memory` kernel plugin.
//!
//! Publishes the deployment's long-term memory under [`KEY_MEMORY`] so
//! a consumer resolves it from the mounted spine instead of receiving a
//! boot-built `Arc`. The file tier is the default; a configuration the
//! file tier refuses degrades to the in-memory tier rather than failing
//! the mount, the same contract the boot had.

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

use crate::memory::{FileMemory, FileMemoryConfig, InMemoryMemory, Memory};

/// The service key the `memory` service is registered under.
pub const KEY_MEMORY: &str = "memory";

// The plugin id the `memory` manifest declares.
const MEMORY_ID: &str = "memory";

/// The `memory` kernel plugin: provides [`KEY_MEMORY`].
pub struct MemoryPlugin {
    manifest: PluginManifest,
    config: FileMemoryConfig,
}

impl MemoryPlugin {
    #[must_use]
    /// Build the plugin over the file tier's configuration.
    pub fn new(config: FileMemoryConfig) -> Self {
        let mut manifest =
            PluginManifest::new(MEMORY_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.provides.push(KEY_MEMORY.to_string());
        Self { manifest, config }
    }
}

impl KernelPlugin for MemoryPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        let memory: Arc<dyn Memory> = match FileMemory::new(self.config.clone())
        {
            Ok(memory) => Arc::new(memory),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "failed to open file memory; using the in-memory tier"
                );
                Arc::new(InMemoryMemory::new())
            }
        };
        let handle = ctx.provide(KEY_MEMORY, memory)?;
        Ok(vec![handle])
    }
}
