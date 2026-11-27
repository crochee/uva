//! The `web_search` tool brick: the network search made model-facing,
//! as a native kernel plugin.
//!
//! # One brick, one registration
//!
//! [`WebSearchToolPlugin`] holds the [`WebSearchTool`] value and
//! files it in the `tools` registry during
//! [`apply`](uva_kernel::Plugin::apply). The registration returns a
//! [`RegistrationToken`](uva_tool::RegistrationToken); the effect
//! `apply` returns withdraws exactly that token, so unloading the
//! brick removes its `web_search` entry and leaves every other tool —
//! including a foreign entry filed under the same name — in place.
//!
//! # The registry is injected, not built
//!
//! The manifest injects `tools`, which the `tool-registry` plugin
//! provides ([`ToolRegistryPlugin`](uva_tool::ToolRegistryPlugin)).
//! Mounting this brick without that service is reported as a missing
//! dependency instead of silently mounting a tool nobody can call.
//!
//! # The provider is installed, not invented
//!
//! The brick is built with the [`WebSearchProvider`] the host
//! resolved: the deployment's configured search endpoint, or
//! [`NoSearch`](super::search::NoSearch) — which answers every call
//! with "no web search provider is configured" instead of pretending
//! the web is empty.

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

use crate::web::search::{WebSearchProvider, WebSearchTool};

/// The plugin id the `tool-web-search` manifest declares.
const WEB_SEARCH_TOOL_ID: &str = "tool-web-search";

/// Label of the effect that withdraws the `web_search` tool
/// registration.
const WEB_SEARCH_TOOL_EFFECT: &str = "tool-web-search";

/// The `web_search` kernel plugin: registers the `web_search` tool.
///
/// Mount it with [`uva_kernel::mount_all`] in the same group as (or
/// after) the `tool-registry` plugin. See the [module
/// docs](crate::web::search) for the answer's shape and bounds.
#[derive(Debug)]
pub struct WebSearchToolPlugin {
    manifest: PluginManifest,
    tool: Arc<WebSearchTool>,
}

impl WebSearchToolPlugin {
    /// Build the brick whose `web_search` fails closed: no provider
    /// is attached, so every call answers "no web search provider is
    /// configured".
    #[must_use]
    pub fn new() -> Self {
        Self::with_provider(Arc::new(crate::web::search::NoSearch))
    }

    /// Build the brick whose `web_search` asks `provider`.
    #[must_use]
    pub fn with_provider(provider: Arc<dyn WebSearchProvider>) -> Self {
        let mut manifest =
            PluginManifest::new(WEB_SEARCH_TOOL_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.inject.push(KEY_TOOLS.to_string());
        Self {
            manifest,
            tool: Arc::new(WebSearchTool::new(provider)),
        }
    }
}

impl Default for WebSearchToolPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl KernelPlugin for WebSearchToolPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        let tool: Arc<dyn Tool> = self.tool.clone();
        let effect = register_tool_entries(
            ctx,
            vec![ToolEntry::new(tool)],
            ToolProvenance::Core,
            WEB_SEARCH_TOOL_EFFECT,
        )?;
        Ok(vec![effect])
    }
}
