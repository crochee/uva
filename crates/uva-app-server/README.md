# uva-app-server

参考 agent 运行时，一个二进制两种模式：`core` 是 stdio 上的无头
JSON-RPC 2.0 内核，`serve` 是内核前面的 WebSocket 网关。内核持有全部
agent 逻辑；网关不持有任何逻辑——只双向泵帧并看护子进程。

这是工作区**唯一**的应用 crate——`tokio` / `axum` 允许出现在公共签名
的地方（stdio 二进制 `uva-mcp-server` 同款豁免）；其余 lib crate
必须保持运行时中立（`make check-public-api-runtime`）。

> 公开面与模块文档见 [`src/lib.rs`](src/lib.rs)；网关职责见
> [`src/gateway.rs`](src/gateway.rs)；方法表与分帧见 [`src/rpc/`](src/rpc/)。

## 两种模式

```sh
# core（缺省子命令）：无头内核，stdin/stdout 上的 JSON-RPC 2.0
uva-app-server core --directory /srv/uva

# serve：网关——spawn 一个 core 子进程并服务它
uva-app-server serve --port 8080 --web-dir ./dist
```

不带子命令即 `core`；人类入口是 `serve`。两个子命令共享 `--directory`
（工作区根，会话 / 调度 / 工作流 / eval / attachment 的运行时状态都在
这里）与 `--config`（部署配置，见下）。

## core：stdio JSON-RPC 2.0

- **分帧**：Content-Length 帧，`Content-Length: <n>\r\n\r\n` 加恰好 `n`
  字节的 JSON；header 名大小写不敏感，额外 header 行被容忍。请求 / 响应 /
  通知同构：

  ```text
  Content-Length: 59\r\n\r\n{"jsonrpc":"2.0","id":1,"method":"health/ping","params":{}}
  Content-Length: 63\r\n\r\n{"jsonrpc":"2.0","id":1,"result":{"ok":true,"version":"0.1.0"}}
  Content-Length: <n>\r\n\r\n{"jsonrpc":"2.0","method":"session/event","params":{...}}
  ```

  `id` 可为字符串或数字并原样回显；没有 `id` 的帧是通知，处理后不回答。
  长度是 payload 的**字节**数：切分与粘连都按字节数解码，不按换行。
- **stdout 只有协议**：日志一律写 stderr（`LogWriter::Stderr`）——
  `core` 模式下 stdout 上一个非帧字节会被对端当坏帧。
- **`ready` 通知**：boot 完成后发一条
  `{"jsonrpc":"2.0","method":"ready","params":{"version":...,"workspace_root":...}}`，
  网关解析该帧、只认 `method == "ready"` 判定就绪（`/readyz` 同样等它）。
- **并发**：泵每帧 spawn 一个任务，响应可能乱序到达，客户端按 `id` 配对。
- **错误形状**：应用失败是 `error.code = -32000` + `data.code` 机器码
  （`not_found` / `validation` / `follow_cursor_truncated` …）；协议
  失败用标准 JSON-RPC 码。线上没有 HTTP 状态。

## serve：网关

网关 spawn **一个** `<current_exe> core` 子进程，自己承担 App Server
的四件事：

1. **协议转换**——浏览器的一条 WebSocket 文本消息是一个 JSON-RPC
   对象；网关把它包成一条 Content-Length 帧写进 core 的 stdin，把 core
   读出的每帧 payload 原样转发回浏览器。两个方向都不改写 JSON，
   `Content-Length` header 只存在于 stdio 一侧，浏览器永远看不到。
2. **线程（连接）管理**——每个连接记住自己 follow 的会话集合；断线
   重连的客户端对每个会话重发 `sessions/follow`，core 回基线，时间线
   自行重建。
3. **事件分发**——`session/event` / `anthropic/messages/event` 通知按
   `params.session_id` 路由给 follow 该会话的连接（不广播）；响应按
   `id` 路由回发起请求的连接。连接以 `sessions/follow` 订阅、
   `session/unfollow` 或断开退订。
4. **审批中断**——core 把 durable suspension 投影为 `approval/request`
   **通知**：`interrupts` 事件（以及基线里的 `pending_interrupts`）是权威
   投影来源，每个 held call 发一条（`params.approval_id` +
   `params.session_id` + `call_id` / `tool` / `risk` /
   `arguments_hash` / `agent`），session 作用域，与 `session/event` 同样
   按订阅路由。客户端用普通请求 `approval/respond` 回传决策，网关原样
   转发；core 把它映射到既有的 `sessions/resume` + `ResumeToken`。
   `approval_id` 是 UI 关联标识，不是 JSON-RPC `id`，也不是唯一真相：
   决策落成 `approval_decided` 行，超时默认拒绝、重启后仍可恢复都由
   resume 路径保证。

网关**不做**的事：不聚合 / 改写 / 缓存事件载荷，不持有会话内容，也不
生成或丢弃审批决策——durable 真源是 core 的会话日志；它只读帧里的
`id`、`params.session_id` 和订阅管理的 `method` 名，其余一概不透明。

**看护**：core 死亡即重启，首次退避 1s，之后翻倍封顶 5s。就绪只认解析后
`method == "ready"` 的通知（不做字符串匹配）；新 core 就绪前 `/readyz`
保持未就绪。跨重启存活的连接 socket 不断，但订阅与在途请求随旧 core
一起清空（在途请求以原 `id` 收到 `core_unavailable`），由客户端的重
follow 重建。

### HTTP 表面

| 路径 | 作用 |
| :--- | :--- |
| `GET /rpc` | WebSocket，JSON-RPC 帧的通道（一条文本消息一帧） |
| `GET /livez` / `GET /readyz` | 探针（`readyz` 还要求 core 已发 `ready`） |
| `GET /version` | 构建身份 |
| `GET /metrics` | Prometheus 文本 exposition（网关的 HTTP 计数 + 内核的工具计数，后者经 `metrics/gather` 合流） |
| 其余一切 | Web 前端的静态文件（fallback） |

静态前端两个来源：`--web-dir <path>` 指向已构建的 `dist`（优先），或
`embed-web` feature 用 rust-embed 把 `uva-web/dist` 烤进二进制。
旗标：`--bind`（env `UVA_HOST`，默认 `127.0.0.1`）、`--port`
（env `UVA_PORT`，默认 `8080`）。

## RPC 方法

一个方法表，`domain/verb` 命名（LSP / MCP / gRPC 的斜杠风格），60 余个
方法分属：`sessions/*`（含 `sessions/follow` / `sessions/resume` /
`session/unfollow`）、`approval/respond`（审批回复，映射到既有的
`sessions/resume` + `ResumeToken`）、`chat/*`、`agents/*`、`skills/*`、
`tools/*`、`schedules/*`、`tasks/*`、`evals/*`、
`attachments/*`、`search/all`（六域统一检索）、`health/*`、
`models/list`、`metrics/gather`（本进程的 Prometheus exposition，供网关
合流）与 Anthropic 兼容的 `anthropic/messages`。方法表见
[`src/rpc/methods/`](src/rpc/methods/)；每个方法名在
[`src/rpc/methods/names.rs`](src/rpc/methods/names.rs) 里只写一次，
路由按那些常量注册，客户端（终端、浏览器门禁）读同一张表。

## `~/.agents/` 布局

定义（skills / agents / tasks / MCP 服务器声明）住在双层 home，由
`uva-core::layout`（std-only）解析：

- **全局层**：`~/.agents/`（规范）与 `~/.uva/`（legacy，只读，
  仅 skills 域读取）；
- **项目层**：`<workspace-root>/.agents/`。

每层可以直接放定义，也可以放**插件包**：一个 `plugins/<name>/` 目录，
带 `plugin.json` manifest 与 `skills/` / `agents/` / `tasks/` / `mcp.json`
的任意子集——插件包是同构子树，只是定义的另一个来源。同名定义的优先级
四源收敛：项目直定义 > 项目插件包 > 全局直定义 > 全局插件包，先到先得。

- agents：`agents/<name>/agent.md`（frontmatter + 正文）+ 可选
  `config.json`，由 `uva-agents` 读写；
- skills：`skills/<name>/SKILL.md`；
- tasks：`tasks/<name>/task.md`（`uva-scheduler::task_file`）；
- MCP：`mcp.json`（`mcpServers` 映射，仅 stdio 服务器），插件包里的
  服务器以 `<plugin>/<server>` 命名空间落地。

运行时状态（sessions / schedules / evals / attachments）不在
这里——它们留在 workspace root，是部署的 durable 真源。

## 第三方工具：走 MCP server

动态插件通道（进程插件 / Wasm 组件）已删除。第三方工具与脚本以
**MCP server** 的形式接入：在一份 `mcp.json` 里按 `command` + `args`
声明要拉起的服务器（Node / Python 都有现成 SDK），服务器走 stdio 说话，
它发布的工具注册进 agent 用的同一个注册表：

```json
{
  "mcpServers": {
    "files": { "command": "node", "args": ["tools/files-mcp.js"] }
  }
}
```

文件位置与其它定义同一套双层布局：`<workspace>/.agents/mcp.json`、
`~/.agents/mcp.json`，插件包里的 `<plugin>/mcp.json` 按
`<plugin>/<server>` 命名空间落地。

## 配置

`--config <path>`（`.yaml` / `.yml` / `.toml` 均可）一次性读入
`providers`、`agents`、`[tools]`、`[operations]` 各段；
缺省时依次回退 `<directory>/config.toml` → `<directory>/.uva/
config.toml` → `~/.agents/config.toml` → 内置默认，provider 再回退环境
变量（`OPENAI_API_KEY` / `ANTHROPIC_API_KEY`）。服务器不做认证 / CORS：
单用户本地运行，`user_id = "dev"`。

## 用法

```sh
cargo run -p uva-app-server -- serve   # 网关（:8080）
cargo run -p uva-app-server -- core    # 无头内核（stdin 上的 JSON-RPC）
cargo run --example observability -p uva-app-server
```

### Profile 装配

插件装配由 Profile TOML 负责，`config.yaml` 只保留部署基础项。层序为
空 → bundle patch → profile 文档 → home patch → CLI overlay，后层按
`id` 整行替换 `config`（不做深合并）：

```sh
cargo run -p uva-app-server -- core \
  --profile ./profile.toml --bundle ./bundle.toml --patch ./ops.toml
cargo run -p uva-app-server -- --profile ./profile.toml --dump-config
```

`--bundle`（及 home 级 patch）是 patch 文档：`[[replace]]` 改已有行、
`[[insert]]` 追加、`disabled` 过滤。home 级 patch 读取
`~/.agents/profile.toml`（或 `UVA_HOME_PROFILE`），文件不存在时跳过。
profile 文档层缺省落在工作区的配置层 `<workspace>/.agents/profile.toml`
—— 与 provider 回退输入 `<workspace>/.agents/config.toml`、种子技能
`<workspace>/.agents/skills/` 同一层；`--profile` 显式指定时以它为准，
设置面也写回同一路径。文档层是设置面的生成物（整篇重写），不是仓库里
该提交的配置源。
`--dump-config` 打印确定性 JSON 后以 0 退出；`provides` 来自已注册工厂的
manifest，`inject` 是**有效边**（工厂 manifest 的 `inject` ∪ 行自己声明的
`inject`，同名键以行为准），工厂尚未注册的行显式输出空数组（`ui` 段同样
输出空数组，因为 UI 行没有核心工厂）。
`config.yaml` 的 provider 段不再被 boot 直接翻译成插件配置：用
`uva-app-server translate --config config.yaml --out .agents/profile.toml`
写出一份**完整**的 profile 文档 —— 内置插件集合 + 迁移后的两行：
`[providers]`（连同 `cache` / `resilience` / `budget`）进 `model-adapter`
行，默认 agent 的 strategy / tool 列表 / compaction 进 `agent-loop` 行。
文档层是权威装配，所以迁移必须带上整个装配：只写 `model-adapter` 一行
的文档会让部署丢掉工具注册表与 agent loop（boot 在 `tools` 服务缺失处
失败）。内联 `api_key` 不写进文档，`api_key_env` 指向
`UVA_PROVIDER_API_KEY_<NAME>`，报告逐条列出要 export 的变量。
其余段（`agents` 表、`operations`、`tools`、`cluster` …）仍由
`ServerConfig::load` 直读。provider 的回退输入是
`<workspace>/.agents/config.toml`（TOML 形状）。

显式 `[core]` 行是权威装配：boot 通过 `build_plugins` →
`mount_all_with` 挂载这些行，省略某个内置行即不装该插件，未知 `name`
直接失败；必需行（`agent-loop` / `model-adapter`）挂着缺服务时 boot 直接
失败（缺依赖不整机报错，但这两行不允许静默停摆）。空的
profile 解析结果挂载内置原生集合。`hmr = true` 时核心每秒轮询 profile 各层，变更后
用 `diff_profiles` 只卸载 / 重挂 `added` / `removed` / `remounted` 的行；
volatile-only 变更不重挂，已开始的 turn 继续用其已解析的服务句柄。
`hmr = false` 的变更只记录「需要重启」。

### 行形状与事务性变更

每一行都是一份 **manifest**（dsh entry 模型）：行自己声明装什么、依赖谁、
开不开。

```toml
[[core.entries]]
id = "agent-loop"
name = "agent-loop"
inject = ["llm.default", "tools", "sessions", "events"]

[[core.entries]]
id = "batch"
group = true
config = [
  { id = "skills", name = "skills" },
]

[[core.entries]]
id = "skills"
name = "skills"
disabled = false
```

- `id` 是行身份，`name` 是它装的插件（工厂目录的键）。
- `disabled = true` 的行不进装配；组行（`group = true`）的 `disabled`
  禁用其全部后代，组自己的子行写在 `config` 数组里。
- `inject` 是这一行**额外**声明的依赖：有效边 = 插件 manifest 自己的
  `inject` ∪ 行的 `inject`（同名键以行为准），文档只能加、不能挖掉插件的
  固有最小依赖。
- `isolate = { tools = true }` 让该行在自己的服务作用域里读 `tools`；
  `isolate = { tools = "batch" }` 让同标签的行共享一个作用域。
  `intercept = { llm = {...} }` 给服务携带拦截配置，沿组子树组合。

缺依赖**不报错**：该行的 fiber 挂起（PENDING），开机审计逐行列出挂起的行
与它还缺的服务名；只有 `agent-loop` 与 `model-adapter` 挂起才致命（没有
模型循环的整机不许「看起来正常」）。行因失去 provider 而不再能运行时，
文档把它写回 `disabled = true`（dsh 的自禁用写回）。

显式写入（JSON-RPC `profile/set` 与自管 MCP 的 `profile` 工具）走同一条
`AppState::profile_apply` 事务：内存重解析整层栈 → `overridden` 检测（home
patch / CLI overlay 拥有该字段时不写）→ 试装（build 在 unload 之前，失败
不动树）→ 原子 `rename` 替换文档层 → 审计并广播无 `session_id` 的
`profile/changed`。结果是 `applied` / `restart-required`（`mcp` 行变更，
远端监督需进程重启）/ `overridden` / `failed`。`hmr = false` 的部署上显式
写入仍然生效（`apply_resolved` 不看该标志）；文件监听入口
`reload_profile` 保留旧的「只报需要重启」语义。写入会重排 `profile.toml`
并丢失操作员注释（文档是设置面的生成物）。

`[ui]` 段里的行必须在 `ui.` 命名空间；行的 `config` 可以
声明插槽（`slots = [{ id = "chat.sidebar", cardinality = "many" }]`，
`cardinality` 取 `one` / `many`，缺省 `many`）并向已声明的插槽注册组件
（`components = [{ slot, kind, props }]`）。插槽 id 与宿主无关：浏览器渲染成
DOM、终端渲染成 ratatui 部件，两个宿主读同一份 `ui/schema`
（`{ slots: [{id, cardinality, owner}], schemas: [{version, components}] }`）。
装配是 Effect 语义：声明与注册各持一个句柄，drop 即回滚；向未声明的插槽
注册、重复声明已占用的插槽、`one` 槽注册第二条，都在激活阶段（boot / HMR
reload）直接失败。核心装载器拒绝任何 `ui.*` 服务键（`build_one` →
`ProfileError::NamespaceViolation`）。前端只渲染注册表内的 kind（`panel` /
`list` / `key-value`），未知 kind 显示占位；v1 永不执行远端 JavaScript 或
CSS，装载器拒绝 `script` / `scripts` / `javascript` / `stylesheet` / `css`
字段（行 config 顶层、每个插槽声明、每个 component 对象都查）。

## CI 契约

- `cargo test -p uva-app-server` 绿（含
  [`tests/stdio_roundtrip.rs`](tests/stdio_roundtrip.rs)：spawn 二进制
  驱动完整 stdio wire）；
- `cargo build -p uva-app-server --release` 产出单二进制
  （`make build-release`）；
- 是 `make ci` 的运行时豁免面（其它 lib crate 必须零 tokio）。
