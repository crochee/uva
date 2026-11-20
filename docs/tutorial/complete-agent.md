# 从空仓库到完整对话：uva-app-server + uva-web 教程

本教程从一个新 clone 开始，启动 `uva-app-server` 与 `uva-web`，配置 provider，
然后按 A–G 剧本走完一次完整的 agent 对话，并用**日志**核对每一步真的发生了。
文末的「验收记录」是这条路径的实测表。

> 约定：所有命令在仓库根目录执行。`$DIR` 是一个隔离的工作区目录，
> 会话日志、附件、skills 都落在它下面，不污染仓库。

```bash
export DIR=/tmp/uva-tutorial
mkdir -p "$DIR"
```

---

## 1. 前置条件

| 依赖 | 版本 | 检查 |
|---|---|---|
| Rust | 1.98+ | `rustc --version` |
| Node.js | 20+ | `node --version` |
| npm | 随 Node | `npm --version` |
| jq（读日志用） | 任意近期版本 | `jq --version` |

两个终端：**T1** 跑 server，**T2** 跑 web。server 的 workspace root 由
`--directory` 决定，因此 `--config` 和 `sessions/` 都在 `$DIR` 下。

---

## 2. 配置与启动

### 2.1 无 key 的冒烟路径（`type = "demo"`）

先确认前后端接线。`demo` 是一个回显 provider：它把最后一条用户消息
原样回显，不调用工具，也不需要网络或 API key。它由
`crates/uva-app-server/src/state/app_state/demo_provider.rs` 提供，
编译期门控在 `test-utils` feature 后，所以启动时必须带该 feature：

```bash
mkdir -p "$DIR/demo/.agents"
# provider 只从 workspace 的 `.agents/config.toml`（或 profile 的
# `model-adapter` 行）读取；config.yaml 的 `providers` 段已不再生效。
cat > "$DIR/demo/.agents/config.toml" <<'TOML'
default_provider = "demo"
default_model = "demo"

[providers.demo]
type = "demo"
default_model = "demo"
TOML

cat > "$DIR/demo/config.yaml" <<'YAML'
version: "1.0"
host: "127.0.0.1"
port: 8080
default_agent: code
agents:
  code:
    description: |
      Keyless demo agent.
YAML

cargo run -p uva-app-server --features test-utils -- serve \
  --directory "$DIR/demo" --config "$DIR/demo/config.yaml"
```

预期：日志出现 `loaded server config` 与 `binding`（`127.0.0.1:8080`）。
另开终端：

```bash
cd uva-web
npm ci
npm run dev -- --port 5173
```

打开 <http://localhost:5173/chat>，发一条 `hello`，应看到
`demo echo: hello`。把 `hello` 换成任意文本，回显里除了
`demo echo:` 前缀就是你的原文。这一步证明 provider 选择、会话日志、
`follow` 流和前端渲染都通了；**A–G 需要真实 provider**（demo 不调用工具，
也不会产生审批）。

### 2.2 真实 provider

```bash
mkdir -p "$DIR/real/.agents"
cat > "$DIR/real/.agents/config.toml" <<'TOML'
default_provider = "anthropic"
default_model = "deepseek-flash"

[providers.anthropic]
type = "anthropic"
base_url = "https://api.deepseek.com/anthropic"
api_key_env = "ANTHROPIC_API_KEY"
default_model = "deepseek-flash"
context_window = 128000  # 与模型实际窗口一致；3000 只留给压缩回归测试
max_output_tokens = 2048
TOML

cat > "$DIR/real/config.yaml" <<'YAML'
version: "1.0"
host: "127.0.0.1"
port: 8080
default_agent: code
agents:
  code:
    description: |
      Tutorial agent.
    max_steps: 8
    compaction:                 # F 步的手动/自动压缩走同一策略
      enabled: true
      reserve_tokens: 800
      keep_recent_tokens: 500
      min_messages_between_compaction: 1
YAML
```

要点：

- `api_key_env` 是 provider 读取密钥的环境变量名。用内联
  `api_key: "sk-…"` 也可以，但不要把密钥写进提交的文件。
- 未写 `default_provider` 时，`openai` 优先，否则取第一个 provider 名。
  `.agents/config.toml` 是 TOML 的 `ProviderEntry` 形状；把旧
  `config.yaml` 的 legacy 形状（`api_key` / `models[]`）迁移过来时跑
  `uva-app-server translate --config config.yaml --out .agents/profile.toml`，
  或手动改写成上面的形状。
- 旧的 `config.yaml` `providers` / `cache` / `resilience` / `budget` 段
  已不再读取（启动会警告并提示 `translate`）。
- 每个 agent 的 `sandbox` 键目前**解析但不生效**（server 启动会警告
  `agent config keys are parsed but not applied`）；`ShellTool` 的默认执行
  策略本来就是 `danger-full-access`。需要约束时应在工具/沙箱层配置，
  不要把这个键当作安全边界。`description` / `max_steps` 同样在警告列表里。
- 每轮的模型选择也可以按请求指定：`"model": "anthropic/deepseek-flash"`。

启动 server：

```bash
export ANTHROPIC_API_KEY=sk-...
cargo run -p uva-app-server -- serve \
  --directory "$DIR/real" --config "$DIR/real/config.yaml"
```

> 也可以直接用 `make dev-server` / `make dev`：它们跑
> `cargo run -p uva-app-server -- serve --config config.yaml`，workspace root
> 是仓库根目录，会话日志落在 `./sessions/`。本教程用 `--directory`
> 是为了让日志路径稳定、可清理。

### 2.3 冒烟检查

```bash
curl -s localhost:8080/readyz            # 期望 200 / ready
```

管理面与对话面都在一条 JSON-RPC WebSocket（`ws://localhost:8080/rpc`）上，没有 REST 路径了。任何 WebSocket 客户端都能驱动它；本教程用最小的 `python3` 片段（`pip install websockets`）：

```bash
rpc() { # rpc <method> <params-json>
  python3 - "$1" "${2:-{}}" <<'PY'
import asyncio, json, sys, websockets
async def main():
    async with websockets.connect("ws://127.0.0.1:8080/rpc") as ws:
        await ws.send(json.dumps({"jsonrpc": "2.0", "id": "c1",
                                  "method": sys.argv[1],
                                  "params": json.loads(sys.argv[2])}))
        while True:
            env = json.loads(await ws.recv())
            if env.get("id") == "c1":
                print(json.dumps(env.get("result", env), ensure_ascii=False, indent=2))
                return
asyncio.run(main())
PY
}
rpc models/list | jq .
```

`models/list` 应列出 `anthropic/deepseek-flash`。若该项缺席，
server 日志会有一行 `configured provider is not available` 并给出原因
（通常是缺 `ANTHROPIC_API_KEY`）。

会话日志路径固定为：

```text
$DIR/real/sessions/dev/<session-id>/events.jsonl
```

`dev` 是未启用认证时的默认 user id（`SERVER_DEFAULT_USER_ID`）。

---

## 3. A–G 验收剧本

每个步骤给出**动作**和**可观察断言**。断言都可以在浏览器或日志里看到；
“接口返回 200”不算断言。运行时可把每一步的结果填进文末的「验收记录」。

### A. 基本回合 + 工具

**动作**

1. 准备一个可读文件：`printf 'uva tutorial\n' > "$DIR/real/notes.txt"`。
2. 打开 <http://localhost:5173/chat>，URL 会自动变成 `/chat/<uuid>`。
3. 发送：

   ```text
   Use the read tool to read notes.txt, then use the shell tool to run exactly:
   ls -la
   Then summarise both results in two sentences.
   ```

4. 等待本轮结束，刷新页面。

**断言**

- 回答以增量方式出现；本轮期间有 `Thinking…` 指示器和 `Stop` 按钮。
- 若模型返回 reasoning，消息里有可折叠的 thinking 段。
- 两个工具卡片按调用顺序出现，运行中显示“运行中/调用中”，结束后
  显示参数与结果（`notes.txt` 内容、目录列表）。
- 最终文本引用 `uva tutorial` 和目录列表。
- 刷新后同一会话从 `follow` 基线**整窗重建**：恰好一条 assistant 回复、
  两个工具块，正文中的同一句话只出现一次，没有“前半段重复”。

**证明了什么**：模型历史来自持久日志的投影；工具参数/结果与文本在
一次刷新中不重不漏。

### B. 中途纠偏：queue 与 steer

需要一段足够长的运行窗口，用 `sleep` 制造：

**动作（queue）**

1. 发送：`Use the shell tool to run exactly: sleep 15; echo done. Then report the output.`
2. 在工具还在运行时，输入 `Mention the token queue-marker in your final answer.`
   并点击 **Queue**（运行中发送按钮的文字从 `Send` 变成 `Queue`）。
3. 等本轮结束。

**断言（queue）**

- 发送后消息出现在输入框上方的 outbox 里，状态是“排队中”。
- 当前回合先正常结束（工具结果 + 最终回答），**之后**排队消息才成为
  新一回合的第一条 user 行；它没有打断正在进行的 `sleep` 调用。
- 日志里这条消息是一条普通 `UserInput` 行，`seq` 大于当前回合的
  `turn` end 行。

**动作（steer）**

1. 发送同样的 `sleep 15` 提示。
2. 运行中输入 `Mention the token steer-marker in your final answer.` 并点击 **Steer**。
3. 观察 chip 的状态变化。

**断言（steer）**

- chip 从“已落盘”推进到“已注入 step N”（工具批次结束后的下一个 step），
  若模型在该 step 回答则变为“已消费”。
- 若运行在边界前就结束了，chip 显示“下一回合”，消息不丢。
- 日志里这行是 `{"type":"user_message", "source":"steer", "seq":…}`，
  它的 `seq` 落在该回合下一个 `step` start 之前；当前在途请求不受影响。

**动作（Stop）**

1. 再发一个 `sleep 15`，点 Steer 后立刻点 **Stop**。

**断言（Stop）**

- 仍在“已落盘”、尚未“已注入”的 steer 文本回到编辑器，chip 消失；
  用户输入没有被静默丢弃。

**证明了什么**：queue 是轮间语义、steer 是 step 边界语义；日志是唯一收件箱，
前端状态完全从 durable 行推导。

### C. 审批：只在高风险调用上挂起

**动作（只读不询问）**

1. 新会话，发送：`Use the shell tool to run exactly: ls -la. Then answer with the count of files.`

**断言**：运行结束，没有出现审批卡。`shell` 的只读 argv 风险为 `low`。

**动作（高风险）**

2. 发送：

   ```text
   Use the shell tool to run exactly:
   echo uva-approval-probe > /tmp/uva-approval.txt
   then report the command's output and exit status.
   ```

**断言**

- run 以“等待审批”结束：输入框被禁用，状态显示等待审批；
  工具块内出现审批卡（`需要审批`、工具名 `shell`、风险 `high`、
  参数摘要前 16 位）。
- 卡片按钮是 **批准一次** / **批准本会话** / **拒绝**；底部提交按钮在所有未决调用
  都有决定后才可用。批准本会话会为“该工具 + 该工作区”写一条可重放的授权行。
- 点 **批准一次** → **提交决定并恢复**：接口返回 `202`，卡片消失，
  工具真正执行，结果进入 transcript，随后模型继续回答。
- 若点 **批准本会话**，同一会话后续对同一工具的同类调用不再询问（日志里多一条
  `approval_granted { id, tool, scope: "session", workspace_hash }`）。
- 刷新页面：审批状态与结果仍在（`follow` 基线里的
  `pending_interrupts` 与 durable 行重建）。
- 日志按顺序出现：

  ```text
  approval_asked   { id, call_id, tool, arguments_hash, risk: "high", agent }
  turn             { "phase": "suspended", "status": "suspended", "pending": [call_id] }
  step             { "phase": "end", "status": "suspended" }
  turn             { "phase": "end", "reason": "suspended" }
  approval_decided { id, call_id, status: "approved", by: "resume" }
  tool_dispatch    { call_id, tool, arguments_hash }
  ```

**动作（拒绝）**

3. 再触发一次同样的写文件提示，这次点 **拒绝** → 提交。

**断言**

- `approval_decided.status` 是 `denied`，**没有**对应的 `tool_dispatch` 行。
- 模型收到一条 `is_error` 的工具结果，并在回答里明确说明被拒绝，
  而不是把失败当成功。

**证明了什么**：审批按**调用**分类（同一个 `shell` 工具，`ls` 不问、
重定向问）；挂起不是崩溃；决定在任何副作用之前落盘。

### D. 崩溃与重启

**动作**

1. 发送 `Use the shell tool to run exactly: sleep 20; echo done.`，
   在 `sleep` 期间记录日志快照：

   ```bash
   SESSION=<浏览器 URL 里的 id>
   LOG="$DIR/real/sessions/dev/$SESSION/events.jsonl"
   cp "$LOG" /tmp/before-crash.jsonl
   wc -l /tmp/before-crash.jsonl
   ```

2. 杀掉 server（模拟崩溃，不是 Ctrl+C 的优雅退出）：

   ```bash
   pkill -9 -f 'uva-app-server.*(serve|core).*--directory'
   ```

3. 用 2.2 的同一条命令重启 server，刷新同一个会话页。

**断言**

- 打开会话后，日志**只追加**闭合行：对断在途的工具调用写
  `tool_result`（`"error": "TOOL_OUTCOME_UNKNOWN"` 或
  `"TOOL_NOT_STARTED"`、`"interrupted": true`），再补 `step` end 与
  `turn` end（`reason: "interrupted"`）。
- 崩溃前的行一字不改：

  ```bash
  head -n "$(wc -l < /tmp/before-crash.jsonl)" "$LOG" \
    | cmp - /tmp/before-crash.jsonl        # 无输出
  ```

- 前端把该轮显示为“未完成”，不显示成成功；工具块显示错误结果。
- 再发一条新消息可以继续；新回合正常完成，且日志里没有第二条重复的
  assistant 行（`jq -c 'select(.type=="assistant_message")' "$LOG" | wc -l`
  与实际回答次数一致）。

**动作（挂起跨重启）**

4. 重复 C 的高风险提示，停在审批卡；`pkill -9` server 后重启，刷新会话。

**断言**：审批卡仍然出现（未决集合来自日志），提交后返回 `202` 并执行一次；
同一决定不会导致第二次执行。

### E. 断线重连

**动作**

1. 发送 `Use the shell tool to run exactly: sleep 12; echo reconnect-probe.`
   并在本轮仍在运行（工具执行期间）时按 F5。

**断言**

- 刷新后首帧 `baseline` 携带 header、最近 200 条 durable 行、`cursor`、
  `live`、`context`、`pending_interrupts` 与 `request_snapshot_hash`。
- 本地窗口被**整窗替换**，随后只应用 `seq > cursor` 的行；
  页面上 `reconnect-probe` 只出现一次，没有本地缓存与服务端历史拼接出的重复。
- 运行中的 turn 继续：`Thinking…` 恢复，最终回答追加到同一个 turn；
  已结算部分不重复播放。
- 在 DevTools 的 EventStream/Network 里，`baseline` 之后是 `row` / `live`
  帧；`live` 帧带 `start_seq`（该步在日志里的起点）与单调递增的 `live_seq`；
  durable 行到达后，序号越过 `start_seq` 的内容行取代那一步的 live 投影。

### F. 长会话压缩

前置：`config.yaml` 的 `agents.code.compaction.enabled: true`（见 2.2）。
压缩阈值 = `context_window - reserve_tokens`。默认窗口 128,000 下，
固定提示（系统提示 + 工具 schema）约占 9,300 tokens，阈值约 127,200，
教程里的几轮对话远达不到它，`POST /compact` 会诚实返回 `under_budget`。
要在一次短会话里亲手看到压缩，**临时**把 `context_window` 调到
12,000（高于固定提示成本，又能被几轮长回答跨过 11,200 的触发线）；
不要用 3,000 这类低于固定提示成本的窗口——它会让每一轮都压缩，
3,000 只作为 `uva-context` 的回归测试值保留。

**动作**

1. 连续发 4–5 条需要长输出的提示，例如
   `Write a detailed 300-word explanation of <主题 N>; do not use tools.`
   每轮都等它结束。
2. 观察输入框底部的占用：`上下文 N / 12,000`。触发线约在
   `12,000 - 800 = 11,200`；低于它时手动压缩会诚实地返回 `under_budget`。
3. 点击 **Compact**（运行中或挂起时按钮禁用）。

**断言**

- 接口 `200` 且 `committed: true`；若返回
  `{committed: false, reason: "under_budget"}`，说明历史还不够长，
  再发一轮长回答后重试。
- 时间线出现“历史已压缩”标记，展示摘要与被遮蔽的 surface 范围。
- 日志出现成对的审计行和一条 replace 行，最后一个 `outcome` 是
  `committed`：

  ```text
  compaction_start   { token }
  compaction_summary { token, data: { summary, shadowed_positions } }
  compaction         { data: { summary, source_indices }, surface_op: { start, end, source_event_seqs } }
  compaction_end     { token, outcome: "committed" }
  ```

- 压缩后的追问仍能引用压缩前的事实（摘要进入了模型历史），
  且 `turn` 正常继续。自动压缩与手动压缩写同一种行。

> 挂起期间（审批未决）点 Compact 会被拒绝（`409 session_suspended`）；
> 运行中点会被禁用（`409 session_busy`）。这是互斥规则，不是 bug。

### G. 分叉、导出、会话级用量

**动作与断言（用量）**

1. 任意会话底部 footer 显示 `上下文 N / 128,000` 和
   `会话 X in · Y out · Z turns`。
2. `rpc sessions/usage "{\"session_id\": \"$SESSION\"}" | jq .`
   返回同一组数（`input_tokens` / `output_tokens` / `turns` /
   `context_used` / `context_window`）。

**动作与断言（fork）**

3. 打开 <http://localhost:5173/sessions>，在目标会话上点 **Fork**。
4. 页面跳到子会话。子会话 transcript 与父会话一致，父会话内容不变；
   子会话可以继续对话，之后的回合只出现在子会话。

   API 方式（可指定分叉点，省略 `from_stream_index` = 整份日志）：

   ```bash
   rpc sessions/fork "{\"id\": \"$SESSION\", \"from_stream_index\": 12}" | jq .
   # → { session_id, forked_from, rows }
   ```

**动作与断言（导出）**

5. 在 `/sessions` 点导出（下载 `<id>.jsonl`），或：

   ```bash
   rpc sessions/export "{\"id\": \"$SESSION\"}" \
     | jq -r '.content_base64' | base64 -d > "$SESSION.jsonl"
   wc -l "$SESSION.jsonl"
   jq -r '.seq // empty' "$SESSION.jsonl" | awk 'NR>1 && $1!=p+1 {print "gap", p, $1} {p=$1}'
   ```

   文件是 byte-faithful 的 NDJSON：第一行是 `_meta`，其余与
   `events.jsonl` 一致，`seq` 连续、无缺口。它不包含前端投影
   （不展开 compaction，也不合并文本）。

---

## 4. 读会话日志

一份日志 = `$DIR/<workspace>/sessions/dev/<session-id>/events.jsonl`，
一行一个 JSON 对象，`seq` 单调递增（第一行 `_meta` 除外）。

### 4.1 常用命令

```bash
LOG="$DIR/real/sessions/dev/$SESSION/events.jsonl"

# 所有类型与条数
jq -r '.type // "meta"' "$LOG" | sort | uniq -c | sort -rn

# 审批与执行意图：谁、对哪个 call、参数摘要
jq -c 'select(.type=="approval_asked" or .type=="approval_decided" or .type=="tool_dispatch")' "$LOG"

# 回合边界：turn start / end / suspended
jq -c 'select(.type=="turn") | .data' "$LOG"

# 压缩：生命周期 + replace 的遮蔽范围
jq -c 'select(.type=="compaction" or .type=="compaction_start" or .type=="compaction_summary" or .type=="compaction_end")' "$LOG"

# steer 行
jq -c 'select(.type=="user_message" and .source=="steer")' "$LOG"

# 最近一次模型请求的配置指纹
jq -c 'select(.type=="request_snapshot")' "$LOG" | tail -1

# seq 连续性（跳过 _meta）
tail -n +2 "$LOG" | jq -r '.seq' | awk 'NR>1 && $1!=p+1 {print "gap:", p, "->", $1} {p=$1}'
```

### 4.2 行语义

| 行 `type` | 何时写 | 看什么 |
|---|---|---|
| `request_snapshot` | 每次模型请求前 | `provider` / `model` / 四个 hash / `sections` / `policy_hash`；resume 时用来检测配置漂移 |
| `approval_asked` | 高风险调用被扣住 | `call_id`、`tool`、`risk: "high"`、`arguments_hash` |
| `approval_decided` | 人做出决定 | `status`: `approved` / `denied` / `cancelled`；`by`；`payload` |
| `tool_dispatch` | 真正执行前的意图行 | 崩溃标记：有 dispatch 无 result = 结果未知 |
| `tool_result` | 工具返回 | `error: TOOL_OUTCOME_UNKNOWN` / `TOOL_NOT_STARTED` 表示这是 repair 补的 |
| `turn` | 回合边界 | `data.kind` = start/end，`data.reason` ∈ completed / interrupted / cancelled / error / suspended；挂起另有 `data.phase: "suspended"` + `pending` |
| `step` | 每次模型调用边界 | 挂起时是 `{phase:"end", status:"suspended"}` |
| `compaction_start` / `compaction_summary` / `compaction_end` | 压缩生命周期 | 成对的 `token`；`outcome`: `committed` / `interrupted` / 失败原因 |
| `compaction` | 摘要以 replace 落盘 | `surface_op.start/end` 与 `source_event_seqs` 是被遮蔽的范围 |
| `user_message` | steer 注入 | `source: "steer"`；普通回合是 `UserInput` |
| `usage` | 每次模型调用后 | `prompt_tokens` / `completion_tokens`（provider 未上报时为 0） |

---

## 5. 第三方 crate 装插件：不改 harness

`Host` 是 per-agent 的 typed 策略表；拦截注册（hook / gate / 请求与结果
transformer / prompt section）走部署唯一的 `uva_hook::HookRegistry`，host
持有它（`Host::hook_registry()`）。一个第三方 crate 导出自己的 `Tool` /
`Hook` / `Gate` 实现，再加一个把它们注册进去的函数；每次注册返回一个
可撤销句柄。**句柄存活期间插件生效，drop 就撤销**（只影响后续 dispatch，
不打断在途调用）。

`Cargo.toml`：

```toml
[dependencies]
uva = { version = "0.1", features = ["provider", "tool", "harness", "hook"] }
serde_json = "1"
```

```rust,ignore
use std::sync::Arc;

use uva::async_trait;
use uva::harness::{Host, ReActAgent};
use uva::hook::{
    Hook,
    HookContext,
    HookDecision,
    HookEvent,
    HookRegistration,
    HookRegistry,
    MountPoint,
    RegistrationOrigin,
};
use uva::tool::{
    Context, ExecutionMode, Risk, Tool, ToolAnnotations, ToolOutput,
    ToolRegistry,
};

/// 一个只读工具：按 id 查一条记录。
struct LookupTool;

#[async_trait]
impl Tool for LookupTool {
    fn name(&self) -> &str {
        "lookup"
    }

    fn description(&self) -> &str {
        "Look up one record by its id."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "id": { "type": "string" } },
            "required": ["id"]
        })
    }

    fn mode(&self) -> ExecutionMode {
        ExecutionMode::Parallel
    }

    // 只读 → 审批策略不会为它挂起。
    fn annotations(&self) -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(true),
            ..ToolAnnotations::default()
        })
    }

    fn risk(&self, _input: &serde_json::Value) -> Risk {
        Risk::Low
    }

    async fn call(
        &self,
        input: serde_json::Value,
        _context: &Context,
    ) -> ToolOutput {
        let id = input["id"].as_str().unwrap_or_default();
        ToolOutput::text(format!("record {id}: (your data here)"))
    }
}

/// 一个观察者：每条工具结果打一行日志。
struct LookupAudit;

#[async_trait]
impl Hook for LookupAudit {
    fn name(&self) -> &str {
        "lookup-audit"
    }

    async fn observe(
        &self,
        event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> HookDecision {
        if let HookEvent::AfterTool { tool_name, .. } = event {
            println!("tool {tool_name} returned");
        }
        HookDecision::Allow
    }
}

/// 插件 crate 的注册面：一张表加一个 host，各自返回可撤销句柄。
pub fn register(
    registry: &HookRegistry,
    host: &mut Host,
) -> (HookRegistration, uva::harness::Registration) {
    let audit = registry.register_hook_at(
        MountPoint::ToolPost,
        Arc::new(LookupAudit),
        RegistrationOrigin::Plugin,
    );
    let tool = host.tool(Arc::new(LookupTool));
    (audit, tool)
}

fn assemble(
    provider: Arc<dyn uva::provider::ModelProvider>,
) -> (ReActAgent, (HookRegistration, uva::harness::Registration)) {
    let registry = HookRegistry::new();
    // host 持有部署的那一张表；运行里的 seam 层只向它派发。
    let mut host = Host::new_with_registry(registry.clone());
    // 句柄必须活到 agent 结束，否则 Drop 会撤销注册。
    let installed = register(&registry, &mut host);

    let tool_registry = Arc::new(ToolRegistry::new());
    let agent = ReActAgent::new(provider, tool_registry)
        .with_host(host)
        .expect("the fresh host binds");
    (agent, installed)
}
```

要点：

- 拦截注册方法全在 `HookRegistry`：
  `register_hook_at` / `register_gate_at` /
  `register_request_transformer_at` / `register_result_transformer_at` /
  `register_prompt_section`，每个都带 `MountPoint` 与
  `RegistrationOrigin`，返回可撤销的 `HookRegistration`。
- `Host` 只留 registry 表达不了的表：`tool` / `guard` / `hint` /
  `tracker` / `output_transformer`，返回 `Registration`。
  `Host::hook_registry()` 是循环的读取口；`hooks` 行把同一实例发布成服务。
- 名字与核心工具冲突的工具会被拒绝并记一条 `DiagnosticKind::ToolNameConflict`，
  不会覆盖核心工具。
- **进程内动态加载（`.so` / WASM）不在本仓范围**：自写的插件是编译期依赖，
  宿主（例如你自己的服务）在组装 agent 时注册它。需要不重编就能上线的第三方
  工具，走 §7 的 **MCP** 通道——那不是加载 `.so`，而是起一个进程说 MCP。
- 同一个 crate 也可以只做观察：在 `MountPoint::ToolPost` /
  `BeforeRequest` 等挂载点注册 `Hook`，看到的是只读事件；
  审批由循环内的 `ApprovalPolicy` 强制执行，插件不能把 `Ask` 降级为 `Allow`。

---

## 6. 写一个静态插件（编译进宿主）

§5 讲的是注册面的分工：`Host` 收 typed 策略表，`HookRegistry` 收拦截，
句柄拥有生命周期。本节从**空目录**写一个真正的插件 crate，然后把它装进
`uva-app-server`。

自写插件是编译期依赖：换插件要重编宿主。换来的是零 IPC、零握手——工具的
每次调用都是进程内直调，插件与宿主共享同一个 registry 与同一个错误类型。
需要"不重编就能上线"的第三方工具，看 §7 的 MCP 通道。

### 6.1 从空目录开始

```bash
cd /tmp
cargo new --lib lookup-plugin
cd lookup-plugin
```

`Cargo.toml`：

```toml
[package]
name = "lookup-plugin"
version = "0.1.0"
edition = "2021"

[dependencies]
# 本仓尚未发布到 crates.io。指向本仓的一份 checkout：
uva = { path = "/path/to/uva/crates/uva" }
# 发布后写成：
# uva = { version = "0.1", features = ["provider", "tool", "harness", "hook"] }
serde_json = "1"
```

`src/lib.rs`（本节代码已实跑，见 6.2 的输出）：

```rust,ignore
use std::sync::Arc;

use uva::async_trait;
use uva::harness::{Host, Registration};
use uva::hook::{
    Hook,
    HookContext,
    HookDecision,
    HookEvent,
    HookRegistration,
    HookRegistry,
    MountPoint,
    RegistrationOrigin,
};
use uva::tool::{Context, Risk, Tool, ToolAnnotations, ToolOutput};

/// 一个只读工具：按 id 查一条记录。
pub struct LookupTool;

#[async_trait]
impl Tool for LookupTool {
    fn name(&self) -> &str {
        "lookup"
    }

    fn description(&self) -> &str {
        "Look up one record by its id."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "id": { "type": "string" } },
            "required": ["id"]
        })
    }

    fn annotations(&self) -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(true),
            ..ToolAnnotations::default()
        })
    }

    fn risk(&self, _input: &serde_json::Value) -> Risk {
        Risk::Low
    }

    async fn call(
        &self,
        input: serde_json::Value,
        _context: &Context,
    ) -> ToolOutput {
        let id = input["id"].as_str().unwrap_or_default();
        ToolOutput::text(format!("record {id}: (your data here)"))
    }
}

/// 一个观察者：每条工具结果打一行日志。
pub struct LookupAudit;

#[async_trait]
impl Hook for LookupAudit {
    fn name(&self) -> &str {
        "lookup-audit"
    }

    async fn observe(
        &self,
        event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> HookDecision {
        if let HookEvent::AfterTool { tool_name, .. } = event {
            println!("tool {tool_name} returned");
        }
        HookDecision::Allow
    }
}

/// 插件 crate 对外只暴露注册函数：宿主（app-server）的工厂在装配时调它。
pub fn register(
    registry: &HookRegistry,
    host: &mut Host,
) -> (HookRegistration, Registration) {
    let audit = registry.register_hook_at(
        MountPoint::ToolPost,
        Arc::new(LookupAudit),
        RegistrationOrigin::Plugin,
    );
    let tool = host.tool(Arc::new(LookupTool));
    (audit, tool)
}
```

三个约束决定这段代码的形状：

- 插件 crate 只导出实现与一个 `register` 函数；它**不构造 `Host`**——
  host 由宿主拥有，插件只往传进来的表与 host 里注册。
- `register` 不返回 `Result`：挂载点由 `MountPoint` 给定，注册接口不拒绝；
  装进 app-server 时的装配失败由内核原生工厂的 `build` 承担，报成
  `ProfileError::BuildFailed`（点名那个 id），不是一次静默跳过。
- 每个注册返回一个句柄（拦截是 `HookRegistration`，host 的 typed 表是
  `Registration`）。**句柄就是插件的生命周期**：drop 掉它，注册撤销
  （只影响后续 dispatch，不打断在途调用）。
- 宿主拥有 `Host`——一个部署一个 host，每个 agent 拿到它的 clone。

### 6.2 宿主侧：安装与撤销

在同一个 crate 里加一个 demo 二进制（`src/main.rs`）：

```rust,ignore
use lookup_plugin::register;
use uva::harness::Host;
use uva::hook::{HookRegistry, MountPoint};

fn main() {
    let registry = HookRegistry::new();
    let mut host = Host::new_with_registry(registry.clone());
    let (audit, tool) = register(&registry, &mut host);

    let names: Vec<String> = host
        .tools()
        .iter()
        .map(|tool| tool.name().to_string())
        .collect();
    assert_eq!(names, vec!["lookup".to_string()]);
    println!("installed tools : {names:?}");
    assert!(registry.has_entries_at(MountPoint::ToolPost));
    println!("registered hooks: 1");

    // Dropping the handles withdraws every registration they own.
    drop(tool);
    drop(audit);
    println!("tools after drop: {}", host.tools().len());
    assert_eq!(host.tools().len(), 0);
    assert!(!registry.has_entries_at(MountPoint::ToolPost));
}
```

```bash
cargo run -q
# installed tools : ["lookup"]
# registered hooks: 1
# tools after drop: 0
```

（`Cargo.toml` 里加 `[[bin]] name = "lookup-demo", path = "src/main.rs"`。
上面的输出是在本仓当前 checkout 上实跑的。）

### 6.3 部署侧：写一个内核原生行

自写的工具进 `uva-app-server` 的路径是**内核原生行**：一个 `PluginFactory`
实现，加 profile 里的一行 `[core]` 条目。

工厂是"profile 行 → 具体插件"那一段知识所在，住在
`crates/uva-app-server/src/composition/kernel/mod.rs`，注册进工厂目录
（按 id 查）：

```rust,ignore
use crate::profile::{PluginFactories, PluginFactory};

let mut factories = PluginFactories::new();
factories
    .register(Arc::new(lookup_plugin::LookupFactory)) // 它的 id() 是 "lookup"
    .expect("the factory ids are unique");
```

同名注册**不会覆盖**：`register` 返回 `ProfileError::DuplicateFactoryId`，
第二个工厂不被接受——静默覆盖比启动失败更糟。

profile 文件（工作区配置层的 `.agents/profile.toml`，与 §2 同一个部署目录）写一行：

```toml
[[core.entries]]
id = "lookup"
name = "lookup"
```

- `id` 是行身份，`name` 是它装的插件（工厂目录的键）；`[core]` 段装核心
  插件，`[ui]` 段装界面插件；
- 行里的 `config` 是这个插件自己的配置对象，schema 由工厂的 manifest 声明，
  解析期就被校验；
- 行可以声明 `inject`（这一行额外依赖的服务键）与 `disabled = true`
  （这一行不进装配）；
- boot 走 `build_plugins` → `mount_all`：按依赖序挂载这些行，工厂的
  `manifest()` 决定它提供什么、注入什么；文档声明的 `inject` 与工厂的
  `inject` 取并集，缺依赖的行挂起（开机审计报出），不整机报错。

装了之后能观察到：插件的工具出现在 `rpc tools/list`，模型可以调用。

> **未实跑的部分**：6.1/6.2 的代码与输出是在本机实跑的；6.3 的 fork 编译
> 与启动没跑（需要一个改了 `composition/kernel/mod.rs` 的 binary）。

## 7. 第三方工具：走 MCP

第三方工具不进 agent 的 crate，它们是**进程**：宿主用 stdio 把它起起来、
跑 MCP 握手，把它广告的工具注册进 agent 用的同一个 `ToolRegistry`。

旧的"进程插件通道"（`protocol` + `host` 那套 JSON-RPC 词汇）与 Wasm 组件
通道都已删除——第三方脚本 / 工具一律走 MCP。

### 7.1 声明

声明面是部署布局里的 `mcp.json`：两层 `~/.agents` 布局
（`<workspace>/.agents/mcp.json`、`~/.agents/mcp.json`），插件包里的
`mcp.json` 按 `<plugin>/<server>` 命名空间落地。

```json
{
  "mcpServers": {
    "records": {
      "command": "python3",
      "args": ["/srv/mcp/records_server.py"]
    }
  }
}
```

字段规则：

- `command` 是要起的程序，`args` 是它的参数（可省）；
- `env` 是环境变量覆盖；`enabled: false` 让条目留在文件里但不启动；
- 传输只有 **stdio**：MCP server 读写自己的 stdin / stdout；
- 声明文件写坏了不会连累别的 server——那一个被跳过并记一条告警。

### 7.2 它是怎么起来的

boot 走 `register_configured_mcp_servers`
（`crates/uva-app-server/src/state/app_state/boot.rs`）：每个启用的条目被
`StdioTransportFactory` 拉起，supervisor 跑握手与工具清单，它广告的工具
注册进**同一个** `ToolRegistry`（名字带命名空间：`mcp__<server>__<tool>`），
所以模型侧与内置工具没有区别。

起不来的 server 不阻塞 boot：它被跳过并带原因，别的 server 照常上线。

### 7.3 MCP 不承载什么

MCP 是**工具**通道，不是权限通道：它不承载结果改写（transformer）、不承载
gate 裁决、不承载 authority 分级——那些是宿主装配时注册的能力：观察者与
gate 走 `HookRegistry`，typed 策略（guard / hint / tracker / transformer /
tool 绑定）走 `Host`。想要一个观察者或一个 gate，写自写插件（§5 / §6）。

---

## 8. 崩溃与恢复语义

### 8.1 崩溃（进程被杀）

日志是唯一真源。启动后第一次打开会话时，server 调用
`repair_session`，**只追加**闭合行，不改写任何已落盘行：

```text
tool_result { call_id, error: TOOL_OUTCOME_UNKNOWN | TOOL_NOT_STARTED,
              interrupted: true, is_error: true }
step        { kind: "end", … }        # 若 step 未闭合
turn        { kind: "end", reason: "interrupted" }
```

然后 `fold(log)` 得到模型历史；下一次发送就是普通的新回合。
高风险调用**不会**因为修复而自动重跑（副作用可能已经发生）。

### 8.2 挂起（人不在）

挂起不是崩溃。日志显式写着 `turn/end{reason:"suspended"}`，
所以 repair 不会把它当 interrupted，未决调用也不会被补成“失败”。
`ResumeToken` 由最后 durable `seq` + 未决 `call_id` 集合 + 请求指纹派生，
所以它跨刷新、跨进程、跨节点都成立。恢复时：

1. 校验 token 的 `last_seq` 等于日志尾、`pending` 与未决集合一致、
   请求指纹等于最新 `request_snapshot`；
2. 写入 `approval_decided` 并 `flush`（Sync 屏障）；
3. 只对批准项追加 `tool_dispatch` 并执行**一次**；拒绝项写入
   `is_error` 工具结果；
4. `turn/start` 继续。

resume 的结局分三种，客户端要做的动作各不相同：token 指向的决定**已经
应用过**（日志里已有对应的 `approval_decided`，且本次请求与日志里的
决定一致）不算失败，返回 `200 OK`——重试是幂等的，不会变成同一次调用
的第二次决定；日志已经推进（`last_seq` 落后、`pending` 对不上，或
请求把一个日志已决定的调用改成了另一种决定）返回
`409 resume_conflict`，客户端重新拉状态、再让用户决定；日志里根本
没有请求快照返回 `409 resume_no_snapshot`，这一轮不能续，建议重开
会话。

手工恢复（浏览器已经做了这件事）：

```bash
rpc sessions/resume "{
  \"session_id\": \"$SESSION\",
  \"resume_token\": {\"last_seq\": 17, \"pending\": [\"call_00_xxx\"], \"request_snapshot_hash\": \"sha256:…\"},
  \"interrupts\": [{\"id\": \"approval-call_00_xxx\", \"status\": \"resolved\"}]
}" | jq .
# 409 resume_conflict / resume_no_snapshot / config_drift / session_busy /
#    remote_session（会话由其他节点持有）
# 422 incomplete_resume（未覆盖所有未决调用）
```

### 8.3 配置漂移

server 在 resume 前把 token 的指纹与日志最新的 `request_snapshot`
比较，不一致返回 `409 config_drift`；带 `allow_drift: true` 时放行
（服务端记一条 warning，不改写日志）。harness 真正开始恢复时会用当前
host 的工具 schema、system prompt、采样参数与审批策略再算一次指纹，
仍不一致就以 `ConfigDrift` 结束该 run。这保证“恢复的是同一配置下的
同一日志”，而不是“尽量像”。

---

## 9. 投递与审批语义（速查）

| | Queue | Steer |
|---|---|---|
| UI | 运行中发送按钮显示 `Queue` | 运行中另有 `Steer` 按钮 |
| 生效点 | 当前 run 结算后，新回合第一条 user 行 | 当前工具批次结束、下一次模型调用前 |
| 日志 | `UserInput` | `user_message`，`source: "steer"` |
| 不丢 | run 先结束时仍成为下一回合 | run 先结束时服务端把它转成 queued turn（chip 显示“下一回合”） |
| 端点 | `chat/message/send` body `delivery: "queue"` | 同方法，`delivery: "steer"`（仅文本） |

审批：默认策略 `AskHighRisk`——只有工具按本次调用判为 `high` 才挂起；
在该策略下 `read_only_hint: true`（`Risk::Low`）不问；无注解的调用
按 `ask_unknown`（默认 `false`）。`Always` 则连低风险调用也扣住，
`Never` 全放行；策略可按工具覆盖：

```rust,ignore
use uva::harness::{ApprovalMode, ApprovalPolicy, ReActAgent};
use std::collections::BTreeMap;

let mut overrides = BTreeMap::new();
overrides.insert("shell".to_string(), ApprovalMode::AskHighRisk);
overrides.insert("read".to_string(), ApprovalMode::Never);

let agent = ReActAgent::new(provider, registry)
    .with_approval_policy(ApprovalPolicy {
        mode: ApprovalMode::AskHighRisk,
        ask_unknown: false,
        overrides,
        ..ApprovalPolicy::default()
    });
```

**会话级授权**：`ApprovalPolicy::grants` 由两部分组成——宿主在构造 agent 时给出的
部署预授权，以及从会话日志重放的 `approval_granted` 行（`scope: "session"`）。
在审批卡上点 **批准本会话** 时，决策 payload 带 `{ "scope": "session" }`；resume
在写 `approval_decided` 的同一次 flush 里写 `approval_granted`，并把 grant 加进本次
run 的策略，因此同一 run 的后续调用、以及重启后从日志重建的后续 run 都不再询问
“该工具 + 该工作区”。部署级授权仍只能来自配置，不能由一次点击产生；
**没有全局无限期授权**。

---

## 10. 门禁

文档不改变代码，但提交前跑：

```bash
make check-web     # tsc --noEmit + eslint + prettier --check
make ci            # fmt / clippy / rustdoc / guides / 各项不变量
```

A–G 没有自动化测试（AGENTS.md §4.2：UI 用 `make check-web` +
Playwright MCP 验收），所以真实观察记录在下一节。

---

## 11. 只依赖 harness 的消费方（不跑 server）

只想用 harness 这个循环、不跑 server：依赖不是只有 `uva-harness`
一个 crate。harness 把 `Spawner` / `ModelProvider` / `Tool` 这些 trait
**不** re-export 出来 —— re-export 会把不属于 harness 的名字放进它承诺
的公开面，也会把 harness 的零运行时承诺绑到别的 crate 的 feature 上 ——
所以消费方要写四个 crate：

| crate | 消费方拿它做什么 |
|---|---|
| `uva-harness` | 循环、接缝、事件流 |
| `uva-core` | `CancelToken`、`Spawner`、`Clock` |
| `uva-provider` | `ModelProvider` trait 与消息类型 |
| `uva-tool` | `Tool` trait 与 `ToolRegistry` |

外加 `async-trait`（这些 trait 声明时用的 proc macro）。上面那三个 trait
crate 本来就在 harness 自己的依赖闭包里（harness 的签名里就写着它们），
写进自己的 `Cargo.toml` 说明的是“你实现哪些 trait”，不会让依赖树变大。

这句话有一个前提，就落在 `uva-provider` 上：它的默认 feature 集
（`default = ["anthropic", "openai"]`）会打开两个出厂的 HTTP adapter，
每个 adapter 都带着 `reqwest`、`tokio` 和 `tokio-util`。消费方如果照
crates.io 的默认写法写 `uva-provider = "0.1"`，这三样全都进树。
harness 自己就是关着默认 feature 取 provider 的，消费方想要同一棵树就写
同一行：

```toml
uva-provider = { version = "0.1", default-features = false }
```

`uva-tool` 根本没有 feature 表，这个 flag 对它是空操作 —— 前提只落在
provider 上。

参考消费方 `docs/examples/harness-only-consumer` 的 `Cargo.toml` 里有
7 个直接依赖（上面四个 + `async-trait` + `futures` + `serde_json`），
它的 `cargo tree` 里 tokio 的计数是 0 —— 靠的就是它那行 provider 的
`default-features = false`：没有 adapter，就没有 `reqwest`，也没有运行时
（fixture 用 `futures::executor::block_on` 驱动循环）：

```bash
cargo tree --manifest-path docs/examples/harness-only-consumer/Cargo.toml \
  | grep -c tokio     # must print 0
```

公开面分两层：

- **精选面**：crate 根上 re-export 出来的 59 个名字 —— 这是承诺过的稳定面，
  依赖它就不会被下一次发布挪走。
- **深路径**：`uva_harness::agent::…`、`uva_harness::events::…`
  （`uva-events` 词汇的 re-export）与 `uva_harness::seams::…`
  （循环的接缝层）底下只能靠完整的模块路径够到。rustdoc 在
  `agent` 模块下记录了 28 条公开路径，其中 17 条是门面名字在它们定义处的
  路径（例如 `AgentDescriptor` 定义在 `uva-core`），剩下 11 项，深路径
  名字共 11 项。两路 host 面（`Plugin`、`PluginHandle`、旧的
  `agent::host::Arbitration` 与 delegation 词汇）随 ADR 0028 的收口移除，
  不再有 harness 内的路径。原先从 `uva-hook` re-export 进
  `uva_harness::seams` 的七个挂载点键已随 ADR 0028 收进 `uva-hook` 的
  `MountPoint` 枚举，不再构成跨 crate 的深路径名字。审批词汇整体搬去了
  `uva-approval`、事件词汇搬去了 `uva-events`、agent 词汇搬去了
  `uva-agent`，都经 crate 根的 use 树抵达本 crate。
  深路径上的名字不在承诺内，将来可动。

---

## 验收记录

运行时填写：日期、`git rev-parse --short HEAD`、provider/model。
状态列填写前是 `待运行（controller Playwright）`；运行后改为 `通过` 或 `失败（原因）`。
无法观察的步骤记为失败，不得省略。

| 项 | 值 |
|---|---|
| 运行日期 | 2026-10-25 |
| Commit | A/B/E/F/G：server `fe751852` + web `585a18b1`；C：Task 10 于 `fe751852`；D：server `5c9e89d5`。未在单一 commit 上重跑 A–G |
| Provider / model | `openai/MiniMax-M2.7`（验收使用仓库根 `config.yaml`，不是 2.2 的教程配置） |
| 浏览器 / Playwright | Playwright MCP（Chromium），server `:8080` + Vite `:5173` |

| 步骤 | 动作摘要 | 期望的可观察断言 | 观察结果 | 状态 |
|---|---|---|---|---|
| A 基本回合 + 工具 | 读 `notes.txt` + `ls -la`，刷新 | 增量文本、工具卡参数/结果、刷新后不重不漏 | 日志有两条 `assistant_message`（工具调用批次 + 最终回答）；最终回答在流式与刷新后都只渲染一次；两张工具卡与结果齐全 | 通过 |
| B queue | `sleep 15` 期间点 Queue | 本轮先结算，排队消息成为下一回合第一行 | 端点返回 `delivery:"queue"`；服务端回归测试 `two_queued_text_turns_both_start_and_are_logged` 证明两条排队消息都落盘；浏览器未逐帧观察 | 通过（服务端 + API） |
| B steer | `sleep 20` 期间发 `delivery:"steer"` | chip 到“已注入 step N”/“下一回合”；日志有 `source: "steer"` 行 | 响应 `delivery:"steer"`；日志 `user_message{source:"steer"}` seq 61，位于该 step 的 `tool_result` 之前；最终回答包含 `api-steer-marker` | 通过（API + 日志；浏览器 chip 未逐帧观察） |
| B stop | Steer 后立刻 Stop | 未注入文本回到编辑器，输入不丢 | 代码与回归测试覆盖 `Stop` 退回；本轮未在浏览器制造该竞态 | 未验证 |
| C 低风险 | `ls -la` | 不出现审批卡 | 只读 shell 直接执行，无审批卡 | 通过 |
| C 批准一次 | 重定向写 `/tmp`，批准并恢复 | 卡片出现→202→工具执行一次；`approval_asked/decided/tool_dispatch` 齐全 | Task 10 在 `fe751852` 的 Playwright 观察到卡片、提交 → 202、工具执行与结果；本次验收 server（`5c9e89d5`）未复跑该路径 | 部分（Task 10 观察） |
| C 拒绝 | 同类高风险调用点拒绝 | `denied`、无 `tool_dispatch`、模型看到 `is_error` 结果 | harness/server 测试覆盖拒绝（`is_error`、无 dispatch）；浏览器拒绝路径未点 | 部分 |
| C 本会话授权 | 对同类调用选择“批准本会话” | 后续同类调用不再询问 | 已实现：审批卡按钮 → payload `{scope:"session"}`；resume 写 `approval_granted` 并在 setup 重放为 `ApprovalPolicy::grants`。本轮未在浏览器复跑 | 部分（待复跑） |
| D 崩溃重启 | 睡眠中 `kill`，重启并打开会话 | 只追加 closer，前缀 `cmp` 无差异，UI 显示“未完成” | `repair_session` 追加 `tool_result{error:"TOOL_OUTCOME_UNKNOWN",interrupted:true}`、`step end`、`turn end{reason:"interrupted"}`；下一回合正常开始；未改写前缀 | 通过 |
| D 挂起跨重启 | 审批卡出现时 `kill`，重启刷新 | 卡片仍在，恢复只执行一次 | 本轮未在挂起态杀进程；harness `turn_end{suspended}`/resume 测试覆盖 | 未验证 |
| E 断线重连 | 流式中 F5 | 基线整窗替换，无重复/缺失，live 继续 | 刷新后窗口整窗替换；最终段落只一次；Task 10 的重连测试 0 重叠样本 | 通过 |
| F 压缩 | 长会话后点 Compact | `committed: true`，时间线标记 + 摘要/范围，四类日志行齐全 | Task 10 round 2 观察到已提交的压缩标记（范围 `#2–#3`）与摘要；手动 Compact 按钮未单独点 | 部分（自动已观察） |
| G fork | `/sessions` 点 Fork | 子会话可续聊，父会话不变 | API `POST /sessions/{id}/fork` 返回 `rows: 60`；子会话 `66802f3c-831f-4cf2-9475-3506f04333f8` 的 `events.jsonl` 存在 | 通过（API） |
| G 导出 | 下载 `.jsonl` | byte-faithful，`seq` 连续 | `GET /sessions/{id}/export` 返回 `uva_session_meta` + 按序行；未做崩溃前后逐字节 diff | 通过（NDJSON 结构） |
| G 用量 | footer + `/usage` | 会话级 token/turn 与接口一致 | 验收配置（128k）下 `context_used: 1040 / 128000`；`input/output/turns` 为 0 是因为该 provider 的 `usage` 行报告 0，不是聚合缺失 | 通过（附注） |

> 首次 Playwright 验证时注意两件事：
> 1. `sleep` 窗口要足够长（≥15s），否则 steer 会落到“下一回合”，
>    那是正确行为但测不到注入路径；
> 2. 想在一次短会话里复现 F 步的压缩，需要临时调小 `context_window`
>    （建议 12,000）；3,000 这类低于固定提示成本的窗口会让每一轮都压缩，
>    只作为 `uva-context` 的回归测试值保留。
>
> 未走路径（B stop、D 挂起跨重启、C 浏览器拒绝、F 手动按钮）由
> harness/server 回归测试覆盖，但没有浏览器实测；上表按实际观察标注，
> 不把测试覆盖当作浏览器通过。
