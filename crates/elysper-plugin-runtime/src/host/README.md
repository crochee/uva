# elysper-plugin-runtime — the host half

The host half of Elysper's dynamic plugin channel: it turns a plugin
**process** that speaks `elysper-plugin-runtime`'s `protocol` module into registrations on
the harness's `Host`, so the agent loop never learns that a plugin
exists.

The static half — the `Plugin` trait, `Registration` and the withdrawal
contract — lives in `elysper-harness`. This crate is only the adapter.

## Layers

```text
RemotePlugin::install(&mut Host)
  ├─ process     std::process::Command, piped stdin/stdout
  ├─ client      line-delimited JSON-RPC 2.0, correlated by `id`,
  │              one reader thread + one deadline reaper per plugin
  ├─ RemoteTool  one elysper_tool::Tool per `elysper/tools/list` row
  ├─ observer    hook events → `elysper/event` notifications
  └─ event_sink  `elysper/publish` notifications → PluginEventSink
```

Every message is bounded by one deadline. A plugin that misses it is
treated as wedged and killed, because a plugin that stops reading its
stdin blocks the host's writer. An expired deadline fails *that call*,
not the channel — the one exception is the plugin that stopped reading
its stdin, where only the kill releases the blocked writer.

## Minimal example

```rust
use elysper_harness::Host;
use elysper_plugin_runtime::host::{
    Authority,
    RemotePlugin,
    RemotePluginConfig,
    TransportKind,
};
use elysper_plugin_runtime::protocol::PluginCapabilities;
use elysper_tool::Risk;

fn config() -> RemotePluginConfig {
    RemotePluginConfig {
        name: "demo".to_string(),
        command: vec!["my-plugin".to_string()],
        transport: TransportKind::Stdio,
        capabilities: PluginCapabilities {
            tools: true,
            observe: true,
            transform: false,
            gate: false,
            publish: false,
        },
        authority: Authority::Reduced,
        risk: Risk::Low,
        call_timeout_ms: Some(5_000),
        env: None,
    }
}

async fn install(host: &mut Host) -> Result<(), Box<dyn std::error::Error>> {
    let plugin = RemotePlugin::new(config());
    plugin.install(host).await?;
    Ok(())
}
```

## Authority

`Authority::Reduced` is the default: register tools, observe events,
rewrite a tool result — nothing that steers the run. `Authority::Full`
is the operator's promotion (never something a plugin can ask for) and
requires the plugin to announce the `gate` capability; a full-authority
plugin that does not is refused at install rather than left blocking
every call.

Publishing is not an authority question at all: `elysper/publish` is
the plugin's own voice, granted by the `publish` capability (the
operator's ceiling and the plugin's handshake declaration, both or
neither) and delivered to the `PluginEventSink` attached with
`RemotePlugin::with_event_sink`. It steers nothing — it only tells.

## Dependencies

- **Depends on**: `elysper-plugin-runtime`'s `protocol` module, `elysper-harness`,
  `elysper-tool`, `elysper-core`, `elysper-steering`.
- **Depended on by**: the `elysper` facade's `plugin-host` feature and
  `elysper-server` (the dynamic-plugin route).

## Track

**Library base.** Runtime-free by construction: the plugin is a
`std::process::Command` and its blocking IO runs through
`elysper_core::blocking::Blocking`, so `cargo tree -p elysper-plugin-runtime
-e normal | grep -c tokio` prints `0`.

See `crates/elysper-plugin-runtime/src/host/mod.rs` for the full API.
