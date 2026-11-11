//! Opt-in WebAssembly **component** runtime for the Elysper plugin
//! kernel.
//!
//! This crate is the one plugin-kernel piece that carries a JIT and a
//! compiler backend. It is never part of the default build: the facade's
//! `wasm` feature opts in, and `make check-wasm-deps` asserts the default
//! dependency graph contains no `wasmtime`.
//!
//! # What a Wasm tool is
//!
//! One WebAssembly component exporting two functions:
//!
//! ```wit
//! describe: func() -> string;
//! execute: func(input: string) -> string;
//! ```
//!
//! `describe()` is called exactly once, at mount, and its JSON
//! (`{"name": …, "description": …, "parameters": <JSON Schema>}`) is the
//! only schema the model sees. `execute()` is called once per tool
//! invocation; the host serialises the call's arguments to JSON, passes
//! them through the canonical ABI as a UTF-8 string, and validates the
//! returned string as JSON. A non-JSON result is a stable
//! [`WasmErrorKind::InvalidJson`] tool error, never a panic.
//!
//! The same component world backs both manifest ABIs: `abi = "wit"` is a
//! component compiled against those exports, `abi = "json"` is the same
//! component described dynamically. The host validates the exports by
//! name either way.
//!
//! # The guarantees
//!
//! - **Capability gating.** Host imports are linked only when the
//!   manifest declares their capability (see [`capabilities`]). An
//!   undeclared import fails the mount with an error naming the
//!   capability; the guest never runs.
//! - **Resource limits.** Every call gets a fresh fuel budget, a
//!   linear-memory ceiling enforced by a [`ResourceLimiter`], and an
//!   epoch-based wall-clock deadline. Each failure maps to a stable
//!   [`WasmErrorKind`] rather than a generic trap or a host panic.
//! - **Compilation cache.** Components are cached by content hash, so
//!   mounting the same bytes twice compiles once.
//! - **Kernel seams.** A [`WasmToolPlugin`] registers one `Tool` under
//!   the `tools` service and withdraws it on unload; [`WasmSandbox`]
//!   exposes the capability boundary as a `sandbox` backend.
//!
//! [`ResourceLimiter`]: wasmtime::ResourceLimiter

pub mod capabilities;
pub mod component;
pub mod limits;
pub mod tool;

use std::fmt;

pub use capabilities::{
    CAPABILITY_FS_READ,
    CAPABILITY_LOG,
    CAPABILITY_NET_HTTP,
    CapabilityHost,
    CapabilitySet,
    HOST_SUPPLIED,
    HostImports,
    capability_for_import,
    host_supplies,
};
pub use component::{
    ToolSchema,
    ToolSchemaOverride,
    WasmComponent,
    WasmRuntime,
    content_hash,
};
pub use limits::{EPOCH_TICK, WasmLimits};
pub use tool::{WasmSandbox, WasmToolPlugin};

/// The stable failure vocabulary of one Wasm mount or call.
///
/// The kind is the machine-readable answer; [`WasmError::code`] is the
/// wire spelling (`"fuel_exhausted"`, …). A tool adapter copies the code
/// into the [`ToolOutput`](elysper_tool::ToolOutput) metadata, so a host
/// can route or retry without parsing prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WasmErrorKind {
    /// The bytes are not a valid WebAssembly component.
    InvalidComponent,
    /// The manifest does not select the Wasm runtime.
    InvalidManifest,
    /// The engine or its deadline ticker could not be constructed.
    RuntimeInit,
    /// `describe()` is missing, traps, or returns an unusable schema.
    DescribeFailed,
    /// `execute()` is missing or a call setup step failed.
    ExecuteFailed,
    /// `execute()` returned a string that is not JSON.
    InvalidJson,
    /// The component imports a capability the manifest does not declare.
    MissingCapability,
    /// The manifest declares a capability this host does not supply.
    UnsupportedCapability,
    /// Instantiation failed for an import this host does not define.
    UnsatisfiedImport,
    /// The guest exhausted its fuel budget.
    FuelExhausted,
    /// The guest was refused linear-memory growth past the ceiling.
    MemoryLimit,
    /// The guest exceeded its wall-clock deadline.
    Timeout,
    /// The guest trapped for any other reason.
    Trap,
}

impl WasmErrorKind {
    /// The stable wire spelling of this kind.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidComponent => "invalid_component",
            Self::InvalidManifest => "invalid_manifest",
            Self::RuntimeInit => "runtime_init",
            Self::DescribeFailed => "describe_failed",
            Self::ExecuteFailed => "execute_failed",
            Self::InvalidJson => "invalid_json",
            Self::MissingCapability => "missing_capability",
            Self::UnsupportedCapability => "unsupported_capability",
            Self::UnsatisfiedImport => "unsatisfied_import",
            Self::FuelExhausted => "fuel_exhausted",
            Self::MemoryLimit => "memory_limit",
            Self::Timeout => "timeout",
            Self::Trap => "trap",
        }
    }
}

impl fmt::Display for WasmErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A failed mount or call, with a stable kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmError {
    kind: WasmErrorKind,
    message: String,
}

impl WasmError {
    /// Build an error from its kind and a human-readable message.
    ///
    /// The message is diagnostic; [`Self::code`] is what a caller should
    /// branch on.
    #[must_use]
    pub fn new(kind: WasmErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// The machine-readable kind.
    #[must_use]
    pub const fn kind(&self) -> WasmErrorKind {
        self.kind
    }

    /// The stable wire spelling of [`Self::kind`].
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.kind.as_str()
    }

    /// The diagnostic message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for WasmError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for WasmError {}
