# elysper-mcp-server

A stdio MCP server that exposes elysper's plugin-brick tools (`read` /
`write` / `shell` / `todo` / `web_fetch` / `schedule` / `search` plus a
`elysper` overview tool) to any MCP client — `pi`, a Claude Code plugin,
the Anthropic SDK, or your own JSON-RPC 2.0 stack.

`elysper-server` speaks MCP over HTTP *inside* its own address space, so
the deployment's agents can manage themselves. This crate is the
**outbound** path: a foreign client spawns the binary as a subprocess
and speaks newline-delimited JSON-RPC 2.0 over its stdin/stdout.

## Minimal example

Spawn the binary and drive one call:

```text
$ elysper-mcp-server --workspace /path/to/project
→ {"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}
← {"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"…"}}
→ {"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
← {"jsonrpc":"2.0","id":2,"result":{"tools":[…]}}
→ {"jsonrpc":"2.0","id":3,"method":"tools/call",
   "params":{"name":"read","arguments":{"file_path":"…"}}}
← {"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"…"}],
   "isError":false}}
```

stdout carries protocol frames only; stderr carries the operator logs.
The server exits when the client closes stdin (EOF).

## Library surface

The crate ships **both** halves: a library (`elysper_mcp_server`) and
the `elysper-mcp-server` binary that wraps it. The library half keeps
the runtime out of its own surface: `run` is generic over the reader
and the writer, so the transport is the caller's choice. The I/O seam
is a **trait bound** (`tokio::io::AsyncRead` / `AsyncWrite`), not a
concrete runtime type — the same exemption `elysper-server` has:

```rust
use elysper_mcp::ServerInfo;
use elysper_mcp_server::{ServerSurface, run};

async fn serve<R, W>(
    surface: ServerSurface,
    info: ServerInfo,
    reader: R,
    writer: W,
) -> Result<(), Box<dyn std::error::Error>>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    run(surface, info, reader, writer).await?;
    Ok(())
}
```

The binary hands `run` the process's own stdio; `main.rs` is the only
place that holds concrete runtime types — the same exemption
`elysper-server` has.

## Dependencies

- **Depends on**: `elysper-mcp` (the protocol server), `elysper-tool`
  plus the tool plugins (`-read`, `-write`, `-shell`, `-todo`, `-web`,
  `-scheduler`, `-search`), `elysper-scheduler`, `elysper-search`,
  `elysper-core`, `elysper-provider`.
- **Depended on by**: nothing inside the workspace. It is a leaf: a
  consumer either spawns the executable
  (`cargo install elysper-mcp-server`, or the binary your deployment
  shipped) or links the library to drive `run` over its own transport.

## Track

**Library base (binary leaf).** `cargo add elysper-mcp-server` installs
the library half; `cargo install elysper-mcp-server` installs the
server a foreign MCP client spawns. The wire proof lives in
`crates/elysper-mcp-server/tests/stdio_roundtrip.rs` and
`tests/pi-integration.ts`.

See `crates/elysper-mcp-server/src/lib.rs` for the full API.
