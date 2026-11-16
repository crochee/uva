# Harness V5 收口轮设计 — 以 V4 验收为基线的差距台账

> 状态：本文件是 **台账 + 决策记录**，不是新设计。V4 已验收的结论（零 feature、零运行时依赖、
> 双通道插件、无状态 server）本轮不动；这里只登记"验收之后仍然存在的差距"，以及每条差距
> 的处置与验收方式。
>
> 基线文档：[`2026-10-26-harness-v4-design.md`](2026-10-26-harness-v4-design.md)（设计）、
> [`2026-10-26-harness-v4-acceptance.md`](2026-10-26-harness-v4-acceptance.md)（验收）、
> [`2026-10-26-harness-v4-remainders-design.md`](2026-10-26-harness-v4-remainders-design.md)（遗留 8 项）。
> 对照研究：[`../research/harness-comparison-pi-dsh-2026-10-25.md`](../research/harness-comparison-pi-dsh-2026-10-25.md)。

## 0. 本轮的口径

| 维度 | 本轮口径 |
|:---|:---|
| 范围 | 15 条已定位的差距（G1–G15），分五批落地 |
| 不在范围 | 已被 V4 裁决的两件事：泛型化（V4 §5.5 逐站点复核＝dyn 全部保留）、依赖面收缩（保留 7 个兄弟 crate 依赖） |
| 性能口径 | **先测再改**：新增驱动真身的 bench 行（R1–R4），占比 < 5% 的项不改代码，只把实测占比写回台账 |
| 契约口径 | `contract-scan` + 手写 client；不引入前端 e2e |
| 交付形状 | 一个分支，五批；每批结束时代码可编译、既有测试仍绿、台账有一行新增 |

本轮开始时的基线（批 0 完成后复核）：

- `git status --short` 为空（工作树干净）；
- `cargo tree -p uva-harness -e normal | grep -c tokio` = **0**（harness 依赖闭包无运行时）。

---

## 1. 差距台账

四栏：**差距** / **证据（file:line）** / **本轮处置** / **验收方式**。
"本轮处置"三态：**落地**（本轮改代码）／**进台账**（本轮只测量与记录，代码不动）／**不做**（需另立设计）。

### 1.1 harness 侧（批 1–2）

| # | 差距 | 证据（file:line） | 本轮处置 | 验收方式 |
|:--|:---|:---|:---|:---|
| **G1** | 每轮把整条 transcript 深拷一遍——`messages.extend(session.messages.iter().cloned())` 把整条历史逐条克隆进请求列表 | `crates/uva-harness/src/agent/re_act/loop/prepare.rs:139` | **落地**（批 2 Step 2.1） | 新增契约测试：`prepare` 之后的 `messages` 与"`kept_range` 裁剪 + `prepare_arc`"逐条相等；`cargo test -p uva-context --lib` 绿 |
| **G2** | 压缩时无条件整表克隆——`let mut next = (*session.messages).clone()` | `crates/uva-harness/src/agent/re_act/loop/compaction/mod.rs:153` | **落地**（批 2 Step 2.2） | `cargo test -p uva-harness --lib`（含 `compaction.rs`）绿；压缩后 transcript 与压缩前语义一致 |
| **G3** | 工具批每轮 4 个 Vec 分配（`Batch.outputs` / `runnable` / `safe_indices` / `unsafe_indices`）+ `join_all` 无并发上限、无背压 | `crates/uva-harness/src/agent/re_act/loop/tools/mod.rs:48-54,186-188,261,279` | **落地**（批 2 Step 2.3） | 顺序不变式测试：一轮 3 个 tool_use 时写入日志的 `tool_result` 行顺序 == assistant 行里 tool_use 的顺序；`parallel_safe.rs` / `parallel_unsafe.rs` / `parallel_cancel.rs` 绿 |
| **G4** | 每轮一次 `format!("LLM pass {iteration}")` | `crates/uva-harness/src/agent/re_act/loop/mod.rs:473` | **落地**（批 2 Step 2.5，**按占比判定**：< 5% 则不改代码，只记录占比） | R1–R3 场景里该行的实测占比写回台账 §1.1 末 |
| **G5** | 每个非只读工具调用一次 SHA-256+hex，且 `to_string` 后再 `as_bytes()` 二次拷贝 | `crates/uva-harness/src/agent/re_act/loop/guard.rs:396-401` | **落地**（批 2 Step 2.4，**按占比判定**：< 5% 则不改代码，只记录占比） | R2 场景里该行的实测占比写回台账 §1.1 末；哈希输出逐字节不变（`sha256:` 前缀 + 同样的 hex） |
| **G6** | 超窗路径 ~305 µs 至今没有归因——V4 §5.6 原文："下一轮先加一行能隔离它们的 bench 再谈降本" | `docs/design/2026-10-26-harness-v4-design.md` §5.6 | **落地**（批 1 Step 1.2 R4） | R4 bench 行存在；`cargo bench -p uva-harness --bench hot_paths -- --check` 退出 0；拷贝 / 三次 token 估计 / `Vec::remove` 搬移三段的实测占比写回台账 §1.1 末 |
| **G7** | 基准名不副实：15 行里只有 1 行驱动真 harness，projection 两行测的是 bench 文件里的副本 | `crates/uva-harness/benches/hot_paths.rs:918-954` | **落地**（批 1） | `grep -c "fn projection_memo_hit_v2\|fn close_iteration_v2_cost" crates/uva-harness/benches/hot_paths.rs` = 0；表格里出现 R1–R4 四行 |

### 1.2 server 侧（批 3）

| # | 差距 | 证据（file:line） | 本轮处置 | 验收方式 |
|:--|:---|:---|:---|:---|
| **G8** | follow 每连接每 100 ms 反向扫尾日志（最多 512 行） | `crates/uva-app-server/src/routes/follow.rs:101,636-637,721,780` | **落地**（批 3 Step 3.1） | follow 测试：一条连接在写入 3 条新行后，durable 读取次数 ≤ 2，客户端收到 3 个 `row` 帧 |
| **G9** | 每轮把整个会话日志重读重折叠 | `crates/uva-app-server/src/session/controller/run_task.rs:270-274` | **落地**（批 3 Step 3.2） | controller 测试：同一会话连续两轮，第二轮不再读全量（计数 sink 的 `read()` 计数在第二轮为 0），两轮的模型历史相同 |
| **G10** | `GET /api/v1/tasks` 扫 50 会话 × 256 行且不翻页 | `crates/uva-app-server/src/routes/tasks.rs:50,53-66,119,136` | **落地**（批 3 Step 3.5；批 5 复核发现 id/游标缺陷并修复，见 §9） | 种 3 条 delegation 后 `GET /api/v1/tasks?limit=2` 返回 2 条且 `next_cursor` 非空 |
| **G11** | `/api/v1/tools` 缓存命中仍深拷贝整份定义（而 handler 只用 name/description，schema 当场丢弃） | `crates/uva-app-server/src/routes/tool.rs:82-90,120-140` | **落地**（批 3 Step 3.3） | `GET /api/v1/tools` 连续两次请求，第二次 registry 版本不变时两次响应体逐字节相同 |
| **G12** | 每轮结束连着 flush 两次（两次 fsync），两个 warn 块逐字相同 | `crates/uva-app-server/src/session/controller/run_task.rs:537,545` | **落地**（批 3 Step 3.4） | `cargo test -p uva-app-server --lib` 里"durable 行只写一次"的既有断言仍绿；删除第二个 flush 块后不出现新的 fsync 计数 |
| **G13** | `chat_router()` 是死代码（注释指向不存在的 `server-bin` crate） | `crates/uva-app-server/src/routes/mod.rs:43-51` + `crates/uva-app-server/src/server/router.rs:433-436` | **落地**（批 3 Step 3.6） | `grep -rn "chat_router" crates/uva-app-server/src` = 0 命中；`make lint-rust` 绿（无未用 import） |
| **G14** | 鉴权白名单里有一条没有路由的豁免——`/.well-known/agent-card.json` 在 server 里未注册 | `crates/uva-app-server/src/middleware/auth/path.rs:3` | **落地**（批 3 Step 3.7） | `grep -rn "agent-card" crates/uva-app-server/src` 只命中 `middleware/auth/`（白名单本身与其测试）；三处断言同步更新 |

### 1.3 契约侧（批 4）

| # | 差距 | 证据（file:line） | 本轮处置 | 验收方式 |
|:--|:---|:---|:---|:---|
| **G15** | 契约与实现漂移三处：① `VAR_LET_RE` 只认 `let VAR = Router::new`，丢掉 re-bind 的 `.route` 的 nest 前缀；② curated 文件 3 条失真；③ 12 条端点缺逐条定性 | `contract-closure/lib/backend-scanner.ts:22`（+ `:186-220` 路由归属解析）＋ `contract-closure/curated/web-free-endpoints.json` | **落地**（批 4） | `cd contract-closure && npm run scan` 后：`docs/interface-contract/contract.yaml` 里有 `POST /api/v1/chat/sessions/{id}/operation`、**没有** `POST /chat/sessions/{key}/operation`；`POST /api/v1/chat/sessions` 的 `source` 为 `both`；`npm run check` 退出 0 |

---

## 2. 决策记录

每条：**决定 / 代价 / 出处**。

### D1 — 泛型化：保留 dyn，不做

- **决定**：`uva-harness` 的公开面继续用 `dyn Trait`（`Arc<dyn ModelProvider>` / `Arc<dyn ToolRegistry>` 等），本轮不改。
- **代价**：多一层动态派发；换来"宿主可运行时替换实现"与 V4 §5.5 逐站点复核的一致性。
- **出处**：V4 §5.5（已逐站点复核，结论＝dyn 全部保留）；V4 验收记录。
- **重开条件**：需要另立设计文档并推翻已验收结论，不在本轮内。

### D2 — 依赖面收缩：保留 7 个兄弟 crate 依赖

- **决定**：harness 继续依赖 `uva-core` / `-provider` / `-tool` / `-session` / `-context` / `-steering` / `-mcp`，不做进一步收缩。
- **代价**：harness 不是"零依赖"库；换来 trait 语义单一真源（`Message` / `ToolDefinition` / `Tool` / `SessionSink` 只有一个定义处）。
- **出处**：V4 §5.x 依赖面裁决；本轮 `cargo tree -p uva-harness -e normal | grep -c tokio` = 0 的**运行时**零依赖承诺仍然成立。

### D3 — 无状态口径：本轮只要求"会话真源可跨节点"

- **决定**：会话 / workflows / evals / schedules 的真源是共享 workspace 文件系统，多节点计算无状态这条**已达成**；节点本地状态本轮不动。
- **代价**：11 项节点本地状态（工具注册表 / agent 注册表 / 插件进程 / MCP 子进程 / usage 计数 / Prometheus / 会话搜索索引 / provider 连接池 / 模型 pin 等）**不跨节点**。
- **出处**：`crates/uva-app-server/src/state/app_state/mod.rs:62-310`；`crates/uva-app-server/README.md` 的「多节点部署」节。

### D4 — 性能口径：先测再改，占比 ≥ 5% 才动代码

- **决定**：批 1 先加 4 条**驱动真身**的 bench（R1–R4）+ 4 条比值门禁；批 2 只改在该场景总耗时中占比 **≥ 5%** 的项，< 5% 的项**不改代码**，把实测占比写回台账。
- **代价**：某些项明知有浪费也不改（本轮可能一个都不改）；换来"每次改动都有测量背书"。
- **出处**：本文件 §3 的占比算法；批 1 的阈值口径（同一台机器跑三次取最小观测值，再乘 0.5，实测数字写进门禁上方注释）。

### D5 — 前后端口径：沿用 contract-scan + 手写 client

- **决定**：契约的真源是 `docs/interface-contract/contract.yaml`，由 `contract-closure` 扫描后端路由 + 扫描前端调用生成；本轮**不写前端 e2e**。
- **代价**：前端调用与后端路由的一致性依赖扫描器正确性——这正是 G15 要修的（扫描器本身有缺陷）。
- **出处**：§3.7（AGENTS.md §4.2 当时「不保留任何前端自动化测试套件」）；`contract-closure/lib/curated.ts:19-24`。

### D6 — 交付形状：一个分支，五批

- **决定**：五批按依赖顺序推进（台账 → 基准 → harness → server → 契约 → 收口），每批结束时代码可编译、既有测试仍绿、台账有一行新增。
- **代价**：中途状态可编译但不是终态；换来每批都有独立的 review / 回退点。
- **出处**：本文件 §1 的批次划分。

---

## 3. 进台账、本轮不做

### 3.1 11 项节点本地状态（不跨节点）

| 项 | 现状 |
|:---|:---|
| 工具注册表 | 每节点一份 |
| agent 注册表 | 每节点一份 |
| 插件进程 | 每节点一份 |
| MCP 子进程 | 每节点一份 |
| usage 计数 | 每节点一份 |
| Prometheus 指标 | 每 scrape 目标一份 |
| 会话搜索索引 | 每节点一份 |
| provider 连接池 | 每节点一份 |
| 模型 pin | 不跨节点 |

证据：`crates/uva-app-server/src/state/app_state/mod.rs:62-310`。
**本轮处置：进台账，不做。** 让"运行期注册的工具 / agent / 插件集群内可见"或"usage / metrics 部署级聚合"成立，需要扩展 cluster frame 协议 + Redis 计数语义，是独立一轮。

### 3.2 泛型化

见 D1。**不做**（需另立设计文档）。

### 3.3 依赖面收缩

见 D2。**不做**。

### 3.4 facade feature 表收窄

V4 §5.10 未做的那部分（facade 的 feature 表是否还能再窄）。**不做**——`check-mvp-deps` 已经把"七 feature 最小子集"钉住，继续收窄的收益低于其带来的组合爆炸风险。

### 3.5 `GET /api/v1/sessions` 的逐会话读放大——**本轮只测量不修**

- 现状：每会话 1 次 `read_head` + 1 次 `read_tail(512)`（`crates/uva-app-server/src/routes/sessions/mod.rs:630-637`），读放大随会话数线性增长，且分页发生在读之后。
- **本轮处置**：只加一条 `tracing::debug!`（session_id + 读耗时）并把该事实写进台账；不修。
- **理由**：修法需要新的缓存层与失效键，属于"状态面"，与本轮"不动节点本地状态"（§3.1）一致。
- **验收方式**：日志里能看到每会话的读耗时；台账记录"读放大随会话数线性增长、分页在读之后"这一事实。

### 3.6 `make ci` 基线复核

批 0 完成后先跑一次基线（`make ci` + tokio 计数）。若已红，**停下报告**，不在破基线上继续。

---

## 4. 批次的验收行

| 批 | 内容 | 验收行 |
|:--|:---|:---|
| 0 | 差距台账 | ✅ 本文件存在，G1–G15 每条有证据行号与验收方式；基线复核（`make ci` 绿、`cargo tree -p uva-harness -e normal \| grep -c tokio` = 0）通过 |
| 1 | 基准驱动真身 | ✅ R1–R4 四行在表里；两行重实现已删（`grep -c "fn projection_memo_hit_v2\|fn close_iteration_v2_cost"` = 0）；`--check` 退出 0 |
| 2 | harness 热路径 | ✅ G1/G2/G3 落地，G4/G5 按 <5% 判定不改（占比见 §6）；实测见 §5 |
| 3 | server 读放大与死代码 | ✅ G8–G14 的验收方式逐条绿（含 5 条新测试） |
| 4 | 契约与前后端贯通 | ✅ 扫描器修好（re-bind fixture 断言）；curated 3 条失真修好；12 条端点逐条定性（§7）；`npm run check` 退出 0，`frontend_only=0` |
| 5 | 收口 | ✅ G10 落地缺陷修复（§9）+ CHANGELOG [Unreleased] 条目；`make ci` 门禁绿 |

---

## 5. 批 1–2 的实测数字

同一台机器（WSL2，本仓 CI 同款），`make bench` 表格行；每行三次运行取最小值。

### 5.1 表格行（改前 → 改后）

| 行 | 改前 | 改后 | 变化 |
|:---|---:|---:|:---|
| `ReActAgent turn (401-message history)` | 1 305 908 ns | **562 868 ns** | **2.32×** |
| `TruncatingContextManager over-window (401 msgs)` | 346 794 ns | **115 183 ns** | **3.01×** |
| `TruncatingContextManager (401 msgs, over window)`（既有行） | 494 876 ns | 122 486 ns | 4.04× |
| `ReActAgent turn (scripted, 1 tool call)`（既有行） | 231 590 ns | 226 721 ns | −2.1% |
| `ReActAgent turn (8 host hooks)`（新行） | 248 516 ns | 246 712 ns | −0.7% |
| `ReActAgent turn (20 parallel tool calls)`（新行，修好 provider 前无效） | — | 2 640 188 ns | — |

### 5.2 门禁观测值（三次运行的最小值 × 0.5 = 门禁阈值）

| 门禁 | 观测（改前） | 阈值 | 观测（改后） |
|:---|---:|---:|---:|
| R1：401-msg 历史 vs 空历史 | 0.171× | 0.08 | 0.39× |
| R2：20 并发 vs 并发上限 1 | 9.0× | 4.5 | 9.2× |
| R3：8 hooks vs 0 hooks | 0.925× | 0.46 | 0.93× |
| R4：超窗 prune vs Noop | 0.272× | 0.13 | 0.79× |

### 5.3 G6 的归因（R4 场景，改前 347 µs）

| 段 | 依据 | 占比 |
|:---|:---|---:|
| `Vec::remove` 搬移 | 401 条消息、window=2000 → 约 160 次成对丢弃，每次 memmove 整个尾段 | **~60%**（≈208 µs） |
| 三次 token 估计 | `estimate_messages_token_count (401 msgs)` = 19 948 ns × 3 次（整表 / 尾段 / 收尾） | ~17% |
| 拷贝（bench 侧） | `Vec<Message> clone (401 msgs)` = 79 168 ns（bench 自己构造输入的开销，不是 manager 的） | ~23% |

**结论：G6 的成本主导项是 O(n²) 的 `Vec::remove` 搬移，不是 token 估计。** 批 2 把 drop 决策抽成 slot 上的 `drop_plan`、把"边扫边删"换成"一次重建"（O(n)），这一项随 G1 的投影实现一起消失——R4 从 347 µs 降到 115 µs，既有行从 495 µs 降到 122 µs。

### 5.4 G1 的占比与改后效果

- **占比（改前）**：`build_request` 的整表深拷 ≈ 79 µs / 1 306 µs = **6.0%**（≥5% 命中）。注意分母里另有 bench 自身 79 µs 的输入拷贝与 harness `initial_transcript` 的 79 µs 拷贝——G1 本项就是 6%。
- **改后**：R1 场景 1 306 µs → 563 µs（2.32×）。增益来源是**两个**：跳过被丢弃消息的拷贝（G1 本项），以及跳过 manager 在 `prepare` 里的 O(n²) 就地删除（G6 的主导项）——投影让 manager 从"重写一份列表"变成"只做决策"。

---

## 6. 批 2 的逐条判定（Step 6 的 ≥5% 规则）

| 项 | 场景 | 实测占比 | 判定 | 依据 |
|:---|:---|---:|:---|:---|
| G1 整表深拷 | R1 | 6.0% | **改** | ≥5%；`kept_indices` 投影（§8 说明为何不是 `Range`） |
| G2 压缩整表克隆 | 无隔离行（压缩路径） | — | **改** | 计划直接指定；`Arc::make_mut` 在独占引用下零拷贝，同阶最坏 |
| G3 工具批 | R2 | Vec 分配 ~0.04% | **改（语义半）** | 性能半（4 个 Vec 合并）按 <5% 不改；落地的是**并发上限**（`join_all` 无上限 → `buffer_unordered(concurrency)`），顺序不变式由新测试守住 |
| G4 `format!("LLM pass {n}")` | R1/R3 | ~0.04% | **不改** | 200 ns / 250 µs；计划明言"默认不动" |
| G5 SHA-256+hex 二次拷贝 | R2 | ~0.7% | **不改** | 每调用 ~1 µs、20 调用 ~30 µs / 2.6 ms；且 `serde_json::to_vec` 换 `to_string` 的收益在它自己身上只有一次分配 |

---

## 7. 批 4 的 12 条端点定性

修好扫描器与 curated 之后：`POST /api/v1/chat/sessions` 变 **both**（不再抑制），backend-only 抑制剩 **11 条**，`frontend_only=0`（前端没有调用任何后端不提供的路由 → **前端无需改动**）。

| 端点 | 定性 | 理由 |
|:---|:---|:---|
| `POST /api/v1/chat/sessions` | **接**（→ both） | `uva-web/src/api/chat-stream.ts:148` 的 `createChatSession` 就在调它；模块文档写明 "creation is the only way a chat starts"。原抑制理由是假的，已删 |
| `GET /api/v1/sessions/{key}/events` | 不接 | web 用 `/follow`（一个 reader per log）；这是外部轮询客户端的 SSE 面 |
| `GET /api/v1/chat/usage` | 不接 | web 读 per-session usage；这是部署级聚合，给运维 |
| `GET /api/v1/sessions/search` | 不接 | web 用 `/api/v1/search`（统一索引）；这是会话域变体 |
| `POST /api/v1/attachments` | 不接 | composer 把 bytes 放在 turn 里；store 的 API 给"先上传后引用"的客户端 |
| `GET /api/v1/attachments/{key}` | 不接 | 同上；composer 不回读 store |
| `DELETE /api/v1/attachments/{key}` | 不接 | 同上；composer 不删 store |
| `POST /api/v1/chat/sessions/{key}/operation` | 不接 | `[operations] enabled` 默认关；web 走 chat message 路由 |
| `GET /livez` | 不接 | 编排器探针。web 的健康探针是 `/readyz`（→ both），不是原理由写的 `/api/health`（后端没有这个路由） |
| `GET /readyz` | both（不在抑制表里） | `useServerHealth.ts:26` 的 `HEALTH_URL = '/readyz'` |
| `GET /version` | 不接 | 编排器/客户端用；web 从 bundle 读版本 |
| `GET /metrics` | 不接 | Prometheus 抓取 |
| `POST /v1/messages` | 不接 | Anthropic 兼容面；web 用 `/api/v1/chat/*` |

（12 条原始条目 = 11 条抑制 + 1 条转为 both 的 `POST /api/v1/chat/sessions`。）

---

## 8. 与计划的两处偏离（记录在案）

1. **`kept_range` 的形状改为 `kept_indices`（`Vec<usize>`）。**
   计划 Step 2.1 假设 `TruncatingContextManager` 的保留集合是一个**连续区间**（`Some(start..len)`）。实测不是：成对丢弃删掉 user+assistant，留下其后的 tool_result 与 summary，保留集合是**散的**——`the_projection_is_not_a_range` 这条测试就是把这件事钉住的。区间形状会让调用方拷到一份**不同的**集合（丢历史，即计划标注的最高风险）。契约测试 `the_projection_names_exactly_what_prepare_keeps` 在 5 个 window 档位断言投影与 `prepare` 逐条相等、estimate 与 truncated 标志一致。
2. **G3 只落"并发上限"半，不落"4 Vec 合并"半。**
   计划要求 `Batch.outputs`/`runnable` 与 `safe_indices`/`unsafe_indices` 四个 Vec 合并为两个。实测 Vec 分配占 R2 场景 ~0.04%（<5%，规则说不改）；且 `outputs` 与 `runnable` 的合并被借用检查挡住——两桶在"借 `&ToolUse` 跨 await"与"写回输出"之间需要同一份可变切片。已删掉的是确实冗余的 `safe_indices`/`unsafe_indices` 二次拷贝（`bucket_by_execution_mode` 本来就跳过了空槽，过滤是恒真的）。

---

## 9. 收口复核：G10 落地缺陷（批 5 发现，已修）

批 3 落地的 G10 分页在收口复核中被对运行中 server 的直接 curl 实测推翻，两个缺陷 + 两个连带破损，均已修：

| 缺陷 | 实测证据 | 修法 |
|:---|:---|:---|
| **游标无法续页** | id 内嵌扫描尾窗里的行下标——子日志每长一行下标就移一位，第二页带着 `next_cursor` 回来时按 id 找不到行，页为空 | id 改为 `<parentSessionId>:<childSessionId>`：两个半都是委派即定的会话 id，一个委派的 id 终身不变 |
| **同一委派列出两次** | 委派被记录两份（子会话自己的日志开头 + 根父日志的镜像，见 `subagent_router` 模块文档），扫描两处都命中 | 扫描按 `child_session_id` 去重；记录统一从子自己的日志读（头行 + `UserInput` prompt + `subagent_exit` 尾行），父镜像只作子日志不可读时的降级 |
| **行序不定** | 旧行序是扫描序（`list_all()` 的 HashMap 迭代序），跨请求不稳定 | 列表排序为全序：`started_at`（默认降序＝最新在前）+ id 平级决胜；未带 `sort` 的请求也注入默认排序，续页顺序与首页一致 |
| **详情路由解不开 id** | 旧 id 的 seq 是尾窗下标，`get_task` 拿它索引全量读，对不上行 | 详情路由读子自己的日志解析；父不匹配 → 404；id 两半按 `validate_resource_name` 校验（子半会落进文件系统路径，穿越形状 → 422） |

web 侧同步：TasksPage 改走 `useCursorList` + Load more（跟随 `next_cursor`，不再截断在默认 limit=20）；ToolsPage 的 "Recent task delegations" 面板改为 `?limit=5` 取最新 5 条（服务端默认序即最新在前）。

**验收**：`cargo test -p uva-app-server --lib routes::tasks` 5/5；`--test management_routes_test` 16/16（含 `%3A` 编码 id 的 wire 往返、父不匹配 404、穿越形状 422 三条 HTTP 级测试）；对运行中 server 实测——list 最新在前、`limit=1` 翻页第二页返回较旧一条且 `next_cursor=null`、`%3A` 详情 200、穿越 id 422、错父 404；`contract-closure && npm run check` 退出 0（`frontend_only=0`）。

---

## 10. 进台账的下一轮项（本轮实测暴露、未在本轮修）

| 项 | 证据 | 说明 |
|:---|:---|:---|
| **G6 的 O(n²) 搬移在 `prepare` 里仍在** | §5.3 | 投影让 **harness 的热路径**跳过了它，但 `ContextManager::prepare`（`prepare_arc` 的兜底、以及所有不实现投影的 manager）走的还是"先收集全部 drop 再一次重建"。`apply_plan` 已把重建降为 O(n)，**但 `drop_plan` 内部的 `tail.remove(i)` 仍是 O(n²)**（只是元素从 ~100 字节的 `Message` 变成 16 字节的 slot）。R4 从 347 µs 降到 115 µs 说明常数项已小很多；若还要再降，把 plan 的模拟换成写指针扫描即可（O(n)），属机械改动 |
| **`NoopContextManager::prepare` 重复调用两次 `estimate_tokens`** | `crates/uva-context/src/context_manager/mod.rs:360-365` | 两次逐字相同；~20 µs/次。不在 G1–G15 里，未动 |
| **`GET /api/v1/sessions` 的逐会话读放大** | `crates/uva-app-server/src/routes/sessions/mod.rs`（本次加了 debug 探针） | 每会话 1 次 `read_head` + 1 次 `read_tail(512)`，**分页在读之后**：读成本随会话数线性增长，`limit` 不约束它。修法需要新的缓存层与失效键，属"状态面"，与 §3.1 一致本轮不动 |
| **会话跨轮的增量折叠** | §G9 | G9 落的是"日志没变 → 不读"；生产里每轮都会 append，所以命中率主要来自同轮内的重复读与接管重建。真正的修法是**增量折叠**（`read_since` 取增量 + 把 `SurfaceFold`/投影状态留在缓存里），难点在折叠是有状态的：compaction 的 `Replace` 是**位置**区间（要能作用到缓存面上），投影持有 pending tool calls 与 deferred user。属 `uva-session` 的折叠 API 扩展，独立一轮 |
| **`GET /api/v1/tasks` 对 depth≥2 的委派不可见** | `crates/uva-app-server/src/session/subagent_router.rs`（`ParentSink::Deeper` 不写父日志） | 孙辈委派只记录在孙辈自己的日志里（`subagent_enter` 头行），而扫描遍历的是注册会话的日志尾窗——孙辈会话不注册就永远扫不到。修法要么扫盘枚举会话（sessions 路由已有该机制），要么路由器把镜像写到直接父而非仅根父。属发现面扩展，独立一轮 |
