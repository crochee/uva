//! Facade re-export for [`elysper_plugin_runtime::wasm`].
//!
//! Enabled by the `wasm` facade feature. The module carries the one
//! plugin-kernel piece with a JIT: a WebAssembly **component** loaded
//! into [`WasmRuntime`], registered as a
//! `Tool` by [`WasmToolPlugin`], and
//! confined by the capability sandbox. The default build never resolves
//! `wasmtime`; `make check-wasm-deps` asserts it.

pub use elysper_plugin_runtime::wasm::*;
