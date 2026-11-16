//! The kernel's error vocabulary: plugin startup failures and mounting
//! validation failures.

use thiserror::Error;

/// A plugin's own startup failure.
///
/// Plugins return this from [`Plugin::apply`](crate::Plugin::apply);
/// `mount_all` wraps it in
/// [`MountError::ApplyFailed`] so the
/// error names both the plugin and the reason.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PluginError {
    /// A free-form failure reported by the plugin itself.
    #[error("{message}")]
    Failed {
        /// What went wrong.
        message: String,
    },
    /// A service registration the plugin attempted failed.
    #[error("service `{key}` could not be registered: {message}")]
    Service {
        /// The service key involved.
        key: String,
        /// The underlying framework message.
        message: String,
    },
}

impl PluginError {
    /// Construct a free-form startup failure.
    pub fn new(message: impl Into<String>) -> Self {
        Self::Failed {
            message: message.into(),
        }
    }
}

impl From<cordis::CordisError> for PluginError {
    fn from(error: cordis::CordisError) -> Self {
        Self::Failed {
            message: error.to_string(),
        }
    }
}

/// A waterfall chain failed.
///
/// Middleware and inner callbacks share this error type, so a failure
/// anywhere in the chain surfaces with one stable message shape.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WaterfallError {
    /// A middleware or the innermost callback reported a failure.
    #[error("{message}")]
    Failed {
        /// What went wrong.
        message: String,
    },
}

impl WaterfallError {
    /// Construct a free-form chain failure.
    pub fn new(message: impl Into<String>) -> Self {
        Self::Failed {
            message: message.into(),
        }
    }
}

impl From<PluginError> for WaterfallError {
    fn from(error: PluginError) -> Self {
        Self::new(error.to_string())
    }
}

/// A group of plugins could not be mounted.
///
/// Every variant is returned before a partial group is left behind:
/// validation errors fire before any plugin starts, and
/// [`ApplyFailed`](Self::ApplyFailed) / [`PluginInactive`](Self::PluginInactive)
/// roll back every fiber mounted so far.
#[derive(Debug, Error)]
pub enum MountError {
    /// A plugin manifest had an empty (or whitespace-only) id.
    #[error("plugin at index {index} has an empty id")]
    EmptyPluginId {
        /// Position in the group passed to `mount_all`.
        index: usize,
    },

    /// Two plugins in one group used the same id.
    #[error("duplicate plugin id `{id}`")]
    DuplicatePluginId {
        /// The duplicated id.
        id: String,
    },

    /// One manifest lists the same service key twice in `provides`.
    #[error("plugin `{plugin_id}` lists service `{key}` twice in `provides`")]
    DuplicateDeclaration {
        /// The plugin whose manifest repeats the key.
        plugin_id: String,
        /// The repeated service key.
        key: String,
    },

    /// Two plugins in one group declared the same `provides` key.
    #[error("plugins `{first}` and `{second}` both provide service `{key}`")]
    DuplicateProvider {
        /// The duplicated service key.
        key: String,
        /// The first plugin that declared it.
        first: String,
        /// The second plugin that declared it.
        second: String,
    },

    /// A plugin's protocol version is incompatible with the host's.
    ///
    /// Compatibility is decided by the major component: host `1.0`
    /// accepts `1.2` and rejects `2.0`. A version without a parseable
    /// major is also rejected.
    #[error(
        "plugin `{plugin_id}` targets plugin protocol `{plugin_version}`, \
         incompatible with host protocol `{host_version}`"
    )]
    ProtocolIncompatible {
        /// The plugin that declared the version.
        plugin_id: String,
        /// The version from its manifest.
        plugin_version: String,
        /// The host's version ([`PLUGIN_PROTOCOL_VERSION`](crate::PLUGIN_PROTOCOL_VERSION)).
        host_version: String,
    },

    /// The group's `inject` edges form a cycle.
    #[error("dependency cycle among plugins: {plugins:?}")]
    Cycle {
        /// The ids still unresolved when the sort stalled.
        plugins: Vec<String>,
    },

    /// A plugin's `apply` returned an error.
    #[error("plugin `{plugin_id}` failed to start: {source}")]
    ApplyFailed {
        /// The plugin whose `apply` failed.
        plugin_id: String,
        /// The plugin's own error.
        #[source]
        source: PluginError,
    },

    /// A plugin's fiber settled in a state other than `Active` or
    /// `Pending` (the parked state a missing dependency produces)
    /// without `apply` reporting an error.
    #[error("plugin `{plugin_id}` did not become active (state: {state})")]
    PluginInactive {
        /// The plugin that stayed inactive.
        plugin_id: String,
        /// The settled fiber state, for diagnosis.
        state: String,
    },
}
