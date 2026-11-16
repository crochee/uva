# uva-agent

agent 词汇 crate：住在 agent 循环（`uva-harness`）之下的运行词汇。
派发方（工具、服务端、第三方委派者）只依赖本 crate 就能描述一次
run、解析 peer、开子会话、组装 prompt，依赖树里不必拉进整个 harness。

## 内容

- **`Agent` trait**：一次 run 的外部契约——`descriptor()` 给出目录视图，
  `run(input, cancel)` 流出一串 `AgentEvent`。具体循环
  （`ReActAgent`）与 `AgentCaps` 仍在 harness。
- **`AgentInput`**：一次 run 的输入——多模态内容、历史、prompt
  manifest 覆盖、日志写入者、转向通道、工作区、续跑令牌；子代理字段
  本阶段保留（删除归子代理轴的删 seam 票）。
- **转向通道**：`RunInbox`（循环每轮开头非阻塞 drain 的契约）与
  `ChannelInbox`（主机保留发送端、run 持有接收端）。
- **`SubagentWriters`**：开子会话、返回该会话唯一写入者的来源；
  子会话开档行由此落地。
- **`AgentRegistry` / `AgentEntry`**：peer 目录，复用
  `uva_core::registry::Registry` 的具名表 + cursor 分页，不另写一份。
- **prompt manifest**：`PromptContext`（技能、peer 代理、插件段落 →
  确定性 system prompt）、`PromptSection` 与 `DEFAULT_SYSTEM_PROMPT`。
  易变的运行时事实不进 manifest——循环以快照消息追加，保持 provider
  前缀缓存命中。

## 路径保持

`uva-harness` 以合并 use 树 re-export 本 crate 的全部名字：crate 根
（`uva_harness::AgentInput` 等）、`uva_harness::agent::Agent`、
`uva_harness::agent::AgentRegistry` 与
`uva_harness::agent::host::PromptSection` 深路径全部不变，冻结基线
不动。

## 非目标

- 不实现循环：ReAct、策略分派、hook 分发与 sink 都在 harness。
- 不写会话行：`SubagentWriters` 只返回写入者，行由该写入者自己写。
- 不承载委派策略：工具如何选择 peer、gate、worktree 属于工具族。

## CI 契约

- `cargo test -p uva-agent --lib` 绿；
- runtime-free（`make check-no-runtime`）；
- 可打包（`make check-publishable`）；
- 依赖树不含 `uva-harness`。
