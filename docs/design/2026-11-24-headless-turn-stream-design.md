# 无头内核的原生回合流：`chat/message/stream` 设计

日期：2026-11-24
状态：已实现（与 react-only 循环收敛同一轮）
取代：无

---

## 1. 意图与为什么

用户对无头内核（`uva-app-server core`，stdio JSON-RPC 2.0）的要求是
「无头模型需要正常流式且丝滑」。现状有两层不满足：

1. **没有原生流式门**。`chat/message/send` 回 `{queued: true}` 就结束了；
   客户端要看到模型的输出，要么 `sessions/follow` 读**累积快照**（每帧重发
   全文，O(n²) 字节；客户端自己 diff），要么走 `anthropic/messages`
   的 Anthropic 方言——那是协议兼容层，不是 Uva 自己的词汇。
2. **回合不可寻址**。`send` 回的 `message_id` 是路由层现铸的 UUID，与
   durable `parked_prompt` 行的 `park_id` 毫无关系；客户端无法把「我发的
   这条消息」与「流上这段输出」对上。

OMP 的形状（一次请求、同一 stdout 上的带 id 关联事件帧）是对的参照：
单连接、类型化帧、按 id 关联。

## 2. 设计

**新方法 `chat/message/stream`**（参数与 `chat/message/send` 相同）：

```text
→ chat/message/stream {session_id, text}
← {accepted: true, turn_id: "<uuid>"}
← chat/turn/event {session_id, turn_id, seq: 1, frame: {kind: "text", text: "…"}}
← chat/turn/event {session_id, turn_id, seq: 2, frame: {kind: "end", reason: "completed"}}
```

帧词汇（`frame.kind`）：`text` / `reasoning`（模型增量）、`tool_call` /
`tool_progress` / `tool_result`（工具生命周期）、`usage` / `warning`、
`gap`（事件被丢弃的诚实声明）、`end`（终帧，reason ∈ completed /
cancelled / suspended）。`seq` 从 1 编号，客户端可用自己的算术发现丢帧。

**三个配套决定**：

| 决定 | 内容 | 为什么 |
|---|---|---|
| 回合 id 由路由现铸 | 路由 mint UUID → 随 op 信封交给控制器 → `park_durably` 用它作 `park_id` | 一个 id 同时是 live 帧的关联键与 durable 行的 id；集群模式下 id 随 `ControlFrame::ForwardOp` 过总线，owner 节点照样落行 |
| live 通道的 item 换形 | `EventBroadcaster` 的条目从 `AgentEvent` 改为 `BroadcastEvent::{Run{turn_id, event}, TurnEnded{turn_id, reason}}` | 事件必须带着回合走，否则「排在前面的回合的尾巴」会污染本回合的流；`TurnEnded` 在 run 收尾（行已 flush）后发布，是回合级终信号——live 事件流里原本没有它 |
| pump 按回合过滤 | 路由先订阅再派发，spawn 的 pump 只保留 `turn_id` 匹配的条目，遇 `TurnEnded` 发终帧退出 | 控制器一回合一 run，但 pump 的订阅窗口可能含前一回合尾部；按 id 过滤是唯一不依赖时序的判据 |

**网关**（`serve` 模式）：`chat/turn/event` 与 `session/event` 同表，按
`params.session_id` 路由到订阅了该会话的连接——无头客户端经网关用时先
`sessions/follow`（它本来就需要会话转录）。

## 3. 被否掉的备选

| 备选 | 为什么不做 |
|---|---|
| 在 `sessions/follow` 的 live 帧上加 delta | live 帧的累积文本是 TUI 与浏览器的渲染契约（`live.text.startsWith(durable.text)` 一类），改语义要动两个 GUI 客户端；本轮不动它们 |
| `chat/message/send` 加 `stream: true` | 一个方法两种响应形（`MessagesOutcome` 式 untagged union）让 `SendMessageResponse` 变多态；单独的方法让契约各归各 |
| 只靠「订阅先于派发 + 终态事件即停」不带回合 id | 会话里排在前面的回合会让 pump 把别人的答案当自己的；Anthropic 兼容面靠 ephemeral 会话回避这个问题，原生面不该回避 |

## 4. 不做的事

- 不改 `sessions/follow` 的累积快照语义（GUI 契约）。
- 不改 `anthropic/messages` 的流（它有自己的方言与终态规则）。
- 不给 `chat/message/send` 的响应加 `turn_id`（那会动 web 契约的
  `SendMessageResponse`；流式门自己回答 turn id 就够了）。

## 5. 验证

- `crates/uva-app-server/tests/stdio_roundtrip.rs::a_turn_streams_its_deltas`
  —— 真二进制、真管道：accepted/turn_id、帧按 seq 编号、text 帧含答案、
  end 帧带 `completed`。
- `tests/parked_prompt.rs::a_turn_parks_under_the_submitters_id` —— 提交方
  的 id 就是落盘行的 `park_id`。
