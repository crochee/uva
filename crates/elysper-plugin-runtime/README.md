# elysper-plugin-runtime

The **dynamic plugin channel** in three halves:

| feature | module | what it is |
|---|---|---|
| `protocol` (default) | [`protocol`](src/protocol/README.md) | the JSON-RPC method names, the capability handshake, the event and decision vocabulary, the stable error codes — **serde-only**, so a plugin author in any language can read it |
| `host` | [`host`](src/host/README.md) | turns a plugin **process** into registrations on the harness's `Host`, so the agent loop never learns that a plugin exists |
| `wasm` | [`wasm`](src/wasm/README.md) | loads a WebAssembly **component** and exposes its `describe` / `execute` pair as a tool |

## Usage

```toml
[dependencies]
elysper-plugin-runtime = { version = "0.1", features = ["host"] }
```

Through the facade, the same three halves are one feature away:

```toml
elysper = { version = "0.1", features = ["plugin-protocol", "plugin-host"] }
```

## The three halves are one channel

The protocol is the vocabulary; the host speaks it over stdio or HTTP;
the Wasm runtime is a third way to produce a tool that obeys the same
contract without a process at all. A deployment picks the halves it
needs.

## The one cross-family edge

The Wasm backend confines a component with the **shell** plugin's
sandbox seam (`SandboxBackend` / `ExecutionPolicy`), so the `wasm`
feature depends on `elysper-tool-plugins`'s `shell` feature. That edge
is optional, one-way and absent from the default build — the protocol
half pulls nothing but `serde`.

## CI 契约

- 默认构建（仅 `protocol`）的依赖树里没有 `wasmtime`：`make check-wasm-deps` 断言；
- `cargo test -p elysper-plugin-runtime --all-features` 绿；
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p elysper-plugin-runtime
  --all-features` 绿（门禁见 `AGENTS.md` §3.6）。
