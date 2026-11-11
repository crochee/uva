//! The crate's error type.
//!
//! Every failure of the channel is classified with the wire's own
//! [`PluginError`], so a caller reports *why* a plugin is unusable
//! without parsing a message: `Unavailable` means the process is gone,
//! `Timeout` means it did not answer in time, `BadRequest` means the two
//! sides disagree about the protocol, `Internal` means the plugin failed
//! on its own terms.
//!
//! A failed install is not fatal to a deployment: plugins are optional
//! capability, so the caller's job is to report the error and carry on.

use std::io;

use thiserror::Error;

use crate::protocol::PluginError;

/// What went wrong hosting a dynamic plugin.
#[derive(Debug, Error)]
pub enum HostError {
    /// The configured command names no program to run.
    #[error("the plugin command is empty")]
    EmptyCommand,

    /// The plugin's process could not be started.
    #[error("cannot start `{program}`: {source}")]
    Spawn {
        /// The program the host tried to run.
        program: String,
        /// Why the spawn failed.
        #[source]
        source: io::Error,
    },

    /// The configured transport is not implemented by this crate.
    #[error("the `{transport}` plugin transport is not implemented; use stdio")]
    UnsupportedTransport {
        /// The transport's wire name.
        transport: &'static str,
    },

    /// The channel reported a failure, classified by the wire's stable
    /// error code.
    #[error("plugin `{plugin}`: {kind:?}: {detail}")]
    Plugin {
        /// The plugin's configured name.
        plugin: String,
        /// The wire classification.
        kind: PluginError,
        /// The human-readable detail.
        detail: String,
    },

    /// Two plugins claim the same tool name.
    ///
    /// The host would append both entries, and the later one would win
    /// the name — with the later plugin's process and the later plugin's
    /// host risk behind it. That is capability substitution, so the
    /// install is refused instead.
    #[error(
        "plugin `{plugin}` claims the tool `{tool}`, which `{owner}` already claimed"
    )]
    DuplicateToolName {
        /// The plugin that was refused.
        plugin: String,
        /// The contested tool name.
        tool: String,
        /// The plugin that claimed it first.
        owner: String,
    },
}

impl HostError {
    /// A `BadRequest` failure: the plugin's answer did not match the
    /// protocol.
    pub(crate) fn bad_request(plugin: &str, detail: impl Into<String>) -> Self {
        Self::Plugin {
            plugin: plugin.to_string(),
            kind: PluginError::BadRequest,
            detail: detail.into(),
        }
    }
}
