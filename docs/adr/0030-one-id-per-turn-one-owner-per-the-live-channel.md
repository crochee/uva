# 一个回合一个 id：live 通道的条目归一个主人

`chat/message/send` 回的 `message_id` 是路由层现铸的 UUID，与 durable
`parked_prompt` 行的 `park_id` 毫无关系；live 通道（`EventBroadcaster`）的
条目是裸 `AgentEvent`，不带任何回合归属。两件事叠起来：客户端无法把「我发
的这条消息」与「流上这段输出」对上，而一个订阅窗口里混着谁的事件也无从
分辨——`anthropic/messages` 靠「一请求一 ephemeral 会话」回避，原生面没有
这个回避手段。

代价是具体的：无头内核（stdio JSON-RPC）没有原生流式门，客户端只能
`sessions/follow` 读累积快照（O(n²) 字节、自己 diff）或借 Anthropic 方言。

## 决定

- **回合 id 由提交方现铸，随 op 信封交给控制器。**
  `SubmittedOp` 增 `turn_id: Option<String>`；`park_durably` 用它作
  `park_id`（`None` 时控制器自铸，与今天一致）。集群转发
  （`ControlFrame::ForwardOp`）携带它，owner 节点照样落行——id 是回合的
  事实，不是节点的事实，与 provider pin 同一规则。
- **live 通道的条目从 `AgentEvent` 换成 `BroadcastEvent`。**
  `Run { turn_id, event }` 与 `TurnEnded { turn_id, reason }`。run task
  持有本回合的 park id，是唯一知道事件归属的地方；`TurnEnded` 在行 flush
  之后发布，成为 live 流里第一个回合级终信号（此前只有会话级的
  `SessionEnded`）。集群总线载荷同形，tail bridge 按它解码、行载荷解码
  失败照旧跳过。
- **原生流式门是单独的方法，不是 send 的参数。**
  `chat/message/stream`：同一份请求形，回答 `{accepted, turn_id}`，该回合
  的增量以 `chat/turn/event` 通知逐帧发布，pump 按 `turn_id` 过滤、遇
  `TurnEnded` 发终帧退出。先订阅再派发；派发用
  `submit_durable_turn`（落行等待与 send 同一承诺）。
- **网关按 `session_id` 路由 `chat/turn/event`**，与
  `session/event` / `anthropic/messages/event` / `approval/request`
  同一张表。

## 后果

- 回答流、durable 转录、集群转发三方共用一个回合 id；客户端的关联键与
  日志的行 id 是同一个字符串。
- `follow` / `anthropic` 两个既有订阅者各解开一层 `Run` 取事件，行为不变；
  `TurnEnded` 对它们是可忽略的新条目。
- `sessions/follow` 的累积快照语义**不动**——它是 TUI 与浏览器的渲染契约；
  本决定只加门，不改门内的东西。

设计细节与被否掉的备选（follow 上加 delta 帧、send 加 `stream` 参数、
无 id 只靠订阅时序）见
`docs/design/2026-11-24-headless-turn-stream-design.md`。
