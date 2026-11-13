# uva-gateway

两个宿主（`uva-tui` 与桌面壳）共用的网关支撑，`publish = false`：
一半管**进程**，一半管**协议**。

- **supervisor**（`supervisor.rs`）：拉起 `uva-app-server serve` 子进程，
  等 `/readyz` 的 **HTTP/1.1 200 状态行**（不是"端口能连"——网关在监听但
  core 还在起时是假就绪），进程死亡后按 1s → 5s 退避重启（重启期间
  `base_url()` / `ws_url()` 保持旧值，`states()` 发 `Restarting`），宿主
  drop 句柄时 kill + reap。
- **client**（`client.rs`）：网关 `/rpc` WebSocket 上的 JSON-RPC 2.0——一条
  文本消息一个报文（Content-Length 分帧只在网关 ↔ core 之间），按 `id`
  关联响应，通知按 `method` 扇出给订阅者，断线后按 250ms → 2s 退避重连，
  **重连后重放全部订阅**（注册表是唯一分发来源，重放不会把处理函数挂两次）。

杀的是**直接子进程**（`serve` 本身）；本 crate 不做进程组管理——若子进程
再派生孙进程，需要宿主自己在更外层兜底。

## 用法

```rust
use std::{path::PathBuf, sync::Arc};

use uva_gateway::{Gateway, GatewayClient, GatewayOptions};

let options = GatewayOptions::new(
    PathBuf::from("uva-app-server"), // 可执行文件（port 默认 0 = 由本 crate 选空闲端口）
    PathBuf::from("."),                  // 工作区目录（透传 serve --directory）
);
let gateway = Gateway::start(options).await?; // 就绪才返回
let _base = gateway.base_url().to_owned();    // http://127.0.0.1:<port>

let client = GatewayClient::connect(gateway.ws_url()).await?;
let _sessions = client
    .request("sessions/list", serde_json::json!({}))
    .await?;

// 订阅：句柄 drop 即退订；断线重连后自动重放注册
let subscription = client.subscribe(
    "session/event",
    Arc::new(|params| {
        let _ = params; // 画到界面上
    }),
)?;

drop(subscription);
client.close();
drop(gateway); // kill + reap 子进程
```

## The desktop shell

The desktop entry (`desktop/`, a Tauri 2 shell in its own workspace,
deliberately outside the root one) consumes only the **supervisor**
half: `Gateway::start` → `base_url()` → open the window at that URL,
re-navigate on a new `Ready`, reap the child when the window closes.
It never touches `GatewayClient`, so nothing in the shell depends on
`tokio-tungstenite` (design D14) — the client stays in this crate for
the next host that wants to speak JSON-RPC.

The sidecar-binary discovery order and the dev/release port policy
live in `desktop/src-tauri/src/options.rs` (module docs). The shell's
own gates (fmt / clippy / test / `cargo deny` over the desktop
lockfile) run in `.github/workflows/desktop.yml`, not in the root
`make ci`.

## 边界

- `publish = false`：它消费的是 `uva-app-server` 的 CLI 契约，而后者
  本身 `publish = false`，这里没有任何属于 registry 的东西。
- 公开面只用 `futures`（`BoxFuture` / `futures::channel::mpsc`），`tokio`
  是实现细节，因此无需进入 `make check-public-api-runtime` 的豁免名单。

## 分层与测试

- **supervisor**：`Launcher` / `GatewayProcess` 是 crate 内的注入缝——生产
  路径 spawn 真进程、拨真 TCP，测试用假进程与脚本化探针把"就绪必须读到
  200 状态行""退避期间 URL 不变但发 Restarting""port=0 选出的端口真正传给
  serve"逐条钉死，不需要网关二进制。
- **client**：测试用进程内 axum WebSocket 服务器脚本化每次连接（回答
  请求 / 断开 / 重连后推通知），覆盖"同 id 响应""通知扇出""重连重放且
  处理函数只被调一次""error 对象映射为 `ClientError::Server`"。
