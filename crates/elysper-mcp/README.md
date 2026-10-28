# elysper-mcp

MCP (Model Context Protocol) client: drive any MCP server over a
pluggable transport (stdio + in-memory) and publish its tools into the
local `ToolRegistry`.

> 详细用法与公开 API 见 [`elysper-mcp/src/lib.rs`](src/lib.rs)。

## 用法

```rust
use elysper_mcp::{McpSupervisor, StdioTransport};

let mut supervisor = McpSupervisor::new(registry);
supervisor.spawn("filesystem", StdioTransport::spawn("mcp-server-fs")).await?;
```

## CI 契约

- `cargo test -p elysper-mcp --lib` 绿；
- 默认 features 仅 stdio + in-memory 传输；HTTP / WebSocket 传输若加，
  必须在 facade feature flag 隔离下（`make check-mvp-deps`）。
