//! Plugin manifests: the declared identity, runtime, services, and
//! capabilities of one plugin.

use std::fmt::{self, Display, Formatter};

use serde::{Deserialize, Serialize};

/// The plugin protocol version this kernel speaks.
///
/// A plugin's [`PluginManifest::version`] must have the same major
/// component as this constant; [`mount_all`](crate::mount_all) rejects a
/// different major with
/// [`MountError::ProtocolIncompatible`](crate::MountError::ProtocolIncompatible).
pub const PLUGIN_PROTOCOL_VERSION: &str = "1.0";

/// A service key: the name a manifest lists in `provides` / `inject` and
/// the name plugins pass to `Context::provide*` / `Context::require`.
///
/// A plain `String` today. The alias exists so the kernel can grow a
/// stronger key type later without rewriting every manifest and plugin
/// signature.
pub type ServiceKey = String;

/// One host capability a plugin may use.
///
/// A newtype rather than a bare `String` so namespaced capabilities
/// such as `net:http` survive the round trip, and so signatures can say
/// "capability" instead of "string".
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Capability(
    /// The wire name, e.g. `net:http` or `fs:read`.
    pub String,
);

impl Capability {
    /// Construct a capability from its wire name.
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// Borrow the capability name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for Capability {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl From<&str> for Capability {
    fn from(name: &str) -> Self {
        Self(name.to_owned())
    }
}

impl From<String> for Capability {
    fn from(name: String) -> Self {
        Self(name)
    }
}

/// Where a plugin executes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeKind {
    /// Compiled into the host binary.
    Native,
    /// Loaded by the host's Wasm runtime.
    Wasm,
    /// Loaded by the web frontend, never by the core loader.
    UiBundle,
}

/// The calling convention of a [`RuntimeKind::Wasm`] plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WasmAbi {
    /// The WebAssembly Interface Types component model.
    Wit,
    /// A JSON-in / JSON-out calling convention.
    Json,
}

/// Everything one plugin declares about itself.
///
/// The manifest is the kernel's whole view of a plugin before its
/// [`apply`](crate::Plugin::apply) runs: `provides` and `inject` drive
/// the mounting order, `version` gates protocol compatibility, and the
/// remaining fields describe how a loader should construct and confine
/// the plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginManifest {
    /// Stable plugin identity, unique within one profile and one mount
    /// group.
    pub id: String,
    /// The plugin protocol version this plugin targets, e.g. `1.0`.
    pub version: String,
    /// Where the plugin executes.
    pub runtime: RuntimeKind,
    /// Service keys this plugin registers while it is active.
    #[serde(default)]
    pub provides: Vec<ServiceKey>,
    /// Service keys that must be available before
    /// [`apply`](crate::Plugin::apply) runs.
    #[serde(default)]
    pub inject: Vec<ServiceKey>,
    /// Host capabilities this plugin may use.
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    /// The calling convention, for [`RuntimeKind::Wasm`] plugins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abi: Option<WasmAbi>,
    /// JSON schema for the plugin's configuration object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<serde_json::Value>,
}

impl PluginManifest {
    /// Construct a minimal native manifest with no services declared.
    pub fn new(id: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            version: version.into(),
            runtime: RuntimeKind::Native,
            provides: Vec::new(),
            inject: Vec::new(),
            capabilities: Vec::new(),
            abi: None,
            config_schema: None,
        }
    }
}
