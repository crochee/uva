//! # elysper-plugin-runtime
//!
//! The **dynamic plugin channel** in three halves — the wire contract,
//! the host that speaks it, and the Wasm component runtime:
//!
//! | feature | module | what it is |
//! |---|---|---|
//! | `protocol` (default) | [`mod@protocol`] | the JSON-RPC method names, the capability handshake, the event and decision vocabulary, the stable error codes — **serde-only**, so a plugin author in any language can read it |
//! | `host` | [`mod@host`] | turns a plugin **process** into registrations on the harness's `Host`, so the agent loop never learns that a plugin exists |
//! | `wasm` | [`mod@wasm`] | loads a WebAssembly **component** and exposes its `describe` / `execute` pair as a tool |
//!
//! ## The three halves are one channel
//!
//! The protocol is the vocabulary; the host speaks it over stdio or
//! HTTP; the Wasm runtime is a third way to produce a tool that obeys
//! the same contract without a process at all. A deployment picks the
//! halves it needs:
//!
//! ```toml
//! [dependencies]
//! elysper-plugin-runtime = { version = "0.1", features = ["host"] }
//! ```
//!
//! ## The one cross-family edge
//!
//! The Wasm backend confines a component with the **shell** plugin's
//! sandbox seam (`SandboxBackend` / `ExecutionPolicy`), so the `wasm`
//! feature depends on `elysper-tool-plugins`'s `shell` feature. That
//! edge is optional, one-way and absent from the default build — the
//! protocol half pulls nothing but `serde`.

#[cfg(feature = "host")]
pub mod host;
#[cfg(feature = "protocol")]
pub mod protocol;
#[cfg(feature = "wasm")]
pub mod wasm;

#[cfg(feature = "host")]
pub use host::{
    Authority,
    HostError,
    PluginEventSink,
    PluginRegistry,
    ProcessPluginsPlugin,
    PublishedEvent,
    RemotePlugin,
    RemotePluginConfig,
    TransportKind,
};
#[cfg(feature = "protocol")]
pub use protocol::{methods, wire};
#[cfg(feature = "wasm")]
pub use wasm::{
    WasmError,
    WasmErrorKind,
    WasmLimits,
    WasmSandbox,
    WasmToolPlugin,
};
