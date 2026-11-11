# plugin-author — a third-party tool plugin

The smallest thing that is really "a plugin": implement the `Tool` trait,
register it, call it. It is what a **third-party** plugin author writes —
and it lives outside the framework's workspace on purpose.

## The dependency story

```toml
[dependencies]
elysper = { path = "../../../crates/elysper", default-features = false, features = [
  "tool",
  "kernel",
] }
futures = "0.3"
```

- **`elysper`** — the one framework entry. `tool` gives the `Tool` trait, the
  `ToolRegistry` and `ToolEntry`; `kernel` gives the plugin lifecycle
  (`Plugin` / `EffectHandle` / `mount_all`) if you want your tool to be a
  kernel plugin that registers and withdraws itself.
- **`futures`** — an executor, so the demo can run the one async call.
  Runtime-agnostic on purpose: a plugin author is not pushed onto tokio.
- **`serde_json`** — *not* a dependency: the facade re-exports the exact
  version the framework was built against (`elysper::serde_json`), because
  `Tool::parameters` speaks `serde_json::Value` and a different version
  would not satisfy the trait.

## Run it

```sh
cargo run -q
# PLUGIN-AUTHOR: OK
```

## What to copy

1. **Implement the trait** — four methods: `name`, `description`,
   `parameters` (a JSON Schema) and `call` (parse the input, return a
   `ToolOutput`). Parse defensively and report a model-facing error
   (`ToolOutput::error`) instead of panicking.
2. **Register it** — `registry.register_entry(ToolEntry::new(Arc::new(MyTool)))`.
3. **Optionally wrap it as a kernel plugin** — a type that holds the tool
   and files it in the injected `tools` registry during `apply`; the
   effect handle returned by `apply` withdraws exactly that registration
   when the plugin unloads. The shipped plugins (see
   [`docs/plugins.md`](../../plugins.md)) all do this.

The derive macro is the short path when you want it:
`#[derive(Tool)]` from `elysper-macros` (the facade's `macros` feature)
generates the four methods from a struct plus an inherent `execute`.
