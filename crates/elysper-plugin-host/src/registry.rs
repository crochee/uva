//! The plugins one host runs, and the tool names each of them claimed.
//!
//! A [`Host`] is per-agent and deliberately has no notion of plugin
//! identity. `Host::tool` refuses a name a *core* tool owns, but two
//! dynamic tools may share one: the registry appends both entries and a
//! lookup returns the last, so installing a second plugin that reuses a
//! name would silently replace the first plugin's tool — with the second
//! plugin's process, its command, and its host risk behind it.
//!
//! In a crate whose premise is untrusted code, that is capability
//! substitution, so this registry remembers which plugin claimed which
//! tool name and refuses the install instead. It is also the record a
//! composition layer needs: what is installed, and who owns what.

use std::{collections::HashMap, fmt};

use elysper_harness::{Host, PluginHandle};

use crate::{RemotePlugin, error::HostError};

/// The plugins installed on one host.
///
/// Dropping the registry drops the handles it owns, which withdraws
/// every registration and kills every plugin process.
#[derive(Default)]
pub struct PluginRegistry {
    installed: Vec<Installed>,
    // tool name → the plugin that claimed it
    claimed: HashMap<String, String>,
}

struct Installed {
    name: String,
    handle: PluginHandle,
    /// Publish frames this plugin's handshake window dropped past its
    /// bounds — the flooding diagnostic [`Self::dropped_early_publishes`]
    /// hands to an installer that can log.
    dropped_early_publishes: u64,
}

impl PluginRegistry {
    /// An empty registry: nothing installed, nothing claimed.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Install one plugin, keeping its registrations and its claims.
    ///
    /// A tool name another plugin already claimed is refused with
    /// [`HostError::DuplicateToolName`], and the check happens before
    /// anything is registered, so a rejected plugin leaves both the host
    /// and the registry untouched.
    ///
    /// Returns the tool names the plugin claimed, so the caller can
    /// check what the *registry* made of them: a name a core tool owns
    /// is refused there, not here, and the caller is the layer that
    /// reports it — see [`Self::withdraw`].
    ///
    /// # Errors
    ///
    /// Everything [`RemotePlugin::install`] reports, plus the duplicate
    /// name. The caller reports it and carries on: a plugin is optional
    /// capability.
    pub async fn install(
        &mut self,
        plugin: &RemotePlugin,
        host: &mut Host,
    ) -> Result<Vec<String>, HostError> {
        let (handle, tools, dropped_early_publishes) =
            plugin.install_claimed(host, &mut self.claimed).await?;
        self.installed.push(Installed {
            name: plugin.name().to_string(),
            handle,
            dropped_early_publishes,
        });
        Ok(tools)
    }

    /// Withdraw a plugin's install: drop its handle — which withdraws
    /// every registration it made, including the ones the caller found
    /// unusable — and free the tool names it claimed.
    ///
    /// This is what makes an install all-or-nothing. A plugin whose
    /// second tool the host refused must not stay installed for its
    /// first, and its claims must not hold the names against the next
    /// plugin.
    ///
    /// Returns whether the plugin was installed.
    pub fn withdraw(&mut self, plugin: &str) -> bool {
        let Some(position) = self
            .installed
            .iter()
            .position(|installed| installed.name == plugin)
        else {
            return false;
        };
        self.installed.remove(position);
        self.claimed.retain(|_, owner| owner.as_str() != plugin);
        true
    }

    /// The installed plugins' names, in install order.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.installed
            .iter()
            .map(|installed| installed.name.as_str())
            .collect()
    }

    /// How many `elysper/publish` frames this plugin's handshake
    /// window dropped past its bounds.
    ///
    /// The install path's one-shot flooding diagnostic: a plugin that
    /// floods the window between its `initialize` answer and the
    /// settle keeps the first frames and loses the rest, and this is
    /// the count of the loss — `None` when no plugin of that name is
    /// installed, `Some(0)` for one that did not flood.
    #[must_use]
    pub fn dropped_early_publishes(&self, plugin: &str) -> Option<u64> {
        self.installed
            .iter()
            .find(|installed| installed.name == plugin)
            .map(|installed| installed.dropped_early_publishes)
    }

    /// The handles that own the installed registrations, in install
    /// order.
    #[must_use]
    pub fn handles(&self) -> Vec<&PluginHandle> {
        self.installed
            .iter()
            .map(|installed| &installed.handle)
            .collect()
    }

    /// Which plugin claimed `tool`, if any.
    #[must_use]
    pub fn owner(&self, tool: &str) -> Option<&str> {
        self.claimed.get(tool).map(String::as_str)
    }
}

impl fmt::Debug for PluginRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PluginRegistry")
            .field("plugins", &self.names())
            .field("tools", &self.claimed.keys().collect::<Vec<_>>())
            .finish()
    }
}
