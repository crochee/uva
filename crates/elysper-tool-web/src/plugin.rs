//! The `web_fetch` tool brick: the network tool made model-facing, as
//! a native kernel plugin.
//!
//! # One brick, one registration
//!
//! [`WebFetchToolPlugin`] holds the [`WebFetchTool`] value and files it
//! in the `tools` registry during
//! [`apply`](elysper_kernel::Plugin::apply). The registration returns a
//! [`RegistrationToken`](elysper_tool::RegistrationToken); the effect
//! `apply` returns withdraws exactly that token, so unloading the brick
//! removes its `web_fetch` entry and leaves every other tool —
//! including a foreign entry filed under the same name — in place.
//!
//! # The registry is injected, not built
//!
//! The manifest injects `tools`, which the `tool-registry` plugin
//! provides ([`ToolRegistryPlugin`](elysper_tool::ToolRegistryPlugin)).
//! Mounting this brick without that service is reported as a missing
//! dependency instead of silently mounting a tool nobody can call.
//!
//! # A refused registration fails loudly
//!
//! [`register_entry_with_token`](elysper_tool::ToolRegistry::register_entry_with_token)
//! returns `None` when a Core tool already owns the name. `apply` turns
//! that into a [`PluginError`](elysper_kernel::PluginError) naming the
//! tool and the core owner, so the mount rolls back rather than
//! shipping a brick whose tool never appeared.

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

use crate::WebFetchTool;

/// The plugin id the `tool-web` manifest declares.
const WEB_TOOL_ID: &str = "tool-web";

/// Label of the effect that withdraws the `web_fetch` tool
/// registration.
const WEB_TOOL_EFFECT: &str = "tool-web";

/// The `web_fetch` kernel plugin: registers the `web_fetch` tool.
///
/// Mount it with [`elysper_kernel::mount_all`] in the same group as (or
/// after) the `tool-registry` plugin. See the [module
/// docs](crate::plugin) for the unload contract and the refusal rule.
#[derive(Debug)]
pub struct WebFetchToolPlugin {
    manifest: PluginManifest,
    tool: Arc<WebFetchTool>,
}

impl WebFetchToolPlugin {
    /// Build the brick over a default [`WebFetchTool`].
    #[must_use]
    pub fn new() -> Self {
        let mut manifest =
            PluginManifest::new(WEB_TOOL_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.inject.push(KEY_TOOLS.to_string());
        Self {
            manifest,
            tool: Arc::new(WebFetchTool::new()),
        }
    }
}

impl Default for WebFetchToolPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl KernelPlugin for WebFetchToolPlugin {
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
        let unregister = ctx.effect(WEB_TOOL_EFFECT, move || {
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
