# elysper-plugin-protocol

The wire contract for Elysper's **dynamic plugin channel**: the JSON-RPC
method names, the capability handshake, the event and decision
vocabulary, and the stable error codes.

The crate is **serde-only** — `serde` and `serde_json` are the entire
dependency surface — so a plugin author in any language can read this
crate (or the JSON shapes it documents) without inheriting the harness,
a runtime, or a provider stack. Nothing here depends on
`elysper-harness`, which is what keeps the channel acyclic: the harness's
internal `HookEvent` / `HookDecision` enums may change, while this wire
vocabulary stays stable.

## Surface

- `methods` — the `elysper/*` method-name constants. Never hard-code
  the strings elsewhere; both sides agree through these.
- `wire` — the payload types: `PluginCapabilities`, `ToolSpec`,
  `PluginEvent`, `PluginPublish`, `PluginDecision`, `PluginError`.

## Minimal example

```rust
use elysper_plugin_protocol::{PluginCapabilities, PluginEvent, methods};

assert_eq!(methods::INITIALIZE, "elysper/initialize");
assert_eq!(methods::TOOLS_CALL, "elysper/tools/call");

// The handshake is sparse: an absent key means "not supported", which
// deserialises to `false` — the fail-closed reading.
let caps: PluginCapabilities =
    serde_json::from_str(r#"{"tools":true}"#).unwrap();
assert!(caps.tools);
assert!(!caps.observe);
assert!(!caps.transform);
assert!(!caps.gate);
assert!(!caps.publish);

// A notification the plugin sends back.
let event = PluginEvent::AfterTool {
    tool_name: "read".to_string(),
    call_id: "c1".to_string(),
    is_error: false,
};
let json = serde_json::to_value(&event).unwrap();
assert_eq!(json["type"], "after_tool");
```

## The conversation

| Method | Host sends | Plugin sends back |
|---|---|---|
| `methods::INITIALIZE` | initialize request | `PluginCapabilities` |
| `methods::TOOLS_LIST` | list request | `Vec<ToolSpec>` |
| `methods::TOOLS_CALL` | tool name + JSON arguments | tool result JSON |
| `methods::TRANSFORM` | tool result JSON | rewritten result JSON |
| `methods::EVENT` | `PluginEvent` | nothing (notification) |
| `methods::PUBLISH` | nothing (notification) | `PluginPublish` |
| `methods::GATE` | the call to gate | `PluginDecision` (elevated only) |

## Dependencies

- **Depends on**: `serde`, `serde_json`. Nothing else.
- **Depended on by**: `elysper-plugin-host` (the host half) and the
  `elysper` facade's `plugin-protocol` feature. A third-party plugin
  author depends on this crate alone.

## Track

**Library base.** This is the crate a plugin author installs; it must
stay dependency-light and stable, because its shape is a public promise
to third parties.

See `crates/elysper-plugin-protocol/src/lib.rs` for the full API.
