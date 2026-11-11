# elysper-plugin-runtime — the Wasm component runtime

Opt-in WebAssembly **component** runtime for the Elysper plugin kernel.

A Wasm tool plugin is one component that exports two functions:

```wit
package elysper:plugin@1.0.0;

world tool {
  /// Called once when the plugin is mounted. Returns one tool schema:
  /// `{ "name": string, "description": string, "parameters": JSON Schema }`.
  export describe: func() -> string;

  /// Called once per tool invocation. Receives the call's JSON arguments
  /// and returns a JSON result.
  export execute: func(input: string) -> string;
}
```

The host's calling convention is intentionally JSON-in / JSON-out: the
manifest's `abi = "json"` and `abi = "wit"` both bind this world — a
`wit` component is compiled against the exports above, while the host
still validates and lifts them by name. Nothing is inferred from
`execute`; `describe()` is called exactly once at mount and its schema
is cached.

## What it enforces

- **Capabilities.** A component may import `log` (default), `fs-read`,
  or `http-request` (the component model requires kebab-case import
  names). The linker defines a host function only when the manifest
  declares the matching capability (`log`, `fs:read`, `net:http`); an
  undeclared import fails the mount before any guest instruction
  executes with `MissingCapability`, naming both the import and the
  capability; an import the linker does not know fails instantiation
  with `UnsatisfiedImport`, also naming the import. `log` is implicit;
  `fs-read` confines reads to the runtime's workspace root. `net:http`
  is not built in: the embedding host installs a `CapabilityHost` with
  `WasmRuntime::with_capability_host` and defines the `http-request`
  function. Without one, declaring `net:http` fails with
  `UnsupportedCapability` instead of silently dropping the import.
- **Fuel.** Every `describe` / `execute` gets a fresh fuel budget.
  Exhaustion traps and maps to `WasmErrorKind::FuelExhausted`.
- **Linear memory.** A `ResourceLimiter` refuses a growth beyond
  `max_memory_bytes`; the refusal is recorded and a guest that traps
  after it maps to `WasmErrorKind::MemoryLimit`.
- **Wall clock.** A per-engine epoch ticker interrupts execution after
  `deadline`; the trap maps to `WasmErrorKind::Timeout`.

A trap poisons the pooled store, so the next call instantiates a fresh
one instead of reusing a store whose guest state may be inconsistent.
Successful calls reuse the same store and instance.

The compilation cache holds `Weak` references: dropping the last
`WasmComponent` for a set of bytes evicts the entry and releases the
compiled JIT code. `WasmRuntime::evict(hash)` removes an entry
explicitly for a host that compiled without mounting.

## Default build

`wasmtime` is behind the facade's `wasm` feature and is never part of
the default dependency graph:

```bash
cargo tree -p elysper --no-default-features | grep -c wasmtime  # 0
make check-wasm-deps
```

## Use

```rust
use std::sync::Arc;

use elysper_kernel::{PLUGIN_PROTOCOL_VERSION, PluginManifest, RuntimeKind, WasmAbi};
use elysper_plugin_runtime::wasm::{WasmRuntime, WasmLimits, WasmToolPlugin};

let runtime = Arc::new(WasmRuntime::with_limits(WasmLimits::default())?);
let mut manifest = PluginManifest::new("echo-wasm", PLUGIN_PROTOCOL_VERSION);
manifest.runtime = RuntimeKind::Wasm;
manifest.abi = Some(WasmAbi::Json);
let bytes = std::fs::read("echo.wasm")?;
let plugin = WasmToolPlugin::new(Arc::clone(&runtime), bytes, manifest)?;
// Mount `plugin` with `elysper_kernel::mount_all` next to `ToolRegistryPlugin`.
```

A host that renames the tool or narrows its parameter schema can chain
`WasmToolPlugin::with_schema_override(ToolSchemaOverride { .. })`; the
override is applied once, after `describe()` and before registration.
The app-server exposes the same object from a `wasm` profile row's
optional `tool` config block.
