# Changelog

All notable changes to Uva are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Releases before **0.1.0 (2026-09-30)** are snapshot milestones from an
earlier internal tree and are folded under **Prior releases** at the
bottom of this file. The per-round development log (R0 – R125, May – Sep
2026) lives under **Historical round log** below the unreleased section
— frozen per the precedent set in `optimization-report-R46`.

## [Unreleased]

### Added (无头 harness 的业界对齐：dsh + omp 逆向后的三十刀)

逆向证据与完整缺口账本：[`docs/design/2026-11-26-headless-parity-dsh-omp-design.md`](docs/design/2026-11-26-headless-parity-dsh-omp-design.md)。
第一轮参照 **DeepSeek Harness（dsh）** 与 **omp（oh-my-pi）** 的无头控制面
对账后落了十八刀；第二轮（工具批的执行模型）再落三刀；第三轮（客户端的
扩展工具）又落两刀；第四轮（提问、改动、检索、断尾、耐久清单、会话的名字与正在动的文件）又落七刀：

第四轮的七刀：

- **模型能把问题交给人**（G21，第四轮切 24）：两个参考都有「模型问人」的
  工具——dsh 的 `tool-ask-user`（`ask_user_question`）与 omp 的 `ask`。
  本仓此前只有审批闸门（一个只会问「准不准」的有界替身）。现在 `ask` 工具
  发**结构化问题**（id / 问题 / 选项 / 可多选），等**有界**的答案（默认
  120 s），答案作为普通工具结果回到模型。答案批次必须**每个问题恰好一次**
  ——漏答 / 重答 / 答了没问的问题都被拒：把坏客户端的批次当答案，等于让
  它替人说话。谁回答是**宿主的缝**（`QuestionOwner`，与文件、终端同构）：
  核心面经 `questions/ask` 向客户端发问（第三轮切 22 的出站通道），没人
  可问就**失败关闭**而不是把回合挂住。**ACP 面如实缺席**：ACP v1 只有
  `session/request_permission` 能问人，编辑器开的会话**立刻回绝**，
  不把预算耗在一个给不出的答案上。
- **文件改动以 diff 呈现**（G22，第四轮切 25）：ACP 的 `ToolCallContent`
  有 `Diff` 变体（`path` / `old_text` / `new_text`），omp 的 ACP 面就靠它
  把每次编辑画成 diff；本仓此前**从不发这个变体**——编辑到了客户端只剩
  一句「改了第 12 行」。现在 `write` / `edit` 把改动报进结果的 metadata
  （**不在**内容里：模型看到的还是那句话，改动是给**人**看的字节），ACP
  面在结算帧发 `Diff` 内容块，核心面的 live 帧带 `diff` 成员。报告有界
  （每边 128 KiB）：装不下就标 `truncated` 且**不带文本**——半个 diff
  比没有 diff 更糟。durable 行**不带** diff：改动是活的事实，事后只读
  得到工具的摘要。
- **客户端知道 agent 正在动哪个文件**（G27，第四轮切 30）：ACP 的
  `ToolCall.locations`（「follow-along」——跟踪 agent 正在处理哪些文件）本仓
  从不发送。现在 `Tool::locations(&input, &context)`（默认：无）让**工具自己**
  在调用开始前答「会动哪个文件」——omp 从参数里抽路径是猜，本仓的规则是
  「工具的边界随工具走」：工具才知道自己怎么解析参数（客户端的工作区、
  会话的根），**发布的位置**与**真正被碰的文件**出自同一个答案。
  `read` / `write` / `edit` 把「解析路径」提成自己的一个方法，`call` 与
  `locations` 共用它。重放没有注册表可问，发的是不带 locations 的形状：
  重放的调用带身份，碰过的文件是日志的事（`changed` 成员）。
- **会话列表给会话起名字**（G26，第四轮切 29）：dsh 有耐久的标题状态、omp 用
  小模型生成标题；本仓的 `sessions/list` 此前只有 id / 状态 / 时间——**一串 id
  不是索引**。现在标题**从日志派生**（第一条 user 消息的第一行，截到 80 字符，
  截在词边）：**不调模型**（提示词工程与成本不在本轮范围），也**不另存一份**
  可能与日志漂移的真相。列表本来就读头几行（创建时间）⇒ 标题不额外读盘；
  没有消息的会话**没有**名字，而不是被编造一个。
- **durable 行知道这次调用改了哪些文件**（G25，第四轮切 28）：改动只活在
  live 帧与 ACP 的 diff 块里——客户端断开后再读日志，只剩工具的一句话摘要。
  现在 `tool_result` 行多一个 `changed` 成员（**路径**，不是 diff 本身：
  diff 是两个文件的大小，进日志太重）：harness 的提交路径从工具的报告里把
  路径提出来（与它提 `metadata.error.code` 同一条漏斗），wire 信封与前端
  类型同步——契约测试自己抓到了这个新成员。
- **断尾截断：半行不是行**（G24，第四轮切 27）：日志末尾那没有换行的半截
  **不是一行**——它是「读者撞上了正在写的行」或「崩溃撕开的行」。本仓此前
  两头都缺：**读者不容忍**（读到最后那半行会解析失败，整个读取失败 ⇒ 客户端
  在 agent 正在写的时候读会话会拿到一个错误，而它分不清「正在写」与「坏了」），
  **写者不截断**（崩溃撕开的尾巴留在原地，下一次追加会被粘上去，一行变成两行，
  而且日志永远读不了）。现在读者只读**完整的行**（中间的坏行仍大声失败），
  写者在第一次写之前**切掉**断尾（只读有预算的尾巴，开 sink 仍是 O(1)；
  不是普通文件的东西不会被读）。dsh 的崩溃修复第一步就是这个。
- **模型能查网**（G23，第四轮切 26）：本仓此前只有 `web_fetch`（读一个模型
  **已经知道**的 URL）——模型不能搜索就是瞎的，训练之后发生的事它一无所知。
  现在 `web_search` 工具问一条**缝**（`WebSearchProvider`）：随仓发布的
  适配器问一个回答 JSON 的搜索端点（SearXNG 及其兼容实例的形状），一个
  形状一个适配器，部署要别的供应商就写一个实现——工具不变。**没配端点就
  失败关闭**：工具照样挂载，调用明确说「没配搜索供应商」，而不是回一句
  「没有结果」假装网上是空的。答案有界（10 条 / 每条 800 字符 / 整篇
  16 KiB），被截断时**自报**。

- **核心面有了出站请求通道**（第三轮切 22）：omp 的 RPC 服务器会向宿主发
  `host_tool_call` 并等 `host_tool_result`，本仓的核心面此前只有一条方向
  上的请求——客户端问，核心答。现在核心可以铸自己的请求 id
  （保留前缀 `core:internal:`），把问题发给客户端并在预算内等应答；
  应答帧先查核心自己的表再落 ACP（ACP 的 `answers()` 认领每一张响应
  形状的帧，这条优先级是唯一的区分）。网关零改动——它早就按会话所有权
  路由核心发的请求帧（ACP 权限问题走的同一条路）。
- **客户端可以拥有工具**（第三轮切 23，omp `set_host_tools` 的核心面
  对应物）：`tools/register` 增 `owner: "client"` 与 `session_id`——客户端
  声明名字/描述/schema，核心把它登记进注册表（模型能看到、能调用），
  调用经 `tools/execute` 请求发回**拥有该会话的客户端**，应答
  `result.{content, is_error}` 就是工具结果；超时是结构化 `TOOL_TIMEOUT`
  （60 s）。别的会话调用被明确拒绝（注册表是节点一个，名字全局、执行
  按会话归属——已知 wart 如实记在账本里）。ACP 面没有 `set_host_tools`
  原语，那一半继续缺席。

第二轮的三刀：
- **工具批按模型序执行**（G18，第二轮切 19）：两个参考都按模型写的顺序
  跑工具批——dsh 分组（`packages/core/tools/src/batch.ts:78-83`），omp 用
  `lastExclusive` 链屏障（`packages/agent/src/agent-loop.ts:4044`）。本仓
  此前**按模式分桶**：全部 Parallel 先跑，再全部 Sequential——模型说
  「先写后读」，本仓先读。现在调度器按模型序分**波**：连续的并发安全调用
  共享一波，Sequential 调用是屏障（前面的波排干才开跑，后面的波等它）。
  既有保全语义按新序重新表述：排他调用失败 ⇒ 其后的排他调用跳过
  （文案不变），共享波照跑（dsh/omp 都不停兄弟）。
- **并发安全按调用判定**（G19，第二轮切 20）：dsh 的
  `isConcurrencySafe(args)` 与 omp 的 `resolveToolConcurrency(tool, args)`
  都按**调用**判，本仓只按工具名判——`shell` 永远串行，`ls` 也排队。
  现在 `Tool::mode_for(&input)`（默认回落 `mode()`）让工具按参数自判，
  调度器问**这一次调用**；`shell` 第一个吃螃蟹：只读命令
  （风险分类 `Low`，失败关闭的白名单）可并行，其余独占。
- **工具级超时是结构化 `TOOL_TIMEOUT`**（G20，第二轮切 21）：dsh 的
  `timeout-policy` 给每个工具武装 `timeoutMs`，到期把结果换成结构化错误
  （`error.info.code = 'TOOL_TIMEOUT'`）。本仓一处都没有：stdio MCP
  服务器不出声就把回合**永远**挂住——无头端没有操作员会注意到。现在
  `ToolOutput::timeout(after)` 造出 dsh 形状的结构化失败，harness 的提交
  路径把 `metadata.error.code` 提升进 durable 行的 `error` 成员（中断行
  已经在用的位置），stdio MCP 传输的每个请求跑在一个预算下
  （默认 30 s = HTTP 兄弟的默认；`StdioConfig::call_timeout` 可调），
  到期 `McpTool` 答结构化超时而不是散文。

第一轮的十八刀：

- **审批有了截止时间**（G1）：被持有的调用在 `approval.timeout_secs` 内
  没有答案，就按**拒绝**结算——决定落进日志（`approval_decided`，`denied`），
  回合**继续**，模型读到的是一条**超时**拒绝而不是「操作者拒绝了」。
  期限由**控制器**持有（harness 在挂起时已结束回合，等操作者是宿主的事），
  到期走的是与人工作答**同一条** resume 路径，因此一次调用只有一条决定。
  新增 `ResumeStatus::TimedOut`：日志词汇仍是 `approved` / `denied` /
  `cancelled`，但模型可见的文案区分「没人回答」与「有人说不」。
  这一刀补上了 `crates/uva-app-server/src/rpc/mod.rs` 早先声称的
  「a timeout still defaults to deny」——此前代码里没有这个机制。
- **`approval` 行的 config 真的生效了**（G1b）：新增 `[approval]` 配置段
  （`mode` / `ask_unknown` / `timeout_secs` / 逐工具 `overrides`）。此前
  `approval` 行发布的永远是 `ApprovalPolicy::default()`，行的配置是摆设。
- **委派有了宿主控制面**（G8）：新增 `subagents/list`、`subagents/cancel`、
  `subagents/steer` 三条核心 JSON-RPC 方法（omp 的
  `get_subagents` / `cancel_subagent` / `steer_subagent` 同构）。机器早就在
  `uva-tool-task` 的控制面里（模型侧是 `agents_*` 三个工具），这一刀只是
  **开门**：控制面由 `tool-task` 行发布为 `control` 服务，宿主经同一张表
  列举 / 硬杀 / 转向子代理。取消是**幂等**的——已经停掉的子代理答
  `cancelled: false`，不是错误。
- **durable 读有了游标**（G11）：`sessions/events` 接受 `since`（日志序号），
  只回它之后的行，并回 `last_seq` 让客户端接着读（omp 的 `get_entries?since`
  同构）。游标跨行推进——连本版本读不懂的行也算在内，客户端不会永远重问
  同一行坏行。
- **运行中的回合能收到外面的消息**：`delivery: "steer"` 的路由文档一直写着
  「注入当下正在跑的回合」，控制器注释也这么写——但那条通道从没接线：行是耐久的，
  模型这一轮却看不到它，而响应回答的是 `"delivery": "steer"`。现在主会话有自己的
  inbox：控制器在跑起来时建通道，live steer 在行落盘后把消息交给它，回合的下一个
  step 边界就把它带进模型的下一个请求（顺序即 `RunInbox` 的契约：先写行，再交消
  息，中间崩了丢的是内存副本）。**后台作业的完成通知走同一条路**：作业结束，拥有
  它的会话立刻知道，模型不用自己去问。
- **目标模式**（G16）：两个参考都有耐久目标——dsh 的 `tool-goal` 与 omp 的 goal
  mode；本仓此前只有 `todo`（一份清单，不是跨回合的目标）。现在 `goal` 工具能
  创建 / 读取 / 暂停 / 恢复 / 完成 / 放弃一个目标，目标是**会话日志里的行**（每
  变一次一行，最后一行即状态），工具不拼任何行——`GoalStore` 缝读最后一行、写一
  行并 flush。**宿主续跑**：回合结束、没有排队的回合、目标是 active 时，宿主自己
  起下一回合（把「继续朝着目标工作」park 成耐久 prompt）。两道边界：目标预算撞
  上会话的 `usage` 总和 → 暂停；续跑 25 回合 → 暂停；任何用户回合清零计数。不
  完成的目标于是变成一次可恢复的暂停，而不是无限自转。
- **工具族有了 `glob` 与 `grep`**（G15）：文件发现与内容搜索此前只有语义 `search`
  与 shell 里的 `rg`——受策略限制或没有 shell 时，模型找不到文件。现在 `glob` 按
  glob 模式列出文件（相对路径、最新在前），`grep` 按正则搜内容（`path:line:text`
  的形状）。两个工具都走本仓自己的**有界遍历**（不引外部 ripgrep）：`.git` 不进、
  符号链接目录不进、最多看 20000 个文件；答案有界（glob 100 条、grep 250 条、单行
  2000 字节）且**截断时自报**——截断过的搜索不能被当成完整的搜索。客户端拥有的
  工作区**如实拒绝**：ACP 的 `fs/*` 没有目录列举，遍历看不见客户端的文件。
- **工具族有了 `lsp`**：模型最缺的那一格是「编译器对这个文件说了什么」——
  不然它只能去 grep 错误文本。现在 `lsp` 工具启动部署为该语言配置的服务器，
  `initialize` → `didOpen`，等服务器发布的诊断，按 `path:line:col severity
  message` 的形状回给模型。服务器是**部署的声明**（`[lsp]` 配置段的
  `[[lsp.servers]]`：语言 / 命令 / 参数），`tool-lsp` 行发布给工具：一种语言
  一个服务器（两个服务器会让工具无从选择），没有对应服务器时工具按语言名回绝。
  协议手写（LSP 的 `Content-Length` 定界与本仓 RPC 同形不同协议，抄 40 行比拉
  新依赖便宜）。工具拥有服务器的长度是**一次调用**：启动、问、停——部署付的是
  「每次调用一个语言服务器」的价，不是「每个会话一个」。**查询面也齐了**：
  `goToDefinition` / `findReferences` / `goToImplementation` / `hover` 四个动作
  （dsh 的同名动作）——服务器没宣告对应能力时工具**按能力名回绝**（不说「没有
  结果」）；光标在工具的边界上是 1 基行列号，上线时变成协议的 0 基 UTF-16；
  引用含声明。如实缺席：`lsp` 的**编辑面**（omp 的 `applyWorkspaceEdit`——本仓
  的 `edit` 工具是它的有界替身）；见
  [`docs/design/2026-11-26-headless-parity-dsh-omp-design.md`](docs/design/2026-11-26-headless-parity-dsh-omp-design.md)
  的切 17。
- **命令可以放到后台跑**（G14）：`shell` 在前台跑命令，600 s 上限即放弃——构建、
  测试、迁移都装不进这个窗口。现在 `job` 工具可以把命令放到后台：一次调用里的
  `start` 给你作业 id，之后 `output`（可以等）、`list`、`kill` 接着用。作业住在
  一个部署级的注册表里，所以上一次调用起的作业下一次还在。后台作业跑在 **shell
  同一条执行策略**下——后台不是绕过沙箱的路。`shell` 与 `job` 现在共用同一份
  spawn 代码（argv、工作目录、环境、进程组、管道读取），「命令在工作区里跑」
  这句话对两个工具是同一句。作业完成后的**主动通知**也落地了（见下条）。注册表
  只保留最近的 32 个**已完成**的作业（ULID 的顺序就是创建的顺序），跑了一千个
  作业的部署不会抱着一千份输出不放。
- **工具族有了 `edit`**（G13）：此前只有 `read` 与 `write`——改一行要把整个
  文件重写一遍，评审者看到的 diff 也因此变大。现在 `edit` 做**逐字**替换：旧文本
  必须原样出现，多处命中被拒并**点名行号**（除非显式 `replace_all`）；失败分两种
  可能，各给一条下一步——「这次改动已经应用过」或「逐字不符，去重读并复制原文」。
  两条在读文件之前就拒：旧新相同、`replace_all` 配空白旧文本。编辑走 `read`/
  `write` 同一条缝：客户端拥有工作区时，改的是客户端的字节。
- **客户端可以拥有终端**（G12 的 terminal 半边）：ACP 的 `terminal/*` 让**客户端**
  拥有终端——agent 不在自己机器上 spawn 进程，而是请客户端在它的窗格里跑，
  用户看得见、能打断。`shell` 工具现在按会话路由：客户端在 `initialize` 里声明
  `terminal: true`，其会话的命令就交给客户端（`create` → `wait_for_exit` →
  `output` → `release`，超时则 `kill`），其余会话照旧在本机跑，策略与沙箱不变。
  工具的描述同时写明两条路——本机路径讲执行策略，客户端路径讲「客户端的规则
  代替策略」：一段谎称沙箱在生效、而命令其实跑在用户窗格里的描述比没有描述更
  糟。恢复的会话（`session/load`）与新会话走同一条认领路径，文件与终端一起
  对齐（此前只有 `session/new` 认领客户端的文件）。
- **出站有了背压落盘**（G10）：出站通道原先是无界的——读者比作者慢时，进程
  内存无限增长（omp 的 `rpc-frame.ts` 记的正是这个问题）。现在出站拆成**编码
  器**与**写入器**两个任务：编码器把信封通道排空成帧字节，写入器拥有 stdout；
  队列 = 内存段（4 MiB 水位）+ 磁盘段（私有临时文件 spool，64 KiB 块排空，
  排空完即删）。溢出之后新帧一律进磁盘段，因此**到达顺序**由构造保证。写不
  下时**大声失败**（日志点名丢帧），不无声丢。入站逻辑上限 8 MiB → **64 MiB**
  （omp v2 的重组上限）：Content-Length 没有物理帧上限要分块绕过，唯一能被撞
  到的旋钮就是逻辑上限。omp 的「分块帧 + 协议协商」不抄：Content-Length 按长
  度定界，拆帧要在 JSON-RPC 层加重组协议（线格式破坏），而协商在只有 v1 的协
  议里是投机——本仓的等价旋钮（帧上限）已对齐。
- **会话有了跨进程的所有权**（G9）：两个进程共享一个 workspace 时，都能打开
  同一会话并各自追加——两个写者盖出碰撞的序号，而序号无法复现的日志不可读。
  现在写路径在**首次追加**时对会话目录的 `owner.lock` 取 flock（dsh
  `session.lock` 语义：读句柄自由共存、无过期、持有到进程死）；第二个写者的
  append 得到 `SessionError::OwnedByAnotherProcess { holder }`——拒绝失败
  关闭，沿着写路径本就致命的语义走。优雅关闭（`writer.close`）释放所有权，
  集群接管（takeover）立即接管——崩溃场景由内核关描述符释放，同一通道。
  与 omp 的差异如实：omp 换新兄弟文件，本仓的会话 id 就是身份，拒绝是唯一
  安全答案。非 unix 平台用 O_EXCL + pid 文件（僵死文件同样失败关闭）。
- **`replay_state` 的声明对齐了现实**（G2）：两个 `replay_state` 字段的文档
  此前声称「适配器校验并消费」，而全仓没有任何生产者铸造过状态。核实后如实
  记述：在仓适配器无可铸造的原生状态（OpenAI 走 Chat Completions；
  Anthropic Messages 无可回放的 response id），dsh §7 背后的机制——推理
  连续性——已由「思维块签名 + 压缩边界作废规则」结构化交付，且该规则是
  **精确的**：压缩的 span 永远从 surface 头开始，每条 kept-tail 消息的前缀
  都含被遮蔽 span，不存在误作废（新增钉子 `a_signature_in_the_kept_tail_is_voided_too`）。
  字段保留为 seam：会铸造原生状态的适配器（Responses API 的
  `previous_response_id`、逐消息前缀摘要）铸造在响应、回传在请求、校验在
  适配器，两端已存在。
- **插件树有了审计报告**（G6）：`profile/audit` 回答「这棵树有没有不能用的行」
  ——dsh `auditRows` 的三态语义。不在报告里的行就是 usable；报告里只有
  `failed`（从未启动或激活被拒）与 `pending`（在等一个组合不提供的服务，
  `missing` 逐一列出）。缺口计算复用 mount 自己的不动点，所以报告说的与
  mount 会说的是同一件事——修好一个依赖后两边同时转绿，不会漂移。
- **压缩的摘要请求复用被路由请求的暖前缀**（G4）：此前摘要是旁路请求——自己
  的 system prompt、span 渲染成一段文字——provider 的前缀缓存对它毫无用处，
  每轮压缩都是一次全新计费。现在 `SummaryInput` 信封让步内压缩重放**会话自己
  的** system prompt 与工具 schema、被遮蔽的消息按 surface 顺序、指令作为
  **最后一条 user 消息**（dsh `summarizer.ts:74-79` 的形状）——摘要请求是上一个
  被路由请求的真前缀，暖缓存覆盖除指令外的一切。手动压缩（`sessions/compact`）
  没有可读的 run 信封，保持独立形状并注明。压缩指令换成 dsh 的分段骨架
  （Primary Request / Key Concepts / Files / Errors / Pending / Current / Next /
  Critical Context——空节写 `(none)`），恢复的模型拿到的是可导航的检查点。
- **Anthropic 适配器不再忽略 `tool_choice`**：canonical
  `CompletionRequest.tool_choice` 此前被完全丢弃——任何调用方设 `None`
  都被静默当作 `auto`。现在映射到线上字段（`none` / `any` / `tool{name}`；
  `auto` 省略——Anthropic 默认就是它）。压缩摘要在重放工具 schema 的同时用
  `None` 锁死答复是文字而不是工具调用；空工具列表保持 `auto`，避开 Anthropic
  拒绝无工具却带 `tool_choice` 的请求。
- **客户端拥有它自己的文件**（G12 的 fs 半边）：一个在 `initialize` 里声明
  `clientCapabilities.fs` 的 ACP 客户端，它开的会话把 `read` / `write` 工具
  指到**它自己的文件**上——路径以客户端的 `cwd` 为根，读写经 ACP 的
  `fs/read_text_file` / `fs/write_text_file` 回到客户端。实现是一个 seam
  （`uva-tool-plugins::files::FileSystemOwner`——「这个会话的文件在哪里、谁
  持有字节」，`LocalFileSystem` 是默认实现）、一个注册表
  （`SessionFileSystem`：会话 id → 客户端 owner，未认领的会话照旧跑部署的
  盘）与一个 owner（`ClientFileSystem`：持有连接与客户端声明的能力，未声明
  的能力是**明确的错误**而不是静默回退）。工具的渲染不变——模型看到的形状
  不因文件在哪台机器上而变。未声明能力的客户端一切照旧。
- **压缩作废了它推翻的签名思维块**（G5）：Anthropic 把思维块的签名绑定在
  它被生成时的请求前缀上，而压缩改写前缀——provider 会因此拒绝**整个请求**。
  本仓此前原样回送作废的签名，压缩后的第一轮请求就是一次 400。现在 harness
  在两处清理签名（开跑前按日志里的压缩行算边界；本轮压缩落盘后按压缩行的
  序号）：清空签名后该 part 变成无签名，provider 自己的规则就会丢掉它——
  **模型失去那段推理，回合不失去**。判定是粗的（压缩点之前那些前缀未变的
  签名也一并作废）：作废一个有效签名只是少一点上下文，保留一个作废签名会
  赔上一整个回合。

### Fixed (无头通道：出站帧不再卡在行缓冲里)

- **stdio 的出站帧自己 flush**：切 10 把出站拆成「编码器 + 写入器」两个
  任务时，写入器只 `write_all` 而**没有 flush**——而 stdout 是**行缓冲**
  的，于是帧停在写入器的缓冲里，直到**下一帧**到来才被带出去。对端于是
  看到的不是「这一帧」，而是「上一帧 + 这一帧」：一个只发一帧就等答案的
  客户端会一直等下去。表现最明显的地方正是无头面本身：ACP over core 的
  集成测试（`crates/uva-app-server/tests/acp_over_rpc.rs`）**挂在握手那一步**
  ——它发的握手帧得不到回答，直到它再发一帧。
  现在写入器每写一帧就 flush（帧是小的，一次系统调用换「对端立刻收到」
  是划算的），并补了一条**钉子**：测试用一个**带缓冲的**写入器 + 一根**不
  结束**的输入管道跑泵——没有 flush 时对端在超时里看不到那一帧 ✓。

### Changed (本轮的对账结论)

- `docs/reference-harnesses.md` 之外新增一份**缺口账本**：dsh / omp 的行为
  对账表、12 条缺口（G1–G12）、本轮三刀与下一轮候选（客户端侧 fs/terminal
  委派、签名思维块的前缀绑定、分块帧、缓存友好的压缩……）。

### Added (一个核心，多协议适配器：ACP 母语面 + AG-UI 投影)

- **核心只维护一套事件流**：会话、回合、工具调用、权限请求、取消。核心不
  说 ACP、AG-UI 或 A2A——它发布事件，适配器翻译。新的
  `uva-app-server::adapters` 模块是这一层的目录：`adapters/acp`（无头控制面
  的母语）与 `adapters/agui`（浏览器的那扇门）。
- **ACP 适配层改用官方 SDK**：协议的消息类型不再手写——依赖
  `agent-client-protocol`（workspace 依赖，`features = ["stdio"]`），握手、
  `session/new`、`session/load`、`session/prompt`、`session/cancel`、
  `session/update`、`session/request_permission` 的线上形状都是 SDK 的
  schema 类型。手写的 `wire.rs`（方法名常量、更新形状、stop reason、权限
  选项）与 `framing.rs`（自写行协议）已删除；仓库自己的映射只剩一处：核心
  事件 → SDK 的 `SessionUpdate`。
- **ACP 走两条传输，一个 agent**：stdio（SDK 的 `Stdio`，编辑器 / 终端直接
  拉起进程）与 WebSocket（经网关 `/rpc`）。后者的事实是：核心的 JSON-RPC
  通道同时承载两套词汇——命中 ACP 方法名的帧归适配层，「有 result 无
  method」的应答（agent 自己问出去的问题的答案）也归适配层，其余归核心
  router。网关补三条路由：记住「连接 ↔ 会话」的所有权、核心发起的请求按
  会话发给拥有它的连接、并记住该请求的 id 好让答案原路回来。
- **AG-UI 投影**：服务端把同一套核心事件投影成 AG-UI 事件
  （`RUN_STARTED` / `TEXT_MESSAGE_*` / `REASONING_*` / `TOOL_CALL_*` /
  `RUN_FINISHED` / `RUN_ERROR`），以 `agui/event` 通知发布，网关按会话投给
  订阅者。投影是有状态的（哪条消息开着、哪次运行活着），词汇表在
  `adapters/agui/event.rs`，可选字段一律不发 `null`（AG-UI 的「缺席即缺席」）。
- **云边界被声明而不是实现**：SaaS / 调度器按语义挂 A2A（把 Harness 当被
  调度 Agent）或 UHP（把服务当统一调度平台），不让远端直接说 ACP。仓内
  没有这两个协议的运行时依赖，本次也不实现——声明位置与理由见设计文档。
- **逆向证据落档**：[`docs/reference-harnesses.md`](docs/reference-harnesses.md)
  记录 dsh / pi / codex 的 ACP 行为与 dsh 插件树（entry 模型、group、mount、
  三态审计、按 id 的 diff）的出处，并逐条对到本仓的落点。

### Changed (ACP 面：无头 harness 的 agent harness protocol)

- **`uva-app-server acp`**：第三个模式，讲 **Agent Client Protocol v1**
  （agentclientprotocol.com），stdio 上一行一条 JSON-RPC。编辑器（Zed、
  JetBrains IDE）用它驱动本部署的同一套回合机器：prompt 仍然先落盘成
  `parked_prompt` 行，run 仍然在会话的 live 通道上发事件，审批仍然由
  harness 的闸门持有、由 durable 行与 `ResumeToken` 结算——只有请求与
  应答的形状是 ACP 的。
- **方法**：`initialize`（版本 1、能力、身份）、`authenticate`（拒绝：本
  部署没有认证方法）、`session/new`、`session/load`（回放转录为
  `session/update`）、`session/prompt`（增量流回，最后给停止理由）、
  `session/cancel`。服务端发给客户端的请求只有
  `session/request_permission`。
- **增量即 update**：模型的一小段文本是
  `agent_message_chunk`，推理是 `agent_thought_chunk`，工具调用是
  `tool_call`（pending）→ `tool_call_update`（in_progress / completed /
  failed），用量帧与警告没有 ACP 形状（durable 行仍然记着）。
- **被持有的调用变成一个问题**：ACP 面的客户端能回答权限问题，所以这个面
  装上部署的 `approval` 闸门（风险由工具自己判定，`Tool::risk`）——此前
  `approval` 行只发布策略、没有任何东西消费它，高风险调用从来不会被
  持有。权限选项是一次性的两项（`allow-once` / `reject-once`），与参考
  harness 相同；拒绝把工具错误交回模型，回合继续。
- **顺序**：被持有的调用**先结束回合**再问操作者——控制器在 run 还活着
  的时候拒绝 resume，先问会让答案被丢弃、回合永远不继续。
- **id 命名空间**：本仓发给客户端的请求用字符串 id（`uva-1`…），客户端
  在同一条会话上给自己的请求编号，共享数字计数器会让一边的应答被当成
  另一边的。
- **取消**：`session/cancel` 取消在跑的回合，停止理由是 `cancelled`；权限
  问题开着时取消，问题按协议以 `cancelled` 应答，回合立刻结束（不会等一
  个不会再来的事件）。

逆向与对账写在 [`docs/design/2026-11-25-protocol-layers-design.md`](docs/design/2026-11-25-protocol-layers-design.md)
与 [`docs/reference-harnesses.md`](docs/reference-harnesses.md)：DeepSeek
Harness（ACP 桥是薄投影，权限是一次性选项，无 `session/load`）、pi（实验性
harness 已删除，全仓没有 ACP）、codex（仓内没有 ACP；审批是服务端发起的
请求/应答，拒绝后回合继续）。

### Added (无头内核的原生回合流)

- **`chat/message/stream`**：无头内核（stdio JSON-RPC 2.0）第一次有了自己的流式门。
  请求与 `chat/message/send` 同形；回答 `{accepted, turn_id}`，该回合的增量以
  `chat/turn/event` 通知逐帧发布——`text` / `reasoning`（模型增量）、`tool_call` /
  `tool_progress` / `tool_result`、`usage` / `warning`、`gap`（live 帧被丢弃的
  诚实声明）、`end`（终帧，`completed` / `cancelled` / `suspended`）。帧按 `seq`
  从 1 编号。客户端不再需要 `sessions/follow` 的累积快照（O(n²) 字节、自己
  diff）或借道 Anthropic 方言。
- **一个回合一个 id**：回合 id 由提交方现铸，随 op 信封交给控制器，成为 durable
  `parked_prompt` 行的 `park_id` 与每帧 live 事件的标签——回答流、durable 转录、
  集群转发三方共用一个字符串。集群转发（`ControlFrame::ForwardOp`）携带它。
- **live 通道的条目换形**：`EventBroadcaster` 携带 `BroadcastEvent::{Run,
  TurnEnded}`（带 `turn_id`）；`TurnEnded` 在行 flush 之后发布，是 live 流里
  第一个回合级终信号。网关把 `chat/turn/event` 与 `session/event` 同表按
  `session_id` 路由。`sessions/follow` 的累积快照语义不动（TUI 与浏览器的
  渲染契约）。

见 `docs/adr/0030-one-id-per-turn-one-owner-per-the-live-channel.md`。

### Changed (一步的回合归一次折叠)

- **一步的回合只有一个主人**：Anthropic 与 OpenAI 的流处理器退回纯解码——正文、工具调用、
  停止原因与 usage 不再在解码器里累计，也不再由解码器构造完整回合；`BlockAssembler`
  是唯一折叠。词汇新增两块：推理签名块，与工具更名块（OpenAI 兼容网关把工具名放在
  后续分片时，名字现在能抵达回合）；停止块成为终止标记。
  `ModelProvider::complete_with_stream` 返回折叠的产物，流式路径不再复制非流式响应
  形状；`SamplingResult::into_completion_response` 删除。
- **流式 Anthropic 的 usage 变成真实值**：解码类型建模 `message_start.message.usage`
  与 `message_delta.usage`，映射与非流式路径逐桶一致（缓存读同时进 `cached_prompt`
  与 `cache_read`，缓存创建进 `cache_write`，provider 报零则保留零值）。在此之前，
  流式 Anthropic 步的 usage 读作零，用量接口、上下文压力锚点、压缩续跑门、Anthropic
  兼容响应与浏览器用量条都读这个零；历史行不补写。
- **OpenAI 兼容 provider 在完成标记之后到达的 usage 不再丢弃**：解码继续消费到流结束
  标记或 EOF，晚到的 usage 照常折进，且仍只终结一次。
- **think 抽取器的残余正文进入活动投影与回合**：此前它只塞进终态结果，而折叠在已有
  流式文本时忽略终态文本，于是通常情形下被丢弃。
- **在终止事件之前结束的流仍是错误**：错误类型从「空响应」改为点名的流协议错误；
  半个工具调用不会被派发。思维链在 Anthropic 上也不再逐帧累加用量——它按折叠后的
  末次合并值发布一次。

见 `docs/adr/0029-the-stream-fold-owns-one-step-s-turn.md`。

### Changed (拦截只有一张表)

- **一次运行只向一张拦截表派发**：`uva_hook::HookRegistry` 现在是唯一的注册与
  派发面。`MountPoint` 声明十二个挂载点（六个 pass、五个观察点、
  `prompt/sections`），`RegistrationOrigin` 决定来源车道与 panic 策略；一个派发
  入口服务 pass 与观察点。旧的七个字符串键、`HookPayload` 与链/残差三条派发路
  删除，不留别名。
- **五个观察点第一次对仓外注册可见**：`before_run` / `after_run` /
  `before_provider_call` / `on_error` / `on_compaction` 现在与 pass 点走同一张表；
  只依赖 `uva-hook` 的插件即可注册（`docs/examples/hook-only-consumer` 有断言）。
- **`Host` 退回 typed 策略表**：保留 guards、hints、trackers、output
  transformers 与 tool 绑定的组合点，并持有部署唯一的 registry
  （`hook_registry()`）。删除 `Host::{hook, gate, request_transformer,
  result_transformer, prompt_section}`、`Host::install`、`host::Plugin` 与
  `PluginHandle`；`Host::default_policy` 改为
  `default_policy(workspace, registry)`。
- **一个部署一个实例**：boot 先建 registry，默认策略写进它，`hooks` 行发布同一
  实例（热重挂复用），`Seams` 从 `Host` 读它。没有 `hooks` 行的部署仍有完整默认
  策略；没有部署表的嵌入方拿到一张空表。冻结基线随之移除
  `uva-harness::Plugin` / `uva-harness::PluginHandle`。

见 `docs/adr/0028-one-interception-table.md`。

### Changed (会话列表的默认顺序与页面校验)

- **`sessions/list` 省略 `sort` 时现在是「最新在前」** —— 这正是它自己的文档与 `SessionSummary`
  一直写着的默认（`-created_at`）。此前默认值只代入了白名单校验、没有代入排序，所以实际返回
  **最旧在前**；同一个应用的两个面因此打架：工作区树显式传 `-created_at`（最新在前），命令面板
  不传（最旧在前）。现在默认由键声明给出，两个面一致。
- **`sessions/list` 的空 cursor 现在是校验错误**：它的内层页面参数此前没有嵌套校验，所以
  `cursor: ""` 在这一条上返回第一页、在其余六条列表方法上返回 invalid-params。现在七条一致。

两条都记在 `docs/adr/0027-the-list-recipe-lives-in-one-module.md`。

### Changed (列表配方：代理、定时任务与技能的排序)

- **`agents` 不再接受 `sort=kind`**：白名单声明了它、排序代码从不读它，所以 `?sort=kind` 返回
  200 并按 name 排——一个在接口上承诺、在实现上撒手的键。现在它是校验错误（`invalid_item`），
  与其余未声明的键一致。
- **`schedules` 开始兑现方向**：三条键（`id` / `name` / `next_fire_at`）的 `-` 前缀此前通过
  校验、解析成降序、然后返回升序。现在 `?sort=-name`、`-id`、`-next_fire_at` 真的倒序；
  键相等的两行按 id 定序，翻页不再有重复或漏行的窗口。
- **`skills` 的 `created_at` 键有了 tiebreaker**：`created_at` 其实是定义文件的 mtime，同一
  毫秒写入的技能此前没有确定的顺序，页边界可能落在它们之间。现在按 name 定序。排序键不在线上
  行上，所以这一条路由排「(行, mtime)」再投影——信封为此新增 `List::map`（保留游标与总数）。

三条都记在 `docs/adr/0027-the-list-recipe-lives-in-one-module.md`。

### Changed (评测列表不再接受分页参数)

- **`evals/list` 不再假装分页**：它此前解析 `limit` / `cursor` / `sort` 之后把整页扔掉——
  三个参数全部无效，而带垃圾 cursor 的请求会 400。客户端本来一个参数都不传（前端发的是
  无参数请求），所以把分页参数从它的签名上摘掉：诚实地返回全量，`next_cursor` 恒空。
  唯一可观测的变化是带垃圾 cursor 的请求从 400 变成被忽略，与「这条路由不分页」一致。
  记在 `docs/adr/0027-the-list-recipe-lives-in-one-module.md`。

### Changed (uva-core 的分页原语收取键闭包)

- **列表的切片算术只剩一份实现**：`uva_core::registry::paginate_registry_list` 以前被
  `RegistryItem` 约束拴住，只为了取 `item.name()` 当游标键；现在改收一个取键闭包、对行类型
  无约束。于是「行是线上类型、没有 registry 元数据」的列表（会话摘要、定时任务行）可以复用
  同一份游标算术，而不是各自抄一份。两个生产 registry 传 `|entry| entry.name()`，行为逐字节
  不变（游标、`next_cursor`、`total`、被删游标的空页）。名字与导入路径不动，`uva-core` 的
  冻结面计数不变；形状变更记在 `docs/api-frozen.md` 的 *Recorded shape changes*。见
  `docs/adr/0027-the-list-recipe-lives-in-one-module.md`。

### Changed (开发手册：MCP 浏览器已在本机)

- **别再重下 playwright**：`~/.cache/ms-playwright/` 里已经躺着 `chromium-1252`
  （由 `@playwright/mcp@0.0.84` 注册），而本机到 Chrome for Testing 的 CDN 不通——
  `install-browser` 必然失败，浏览器本来就在盘上。MCP 报的 revision 与盘上的不一致时
  （换过 MCP 版本就会变），把已缓存的 revision 软链成它要的名字即可。AGENTS.md §4.2
  写下了这几条。

### Changed (published row 的线上形状)

- **行的线上形状归一个主人**：published row 此前**没有主人**——路由写的是裸
  `serde_json::Value`，契约靠一份手写的 flat 9 成员 `SessionTurnEnvelope` 去描述它，
  而行词汇的 33 个变体一共能写 **51 个** 成员。差的那 42 个**都在线上**（它们本来
  就是类型化字段），于是浏览器只能去读 payload 包里的**第二份拼写**
  （`data.kind` / `data.step` / `data.child_session_id` …）。现在：
  - envelope 补成 published row 的**完整超集**（全 optional），成员清单**从词汇派生**
    ——`SessionEvent` / `AttachmentRefRow` / `UsageBuckets` 派生 `JsonSchema`
    （`uva-session` 早就依赖 `schemars`），由一条契约测试断言 envelope 覆盖全部变体的
    字段名。没有第二份手写清单要维持，只有一处比较。
  - 浏览器读**顶层类型化成员**，payload 里的同名键只作**旧行兜底**（这些字段是后加的，
    早于它们的行只在 payload 袋里带着一份）。
  - `publish` **不**归一旧行：published row 仍是原始行。
  - 记在 `docs/adr/0026-the-row-envelope-covers-every-member-the-vocabulary-writes.md`。
- **插件事件在浏览器里不再显示成 `unnamed`**：`Plugin` 变体**没有 `data` 字段**（只有
  `kind` / `payload` / `original_bytes`），而浏览器读的是 `entry.data`——于是**每一个
  插件事件都渲染成「插件事件 \*\*unnamed\*\*」且没有正文**。现在读行自己的字段。

### Changed (工具砖块的注册生命周期)

- **工具砖块的注册生命周期归一个主人**：九处砖块各自手写同一段「解析服务 → 注册 →
  拒绝要响 → 克隆 token 回滚 → file 一个撤下 effect」，`read` / `write` / `todo` /
  `web` / `shell` 五处**逐字节相同**；`search` 的拒绝措辞还漏了工具名。现在这段生命周期
  归 `uva_tool::register_tool_entries`（`uva-kernel` 不能放——`check-kernel-deps` 禁它
  依赖 `uva-tool`），各砖块只声明自己的 manifest 与来源（Core / Dynamic）。**部分回滚**
  这条路径此前一个测试都没有，现在有了。
- **GLOSSARY 收了「砖块（brick）」**：装配里负责把一样东西变成可调用的一行——挂载时登记，
  卸载时撤干净；一个插件可以既当砖块又提供服务。

### Removed (循环只有 react)

- **删除三个推理模式**：`uva-policy-plugins` 的 `cot` / `plan_execute` /
  `best_of_n` 模块、它们的 feature、facade 的 `strategy-cot` /
  `strategy-plan-execute` / `strategy-best-of-n` 与三个 example 一并删除。
  每一样都能用 react 组合出来——思维链是提示工程，计划-执行是 react + 计划
  工具，多候选是 react 跑 n 次再评分——三份实现与三份测试换来的能力为零。
- **`ReasoningStrategy` 范式保留**：`with_strategy` 与 `StrategyRegistry`
  不动，仓外实现照常注册；内置解析表只剩 `react` / `re-act` / `re_act`，
  别名无效时启动报点名的「未知策略」错误。
- **`CandidateScorer` 随 `best_of_n` 删除**：它唯一的使用者是该策略。

见 `docs/adr/0031-the-loop-is-react-composition-replaces-modes.md`。

### Fixed (web 镜像缺失；生产 compose 缺失)

- **web 镜像回来了**：`Dockerfile.web` 在 `19d0cf54`（一个讲委派 id 的无关提交）里被
  删掉，但 `Makefile` 的 `image-build-web`、`scripts/images.sh` 与 `k8s/web.yaml` 一直
  引用它——`make image-build` / `make image-push` 必然失败。现在镜像重建：node 构建
  前端静态包，nginx 运行时提供静态资源并反代服务端的 HTTP 面（`/rpc` 的 websocket、
  `/livez`、`/readyz`、`/version`、`/metrics`）。镜像以非 root（65532）跑、只读根
  文件系统、监听 8080（>1024，故无需任何 capability 或特权端口），nginx 要写的路径
  全在 `/tmp` 下；`k8s/web.yaml` 与 `docker-compose.prod.yml` 都挂了可写的 `/tmp`。
- **`docker-compose.prod.yml` 补回来了**：`make docker-prod-up` 指向的文件不存在，
  该目标必然失败。现在是两个服务：`app-server`（服务端镜像）与 `web`（nginx 镜像）。
- **文档与现实对齐**：`DEPLOYMENT.md` 的 nginx 例子、生产拓扑、Anthropic 客户端小节
  与 compose 表此前描述的是一套已经不存在的 REST 面（`/api/*`、`/v1/messages`）——
  服务端今天只有 `/rpc`（websocket）与四个探针/观测路径，Anthropic 兼容面是
  `anthropic/messages` 这个 RPC 方法。

### Fixed (`/metrics` 恒空；e2e 脚本时好时坏）

- **`/metrics` 不再是空的**：Prometheus 的 RED 计数此前没有任何喂数方——HTTP 家族
  （`http_requests_total` / `http_requests_duration_seconds`）全仓没有写入点，工具家族
  （`tool_executions_total` 等）只在 `uva-tool` 的注册表分发里 +1，而 agent loop 自己
  直接抽 `Tool::stream`、绕开了那条路。于是运行中的服务端 `/metrics` 永远返回 200 +
  空 body。现在两端都记账、再合流：
  - **网关**加了一层中间件，把每个请求按 **method + 匹配的路由模板** 记进两个 HTTP
    家族（模板而非原始 URI，免得每个 session id 自成一串；前端静态资源走 fallback，
    统一记在 `unmatched` 一串下）；
  - **内核**（agent loop 所在的那个进程）由 loop 的工具分发补记工具计数与耗时
    （ok / error / panic，与注册表那条路同义），两条路各记各的，同一次调用不会被数
    两次；
  - **网关**用新增的 `metrics/gather` 方法向内核要它那份 exposition 并拼进
    `/metrics`——两个进程的家族互不重叠（内核没有 HTTP 面），拼接仍是合法的
    exposition。
- **`scripts/verify-harness-e2e.sh` 不再时好时坏**：它只从 assistant 行的 `Multi`
  形状里取答复，而模型的答复有时是 `Single`（单段文本）——那几次它会把答复读成空串并
  误报失败。它等的回合结束键也是错的（`phase`，实际是 `kind`），于是每跑一次都要空等
  45 秒的读超时。两处都改了：两种形状都读，结束键按 `kind` 判。

### Fixed (两份过期的 lockfile)

- **`docs/examples/hook-only-consumer/Cargo.lock` 与 `desktop/Cargo.lock` 都不再匹配
  各自的清单**：`cargo build --locked` 直接失败（`cannot update the lock file …
  because --locked was passed`），而任何不带 `--locked` 的 cargo 命令都会顺手把
  工作树弄脏。刷新后 `--locked` 可构建，树保持干净。

### Fixed (三处门禁红：范式基线、公开文档计数、私有注释预算)

- **`check-pub-surface-strict` 红了**：`crates/uva-hook/src/lib.rs` 有 8 条 `pub use`，
  基线是 7。第 8 条是 `bc4c8138` 加的 `pub use arbitration::{…}`——拦截裁决的公开词汇，
  属有意为之。按门禁自带的处方把基线提到 8（而不是删掉那条 re-export）。
- **`check-public-docs` 红了**：`uva-session` 有 182 个未文档化 pub 项，基线 173。多出
  的 10 个都在 `typed_event_builders.rs`——它们**本来就有 doc**，但 `#[must_use]` 卡在
  doc 块与声明之间，门禁的分类器看不到。把属性移到 doc 块之上（该文件较新的 6 处写法
  就是这样，也已是仓内 25 个文件的既有惯例），计数降到 172。
- **`check-harness-docs` 红了**：`crates/uva-harness/src/agent/host/tests.rs` 有 1 行
  私有项 doc，预算 0。降级为 `//`——与该文件其它私有项注释一致。

### Changed (行载荷形状归一个主人：载荷包与类型化字段由构造保证一致)

- **一行的载荷包与类型化字段由一个构造器写下**：助手轮结束行的构造器
  `uva_session::turn_end` 现在接受一个可选的失败原因（`Option<&str>`），一次写出载荷包与
  类型化字段；harness 改从构造器要那一行，不再在行归属的 crate 之外事后补
  `data["message"]`——那一处改载荷曾让「类型化字段与载荷包可以不一致」成为可能。同一裁定
  下，修理器与压缩器不再手拼变体字面量，改问构造器；表面投影（`log_surface`）补齐出来的
  四行同样改问构造器——六个标签的构造器模块是行形状唯一的主人，两个主人文件之外无人
  拼变体。
- **公开面移除**：`uva_session::SessionEvent::plugin`（`events/mod.rs` 里的那个公开关联
  函数）已删除；插件行改由 `uva_session::plugin_row` 构造——同签名同语义（同样的载荷裁剪
  与 `original_bytes` 规则）。
- **新增六个公开构造器**：`compaction_start` / `compaction_summary` / `compaction_row` /
  `compaction_end_committed` / `compaction_end_failed` / `plugin_row`（`tool_result_interrupted`
  是 crate 私有的）。另一个常量随后补进 crate 根：`STEP_INTERRUPTED`——崩溃补齐的步结束行
  写进载荷的 `action` 值，仓外读者按根级名字匹配它。冻结面 `uva-session` 一共
  147 → 154 names，`docs/api-frozen.md` 与基线已同步。
- **行为变更一：崩溃补齐出来的步结束行多一个 `action` 键**（值 `interrupted`），与正常写出
  的步结束行同载荷；既有日志不受影响（新写的才有），旧日志照旧可读。
- **行为变更二：`interrupted_turn_closers`（公开 API）返回的 closer 不再带最后一条事件的
  时间戳**——`ts` 为空串、`seq` 为 0；落盘时 writer 本来就重盖 `ts` 与 `seq`，落盘字节
  不受影响。
- **轮结束行的线上字节不变**：带原因的载荷仍是 `kind` / `turn` / `reason` / `message`
  四个键（顺序不变），不带原因时仍只有 `kind` / `turn` / `reason` 三个键；插件行的字段、
  裁剪与序号语义逐字保留；既有会话日志继续按今天的形状读。
- **门禁多守一条并收窄**：`check-row-vocabulary` 新增「载荷扫描」半张——除构造器模块外，
  任何生产文件（`uva-session` 内部也在内）不得把键写进一行的载荷包（`data[...] =` /
  `payload[...] =` 这类索引赋值）；构造判别同时认得跨行的 match 臂与 **match 臂体里的**
  构造（收尾行带 `=>` 才算 match 臂）。例外表与棘轮照旧，只许变短。
- **破坏性变更（项目未到 1.0）**：`turn_end` 的签名从 `(u32, &str)` 变为
  `(u32, &str, Option<&str>)`；冻结基线按名字比较，名字未变。
- 决策记录见 docs/adr/0021-row-payload-shape-in-one-owner.md。

### Changed (一轮子会话归一个模块)

- **名册与进度行的结局词归一（行为变更）**：一轮子会话的结局只有一套词——名册的活动
  行、L1 进度行的收尾摘要、子会话的 `subagent_exit` 脚写同一个词，词表由
  `RoundOutcome::as_str()` 一处给出。名册与进度行原先写 `finished` / `failed`，现在写
  `completed` / `error`（`cancelled` 不变）。原因轴不动：模型可见的失败文案说的仍是
  原因（`max_iterations` / `suspended` / …），不是结局词。
- **唤醒路径也会问上限了（行为变更）**：`agents_message` 在**发起轮次的那一方**（调用
  方）坐在上限层时拒绝新起一轮，文案逐字是「cannot start another round: maximum
  sub-agent depth (3) reached; the `agents_message` tool is disabled at this nesting
  level」——唤醒路径原先根本不问上限。拒绝只覆盖**要起轮次**的分支（追问、冷唤醒）：
  steer 不起轮次，不被这条规则拒。被拒的行原样留在原地，同伴一次都不被叫醒。
- **两条路的结果同形（行为变更）**：委派路径与唤醒路径的结果都带
  `[subagent session: <id>]` 句柄行，失败文案逐字相同。委派路径的失败文案里句柄行前
  **由单换行变双换行**：委派路径原先自己拼那句（一个 `\n`），现在两条路共用轮次模块
  拼的 `\n\n`。
- **失败文案带上失败详情（行为变更）**：结束原因自带的详情（例如 `the peer gave up`）
  现在出现在模型读到的失败文案里——原先只知道「失败」，不知道「为什么」。原因词本身
  不换：它是比结局细的一根轴。
- 深度规则的主体是**发起轮次的那一方**（`task` 的调用方 / `agents_message` 的调用
  方），不是孩子：已在上限层的子会话被唤醒时**不被拒**（树没有变深），调用方在上限层
  时被拒，即使它要唤醒的孩子也在上限层。
- 公开面：`ControlRegistry::kill` 收进 crate 内，公开的 `ControlSurface::kill` 是唯一
  的 kill 入口——它顺带清掉终态子会话的深度条目（原先那条条目留到进程结束）。
  `uva-tool-task` 的 `pub use` 计数不变（7），模型面（`task` 工具的 JSON 形状）不变。
- 决策记录见 docs/adr/0022-one-module-owns-a-child-round.md。

### Changed (工具参数词汇归一个主人)

- **`task` 的四个模型可见形状事实（schema 变更）**：`task` 工具的每个参数
  由一份声明拥有（`uva-tool-task` 的 `args` 模块），模型读到的 schema 与解析器
  都是它的投影。模型读到的 schema 因此多出四句实话：`prompt` 是必填字符串
  （缺失 / 非字符串的调用**被拒**，文案 `task tool requires a string \`prompt\`
  field`——解析器过去把缺失与非字符串静默当成空串）；`agent` 写明
  `minLength: 1`（空名解析器早就拒）；`gate.command` 数组写明 `minItems: 1`
  （空数组早就拒）；`gate` 与 `isolation` 写明 `oneOf`——命令对象或**一条裸
  命令**（一个程序名，不做 shell 拆分）、布尔或**一个不带成员的对象**，两者
  都含 `null`。裸命令与对象形态本就被收着，schema 只是如实说出来；`prompt`
  那条是唯一的解析端行为变更（变严，落在 schema 一直许下的承诺范围内）。
- **被删字段被拒即硬失败（行为变更）**：`trace_context` 与
  `gate.timeout_secs` 从声明里消失（它们被解析了整个存在期，却没有读者）。
  旧 schema 明确广告过 `gate.timeout_secs`——照旧 schema 缓存的模型会撞上：
  带它们的调用现在**整次委派失败**，文案说明理由（「`gate.timeout_secs` is
  no longer an argument of the `task` tool: …」），不再是「解析进来然后静默
  无效」。其它从未声明过的键照旧被丢掉。
- **显式 `null` 按「没带」处理**：`"trace_context": null` 或
  `"gate": {"timeout_secs": null}` 的调用**不**触发上面的拒绝——`null` 没有
  信息量，旧解析遇到它也返回「没有」，拒绝它只会把一次成功的调用变成硬失败。
- **公开面**：`TaskTraceContext` 与 `TaskSpec::trace_context` 删除；
  `task_tool_definition` / `task_tool_definition_with_features` 导出删除；
  `GateSpec::timeout` 字段与 `with_timeout` 构造器删除（生产代码永远填不上
  它、`run_gate` 也不执行它）；`uva-tool-task` 的 `pub use` 计数 7 → 6
  （基线同步）。
- 决策记录见 docs/adr/0023-one-owner-for-the-tool-arguments.md。

### Changed (一轮子会话归一个模块)

- **名册与进度行的结局词归一（行为变更）**：一轮子会话的结局只有一套词——名册的活动
  行、L1 进度行的收尾摘要、子会话的 `subagent_exit` 脚写同一个词，词表由
  `RoundOutcome::as_str()` 一处给出。名册与进度行原先写 `finished` / `failed`，现在写
  `completed` / `error`（`cancelled` 不变）。原因轴不动：模型可见的失败文案说的仍是
  原因（`max_iterations` / `suspended` / …），不是结局词。
- **唤醒路径也会问上限了（行为变更）**：`agents_message` 在**发起轮次的那一方**（调用
  方）坐在上限层时拒绝新起一轮，文案逐字是「cannot start another round: maximum
  sub-agent depth (3) reached; the `agents_message` tool is disabled at this nesting
  level」——唤醒路径原先根本不问上限。拒绝只覆盖**要起轮次**的分支（追问、冷唤醒）：
  steer 不起轮次，不被这条规则拒。被拒的行原样留在原地，同伴一次都不被叫醒。
- **两条路的结果同形（行为变更）**：委派路径与唤醒路径的结果都带
  `[subagent session: <id>]` 句柄行，失败文案逐字相同。委派路径的失败文案里句柄行前
  **由单换行变双换行**：委派路径原先自己拼那句（一个 `\n`），现在两条路共用轮次模块
  拼的 `\n\n`。
- **失败文案带上失败详情（行为变更）**：结束原因自带的详情（例如 `the peer gave up`）
  现在出现在模型读到的失败文案里——原先只知道「失败」，不知道「为什么」。原因词本身
  不换：它是比结局细的一根轴。
- 深度规则的主体是**发起轮次的那一方**（`task` 的调用方 / `agents_message` 的调用
  方），不是孩子：已在上限层的子会话被唤醒时**不被拒**（树没有变深），调用方在上限层
  时被拒，即使它要唤醒的孩子也在上限层。
- 公开面：`ControlRegistry::kill` 收进 crate 内，公开的 `ControlSurface::kill` 是唯一
  的 kill 入口——它顺带清掉终态子会话的深度条目（原先那条条目留到进程结束）。
  `uva-tool-task` 的 `pub use` 计数不变（7），模型面（`task` 工具的 JSON 形状）不变。
- 决策记录见 docs/adr/0022-one-module-owns-a-child-round.md。

### Changed (会话解析归一个模块)

- **regenerate / feedback 的 miss 路径从「意外目录」变为「显式完整创建」（行为变更）**：
  「按需创建」成为状态层会话解析模块的有名策略（`create_on_demand`），聊天面按名字
  要它；miss 路径（查不到可重放的 user turn / 可评分的目标行）的副作用从「sink 顺手
  `create_dir_all` 出一个没人认领的目录」变为策略的全部契约——目录、登记条目、控制器
  一起 mint。直接答复不变：regenerate 仍答 404 `no user turn to regenerate in this
  session`，feedback 仍答 `feedback_target_unknown`。**边界**：miss 之后该 id 已被建出，
  `sessions/status` 与 `sessions/events` 对它从 404 变为可见（status 可能仍答 404，理由
  变为「尚无快照」）。
- **两种失败共存时赢家变了（行为变更）**：「日志 schema 不支持 + 无 user turn」的
  regenerate 从答 404 no-user-turn 变为答 schema Session 错误——控制器**先开**，日志后
  读，拒绝先到。新的优先级更诚实：「这个 build 打不开这份日志」是关于会话的事实。
- **路径知识与行解析归一**：`transcript_path` / `user_sessions_dir` /
  `is_path_component` 与行解析原语 `transcript_events` / `transcript_lines` 的唯一住所在
  会话解析模块；路由不再拼路径，follow 不再跨模块调会话路由，`session_index` 改问模块。
  磁盘布局本身仍是 `uva-session` 的财产，本次只搬「位置在哪、行怎么读」的知识。
- **读面的拒绝绝不 mint**：「必须存在」同样是有名策略（`must_exist`，登记表或转录目录
  任一即为存在）；follow / resume / compact 对未知会话仍逐字答 `not found: session
  '<id>'`，且不建目录。
- 决策记录见 docs/adr/0024-session-resolution-in-one-module.md。

### Changed (工具参数词汇归一个主人)

- **`task` 的四个模型可见形状事实（schema 变更）**：`task` 工具的每个参数
  由一份声明拥有（`uva-tool-task` 的 `args` 模块），模型读到的 schema 与解析器
  都是它的投影。模型读到的 schema 因此多出四句实话：`prompt` 是必填字符串
  （缺失 / 非字符串的调用**被拒**，文案 `task tool requires a string \`prompt\`
  field`——解析器过去把缺失与非字符串静默当成空串）；`agent` 写明
  `minLength: 1`（空名解析器早就拒）；`gate.command` 数组写明 `minItems: 1`
  （空数组早就拒）；`gate` 与 `isolation` 写明 `oneOf`——命令对象或**一条裸
  命令**（一个程序名，不做 shell 拆分）、布尔或**一个不带成员的对象**，两者
  都含 `null`。裸命令与对象形态本就被收着，schema 只是如实说出来；`prompt`
  那条是唯一的解析端行为变更（变严，落在 schema 一直许下的承诺范围内）。
- **被删字段被拒即硬失败（行为变更）**：`trace_context` 与
  `gate.timeout_secs` 从声明里消失（它们被解析了整个存在期，却没有读者）。
  旧 schema 明确广告过 `gate.timeout_secs`——照旧 schema 缓存的模型会撞上：
  带它们的调用现在**整次委派失败**，文案说明理由（「`gate.timeout_secs` is
  no longer an argument of the `task` tool: …」），不再是「解析进来然后静默
  无效」。其它从未声明过的键照旧被丢掉。
- **显式 `null` 按「没带」处理**：`"trace_context": null` 或
  `"gate": {"timeout_secs": null}` 的调用**不**触发上面的拒绝——`null` 没有
  信息量，旧解析遇到它也返回「没有」，拒绝它只会把一次成功的调用变成硬失败。
- **公开面**：`TaskTraceContext` 与 `TaskSpec::trace_context` 删除；
  `task_tool_definition` / `task_tool_definition_with_features` 导出删除；
  `GateSpec::timeout` 字段与 `with_timeout` 构造器删除（生产代码永远填不上
  它、`run_gate` 也不执行它）；`uva-tool-task` 的 `pub use` 计数 7 → 6
  （基线同步）。
- 决策记录见 docs/adr/0023-one-owner-for-the-tool-arguments.md。

### Changed (一轮子会话归一个模块)

- **名册与进度行的结局词归一（行为变更）**：一轮子会话的结局只有一套词——名册的活动
  行、L1 进度行的收尾摘要、子会话的 `subagent_exit` 脚写同一个词，词表由
  `RoundOutcome::as_str()` 一处给出。名册与进度行原先写 `finished` / `failed`，现在写
  `completed` / `error`（`cancelled` 不变）。原因轴不动：模型可见的失败文案说的仍是
  原因（`max_iterations` / `suspended` / …），不是结局词。
- **唤醒路径也会问上限了（行为变更）**：`agents_message` 在**发起轮次的那一方**（调用
  方）坐在上限层时拒绝新起一轮，文案逐字是「cannot start another round: maximum
  sub-agent depth (3) reached; the `agents_message` tool is disabled at this nesting
  level」——唤醒路径原先根本不问上限。拒绝只覆盖**要起轮次**的分支（追问、冷唤醒）：
  steer 不起轮次，不被这条规则拒。被拒的行原样留在原地，同伴一次都不被叫醒。
- **两条路的结果同形（行为变更）**：委派路径与唤醒路径的结果都带
  `[subagent session: <id>]` 句柄行，失败文案逐字相同。委派路径的失败文案里句柄行前
  **由单换行变双换行**：委派路径原先自己拼那句（一个 `\n`），现在两条路共用轮次模块
  拼的 `\n\n`。
- **失败文案带上失败详情（行为变更）**：结束原因自带的详情（例如 `the peer gave up`）
  现在出现在模型读到的失败文案里——原先只知道「失败」，不知道「为什么」。原因词本身
  不换：它是比结局细的一根轴。
- 深度规则的主体是**发起轮次的那一方**（`task` 的调用方 / `agents_message` 的调用
  方），不是孩子：已在上限层的子会话被唤醒时**不被拒**（树没有变深），调用方在上限层
  时被拒，即使它要唤醒的孩子也在上限层。
- 公开面：`ControlRegistry::kill` 收进 crate 内，公开的 `ControlSurface::kill` 是唯一
  的 kill 入口——它顺带清掉终态子会话的深度条目（原先那条条目留到进程结束）。
  `uva-tool-task` 的 `pub use` 计数不变（7），模型面（`task` 工具的 JSON 形状）不变。
- 决策记录见 docs/adr/0022-one-module-owns-a-child-round.md。

### Changed (TUI 的 wire 形状归一个主人)

- **页大小 50 → 服务端默认（20）（行为变更）**：目录域列表与侧栏会话列表
  都改问服务端自己的默认（`api::v1::DEFAULT_LIMIT` 与
  `routes::search::DEFAULT_LIMIT`），终端不再有自己的页大小。终端此前
  每页要 50 行——服务端从没见过这个数。
- **活动投影里的工具调用现在入屏（行为变更）**：服务端活动投影里的
  `tool_calls` 不再被终端丢弃。此前终端抄的 live 帧没有这一项，跑着的
  工具调用在终端上不可见，而浏览器看得见。
- **漂移的 follow 帧从「半应用」变为「整帧不应用 + 状态行」（行为变更）**：
  解不开的帧整个不应用，状态行写明「unreadable」；目录列表的信封解不开
  也一样**报出来**，不再静默显示 0 行（无游标、无总数）——「形状漂了」
  与「这个域没有东西」不再长得一模一样。
- **终端新增对 `uva-app-server` 的直接依赖**：wire 形状归服务端一个主人
  ——follow 基线、活动投影、schema 树、方法名都用服务端的类型，终端不留
  副本。这条边**只链接 wire 类型**（实测：二进制里 `uva_app_server` 符号
  1116 个，`axum::routing` / `reqwest::` / `AppState` 各 0 个——服务端的
  运行时没有被拉进来），代价是 wire 类型自己的代码（release 二进制
  +2.2%）。
- **删掉的死路径 `sessions/events`**：终端那一侧的读法与服务端从不对上
  （服务端答的是对象，终端当数组读），也没有调用点——路径连同它唯一的
  DTO 一起删净。
- 决策记录见 docs/adr/0025-the-tui-uses-the-server-wire-types.md。

### Changed (会话解析归一个模块)

- **regenerate / feedback 的 miss 路径从「意外目录」变为「显式完整创建」（行为变更）**：
  「按需创建」成为状态层会话解析模块的有名策略（`create_on_demand`），聊天面按名字
  要它；miss 路径（查不到可重放的 user turn / 可评分的目标行）的副作用从「sink 顺手
  `create_dir_all` 出一个没人认领的目录」变为策略的全部契约——目录、登记条目、控制器
  一起 mint。直接答复不变：regenerate 仍答 404 `no user turn to regenerate in this
  session`，feedback 仍答 `feedback_target_unknown`。**边界**：miss 之后该 id 已被建出，
  `sessions/status` 与 `sessions/events` 对它从 404 变为可见（status 可能仍答 404，理由
  变为「尚无快照」）。
- **两种失败共存时赢家变了（行为变更）**：「日志 schema 不支持 + 无 user turn」的
  regenerate 从答 404 no-user-turn 变为答 schema Session 错误——控制器**先开**，日志后
  读，拒绝先到。新的优先级更诚实：「这个 build 打不开这份日志」是关于会话的事实。
- **路径知识与行解析归一**：`transcript_path` / `user_sessions_dir` /
  `is_path_component` 与行解析原语 `transcript_events` / `transcript_lines` 的唯一住所在
  会话解析模块；路由不再拼路径，follow 不再跨模块调会话路由，`session_index` 改问模块。
  磁盘布局本身仍是 `uva-session` 的财产，本次只搬「位置在哪、行怎么读」的知识。
- **读面的拒绝绝不 mint**：「必须存在」同样是有名策略（`must_exist`，登记表或转录目录
  任一即为存在）；follow / resume / compact 对未知会话仍逐字答 `not found: session
  '<id>'`，且不建目录。
- 决策记录见 docs/adr/0024-session-resolution-in-one-module.md。

### Changed (工具参数词汇归一个主人)

- **`task` 的四个模型可见形状事实（schema 变更）**：`task` 工具的每个参数
  由一份声明拥有（`uva-tool-task` 的 `args` 模块），模型读到的 schema 与解析器
  都是它的投影。模型读到的 schema 因此多出四句实话：`prompt` 是必填字符串
  （缺失 / 非字符串的调用**被拒**，文案 `task tool requires a string \`prompt\`
  field`——解析器过去把缺失与非字符串静默当成空串）；`agent` 写明
  `minLength: 1`（空名解析器早就拒）；`gate.command` 数组写明 `minItems: 1`
  （空数组早就拒）；`gate` 与 `isolation` 写明 `oneOf`——命令对象或**一条裸
  命令**（一个程序名，不做 shell 拆分）、布尔或**一个不带成员的对象**，两者
  都含 `null`。裸命令与对象形态本就被收着，schema 只是如实说出来；`prompt`
  那条是唯一的解析端行为变更（变严，落在 schema 一直许下的承诺范围内）。
- **被删字段被拒即硬失败（行为变更）**：`trace_context` 与
  `gate.timeout_secs` 从声明里消失（它们被解析了整个存在期，却没有读者）。
  旧 schema 明确广告过 `gate.timeout_secs`——照旧 schema 缓存的模型会撞上：
  带它们的调用现在**整次委派失败**，文案说明理由（「`gate.timeout_secs` is
  no longer an argument of the `task` tool: …」），不再是「解析进来然后静默
  无效」。其它从未声明过的键照旧被丢掉。
- **显式 `null` 按「没带」处理**：`"trace_context": null` 或
  `"gate": {"timeout_secs": null}` 的调用**不**触发上面的拒绝——`null` 没有
  信息量，旧解析遇到它也返回「没有」，拒绝它只会把一次成功的调用变成硬失败。
- **公开面**：`TaskTraceContext` 与 `TaskSpec::trace_context` 删除；
  `task_tool_definition` / `task_tool_definition_with_features` 导出删除；
  `GateSpec::timeout` 字段与 `with_timeout` 构造器删除（生产代码永远填不上
  它、`run_gate` 也不执行它）；`uva-tool-task` 的 `pub use` 计数 7 → 6
  （基线同步）。
- 决策记录见 docs/adr/0023-one-owner-for-the-tool-arguments.md。

### Changed (一轮子会话归一个模块)

- **名册与进度行的结局词归一（行为变更）**：一轮子会话的结局只有一套词——名册的活动
  行、L1 进度行的收尾摘要、子会话的 `subagent_exit` 脚写同一个词，词表由
  `RoundOutcome::as_str()` 一处给出。名册与进度行原先写 `finished` / `failed`，现在写
  `completed` / `error`（`cancelled` 不变）。原因轴不动：模型可见的失败文案说的仍是
  原因（`max_iterations` / `suspended` / …），不是结局词。
- **唤醒路径也会问上限了（行为变更）**：`agents_message` 在**发起轮次的那一方**（调用
  方）坐在上限层时拒绝新起一轮，文案逐字是「cannot start another round: maximum
  sub-agent depth (3) reached; the `agents_message` tool is disabled at this nesting
  level」——唤醒路径原先根本不问上限。拒绝只覆盖**要起轮次**的分支（追问、冷唤醒）：
  steer 不起轮次，不被这条规则拒。被拒的行原样留在原地，同伴一次都不被叫醒。
- **两条路的结果同形（行为变更）**：委派路径与唤醒路径的结果都带
  `[subagent session: <id>]` 句柄行，失败文案逐字相同。委派路径的失败文案里句柄行前
  **由单换行变双换行**：委派路径原先自己拼那句（一个 `\n`），现在两条路共用轮次模块
  拼的 `\n\n`。
- **失败文案带上失败详情（行为变更）**：结束原因自带的详情（例如 `the peer gave up`）
  现在出现在模型读到的失败文案里——原先只知道「失败」，不知道「为什么」。原因词本身
  不换：它是比结局细的一根轴。
- 深度规则的主体是**发起轮次的那一方**（`task` 的调用方 / `agents_message` 的调用
  方），不是孩子：已在上限层的子会话被唤醒时**不被拒**（树没有变深），调用方在上限层
  时被拒，即使它要唤醒的孩子也在上限层。
- 公开面：`ControlRegistry::kill` 收进 crate 内，公开的 `ControlSurface::kill` 是唯一
  的 kill 入口——它顺带清掉终态子会话的深度条目（原先那条条目留到进程结束）。
  `uva-tool-task` 的 `pub use` 计数不变（7），模型面（`task` 工具的 JSON 形状）不变。
- 决策记录见 docs/adr/0022-one-module-owns-a-child-round.md。

### Changed (拦截裁决归一个主人：一条规则，三个 adapter)

- **fail-closed 在 host 路径成立（行为变更）**：「一个注册在挂载点上的决定意味着什么」
  被写了三遍且互相矛盾——同一个答不上来的门，注册表路径终止运行、host 路径降级为
  Allow 继续跑（host 自己的注释还写着 the pass fails closed）。现在 `uva-hook` 新增
  `arbitration` 模块作为唯一 owner：合法性（点的表）、fail-closed 合成（能拒绝就拒绝：
  Block 优先于 Terminate；`llm/*` 没有拒绝可用就报告）、折叠（`Arbitration::absorb`
  从 registry.rs 搬来）、两条 panic 策略（外部插件保活、进程内注册撤除，host 内部
  hint 的第三种收掉）。注册表与 host 的三条派发路径全部改问 owner；host 路径的门失败
  从「什么都不落」变为 durable 的 hook warning 行（链上的事实随载荷走，seam 层落行）。
- **不接受的裁决不再静默（行为变更）**：注册说出点不接受的话，以 `Fact`（一个形状、
  两个面共用）落成 warning 行，运维看得见。
- **hint 类注册 panic 后被撤除（行为变更）**：原先只跳过、每次都再试。
- `RegistryPass.failures: Vec<String>` → `facts: Vec<Fact>`；`uva-harness` 删除
  `Host::accept` 与 `DiagnosticKind::{UnsupportedDecision, GateFailed}`（裁决类诊断的
  形状现在是 owner 的 `Fact`）。冻结基线按名字比较，没有名字移动。
- 两条回归测试（改动前失败/后通过）钉住 fail-closed 与「注册表与 host 对同一失败给出
  同一裁决」。决策记录见 docs/adr/0020-interception-arbitration-in-one-owner.md。

### Changed (控制器的句柄不暴露锁：门是承诺，不是机制)

- **压缩门变成"工作"形状**：`SessionController::compaction_gate()`（返回
  `Option<tokio::sync::MutexGuard<'_, ()>>`，句柄上唯一在签名里点名锁的方法）换成
  `with_compaction_gate(work)`——调用方把「这段工作期间不许开新 run」交给控制器，
  guard 永不越出控制器模块；**"门覆盖整段工作"从约定变成结构保证**（生产调用点
  `/compact` 原先靠 `let _compaction_gate = …` 的生命周期约定）。
- **清空策略落进内层**：`ControllerInner::clear_cached_history()` 承接缓存的清空，
  句柄只转发——句柄不再直接写 `*inner.cached_history.lock()`。
- **远端句柄无门照跑**（行为不变，远端句柄 `inner` 为 `None`，没有可串行化的工作），
  新测试 `session/controller/tests/compaction_gate.rs` 钉住这条语义。
- **行为变更零**：门仍只挡 run start（不挡 steer、不挡 cancel），三条既有测试逐字保持。
  `uva-app-server` 不在冻结面里，仓内无 crate 依赖它，无基线要更新。
- 决策记录见 docs/adr/0019-controller-handle-offers-a-promise-not-a-lock.md。

### Changed (折叠载荷的读法归一处：词汇是主人，消费者改问)

- **折叠载荷只有一套解码**：载荷的读法按 ADR-0009 归行词汇管，但那条纪律只落实了
  一半——词汇有宽松的 `decode_message`，history 后来又长了一套严格的，两套各认
  不同的形状；压缩载荷的 `data.summary` 也两个读者。现在词汇新增
  `decode_folded_payload`（provider 形状 → 压缩摘要 → 报错），history 改问它、自己
  那套删掉，`data.summary` 只剩词汇内部一个读者（`compaction_summary_message`）。
  **严格度不变**：历史投影对无法证明归属的载荷仍然报错（新增测试
  `a_payload_that_is_neither_shape_fails_the_projection` 钉住）。
- **provider 内容块形状一个读者**：读 `type: tool_use`/`id` 与
  `type: tool_result`/`tool_use_id` 的两处（`surface/fold.rs`、`tool_pairing.rs`）
  收进新增的 `uva-session/src/content_blocks.rs`，键名只写一遍；手读保持手读（旧
  方言载荷解不成 provider 类型，折叠必须仍能在旧载荷里找到调用）。
- **行为变更零、公开面零增长**（新增项都是 crate 内 `pub(crate)`）。决策记录见
  docs/adr/0018-folded-payload-reads-live-in-one-place.md。

### Changed (聊天窗口读模型归一个模块：界面只做胶水)

- **窗口读模型有模块了**：前端两套读行词汇里，转录（`session-to-messages.ts`）是
  纯模块且有 vitest 覆盖，而窗口那套（12 个 action 的 reducer、窗口/轮/steer 语义、
  状态与 action 类型）是 `ChatPage.tsx` 的私有函数——vitest 进不去（ADR-0001 只测纯
  函数），于是窗口 reducer 的每个分支只能人肉点页面验证。现在整块搬进
  `lib/chat-window.ts`（reducer + `emptyWindow` + 页面真用到的三个符号），模块接口
  是窄的，其余封在模块里；`ChatPage.tsx` 2001 → ~1430 行，只剩 React 渲染与
  effects。
- **行为变更零**：逐行未改（仅 `turnEndReason` 的 doc 从错放的 `rowData` 头上归位）。
  测试面从零到 13 条：新 `tests/chat-window.test.ts` 对 12 个 action 每个至少一条
  用例，外加 baseline 重建、live 帧守卫（退役后不得复活 / 序号单调）、steer 生命周期
  与轮边界分支。决策记录见
  docs/adr/0017-chat-window-read-model-in-one-module.md。

### Changed (退役规则归一个模块：每个客户端各答一次)

- **取代判据只有一处**：ADR-0012 定下的退役规则（内容行序号 > `start_seq` + 运行
  已结束时的轮边界）在客户端被重新推导了三次且分叉不同——网页端 `live-projection.ts`
  只有内容行那一半，reducer 在模块外又拼轮边界与复活守卫，baseline（重连）路径又
  只看内容行（与增量路径自相矛盾）；TUI `app.rs` 缺轮**开始**那一半，迟到的旧投影
  帧能在 TUI 里复活。现在网页端由 `live-projection.ts` 拥有整条规则
  （`retiresProjection` / `liveFrameAdmitted`），TUI 由新增的 `supersession.rs`
  拥有同一职责，两个读模型只问不推。
- **行为变更两条**（向 ADR-0012 的规则对齐）：网页端重连时，若基线里只有轮结束行
  （失败轮、无内容行），投影被退役——页面显示持久转录而非陈旧的流式回答；TUI 的
  轮开始行也退役投影并打标，迟到的旧投影帧不再复活（`suspended` 的轮结束行仍不
  退役，该例外不变）。测试：网页端 `live-projection.test.ts` 覆盖整条规则，TUI
  `supersession/tests.rs` 逐条件覆盖。决策记录见
  docs/adr/0016-supersession-rule-in-one-module-per-host.md。

### Changed (行构造归一个模块：uva-session 之外无人拼 SessionEvent 变体)

- **builder 集补全，14 处字面量归一**：`typed_event_builders` 新增
  `warning` / `tool_result` / `hook_block` / `tool_result_transformed` /
  `parked_prompt` / `park_consumed` / `feedback` / `session_shutdown` 与两个
  语义入口 `message_row`（带角色的 provider 消息 → 行，种子路径的 role match
  整体搬进 uva-session）、`turn_prompt_row`（本轮 prompt 行：attachments +
  fold 算好的 `surface_op`）。app-server 9 处与 harness 5 处的
  `SessionEvent::X { ... }` 字面量全部改走 builder；`SessionShutdown` 不再
  带调用侧的时钟读（`LogWriter::stamp` 在 append 时总是重盖，双钟读病灶消除）。
  读侧 pattern（fold / render / scan）是读模型，不动。
- **不变量进门禁**：`check-row-vocabulary` 的 Rust 扫描器加构造判别——
  `uva-session` 之外拼构造形状的 `SessionEvent` 变体即红（pattern 标记
  `..` / `=>` / `matches!` 与 `let `/`Some(` 前缀豁免读侧），裸 match 臂进
  审计表（`uva-approval` 三处，同一 ratchet）；`--self-test` 补构造夹具。
  决策记录见 docs/adr/0015-row-construction-in-one-module.md。

### Changed (挂载管线归一个模块：装配变成运行时的路径只有一份)

- **build 只有一份配方**：boot 的挂载与 HMR 重挂（`remount_row` / `apply` / rollback）
  此前各拼一遍 factories → `BootContext` → `build_plugins` → `mount_all_with`，且
  `BootContext` 配方已行为性分叉——重挂丢阻塞池、显式 `sessions_dir` 与压缩模型
  （`context-compactor` 会以空 model 重建）。现在收成 `composition/kernel` 的
  `build_rows` + `mount_built` 一个 stanza：一个配方，「先 build、再 unload、后
  mount」的次序约束留给调用方；`Assembly` 存 boot 的 `BootExtras`，重挂行与 boot
  同配方（回归测试 `a_remount_keeps_the_boots_session_corpus` 钉住）。
- **审计进挂载点**：停泊审计（`audit_pending`）从 boot-only 移进 `mount_built`，审计
  本次挂载的行；`REQUIRED_ROWS`（`agent-loop` / `model-adapter`）停泊在任何挂载点
  fatal。行为变更：把必需行挂到停泊的 settings 编辑现在整笔失败并走既有
  best-effort rollback（旧行为：静默停泊 + dep_lost 写回）。纯移除不触发挂载、
  不审计——「移除成功、消费侧大声失败」的既有语义原样。

### Changed (控制器状态转移表归一个模块：怎么走只有一份)

- **写路径有了 owner**：`SessionState`（控制器状态）的 6 个写点散在 4 个文件
  （`mark_awaiting_approval`、`mark_running`、resume parked、cancel、settle、
  `adopt_state`），「哪些转移合法」没有被任何一处拥有；快照总线的 3 个发布点与
  `inner.rs` 的文档承诺（Running / Idle / Cancelled）不一致——`Idle` 从未发布。
  现在收成 `session/controller/state.rs` 一个模块：封闭转移枚举 + 穷尽 `apply`
  （全函数，停留也是定义格）、快照发布、`is_running` / `is_startable` 查询。
- **settle 发两帧**：`Completing`（最后一轮的结局，流上仍可读）之后紧跟 `Idle`
  （会话的当下）。行为变更两条：`delete_session` 在一次运行完成后不再永远
  `session_busy`（`latest` 此前永远停在 `Completing`，而 `turn_in_flight` 把它算
  in-flight）；`sessions/status` 的 `latest` 在 settle 后报 `idle` 而非
  `completing`。`turn_in_flight` 与 `OperationState` 枚举零改动，契约零再生成。
- **`suspended` 收成模块私有记录**：Suspended 事件记账、settle 消费、BeginRun 清零；
  延迟翻转语义原样（立即翻转会让 `is_startable` 在旧任务排干时放行新 run）。
- **`adopt_state` 走表**：接管重建是转移表的 `Adopt` 一格（拒绝覆盖 `Running` 原样），
  不再是绕过表的裸 setter。
- **读路径与纯折叠不动**：`status.rs` 一字不动（ADR-0008 的裁定保持）；路由读
  `state()` 喂 `StatusFacts` 的既有读法不变。
- 决策记录见 [`docs/adr/0013-controller-state-transition-table-in-one-module.md`]。

### Changed (委派发现归一个模块：枚举窗口、头部解析与镜像方言只有一份)

- **一个 `discover(index, scope)`**：「枚举最近会话、认头部的 `subagent_enter`、报出
  委派」此前在两个 crate 写了三遍（`uva-tool-task` 的 `scan.rs`、app-server 的
  `routes/tasks.rs` 与 `routes/sessions` 的 `subagent_attribution`、`routes/follow` 的
  `agent_attribution`），且已分歧——头部垫元数据行的子会话名册找得到、tasks 页看不见；
  镜像方言只有 tasks 页认。现在收成 `uva-tool-task::control::scan` 一个模块、
  两个作用域（`Scope::Parent` 名册用、`Scope::User` tasks 页用）、一个
  `SCAN_LIMIT`。
- **头部规则统一**：头 4 行窗口内第一个 `subagent_enter`——自名的是自有日志记录，
  名别人的孩子是旧构建写进父日志开头的镜像（降级记录：无派发词、无结局），
  同一孩子自有日志压过镜像。归因（列表 / 详情 / follow）从「只认第一条事件」
  放宽到同一窗口规则。
- **名册只收养自有日志的孩子**：镜像孩子无日志可开，冷唤醒要的 writer 不存在——
  收养在发现处过滤，不再靠遗漏。
- **`SessionIndex` 增 `session_ids_of_user`**：按用户枚举是宿主知识，归
  `ServerSessionIndex`（单用户目录 + 注册表）；路由侧的 `recent_session_ids` 删除。
- **窗口预算成真**：`SessionSink` 增默认方法 `read_head_rows(window)`，
  `JsonlSessionSink` 按字节预算实现；`ServerSessionIndex::read_head` 此前是
  **全文读再 `take(4)`**，名册每个会话都在付全文读——现在委托有界读。
  冻结面记名字不记方法（ADR-0009 同一裁定），基线不动。
- **发现 / 描述分界**：发现答「谁存在」（窗口证据），描述答「它说了什么」（全文：
  派发词、`subagent_exit`、状态）留在 tasks 路由；`get_task` 不动。
- 决策记录见 [`docs/adr/0011-delegation-discovery-in-one-module.md`]。

### Changed (活动投影按步铸造，身份是它的持久位置)

- **活动投影的身份换成持久位置**：投影带 `start_seq` = 该步开始时日志已落盘的序号，
  客户端的取代判据只有一条 —— **一条内容行（助手消息 / 工具结果）的序号大于
  `start_seq`，即取代该投影**（另有一条边界规则保留：轮边界行在运行已结束时取代
  投影）。行与投影之间的关联因此是日志序号本身：不需要服务端打标、不需要 id
  匹配、不需要比较文本。
- **投影按步铸造**（一次模型调用一步，步边界处文本 / 推理 / 工具调用清空）：
  投影与 durable 行同粒度，一条助手行就是它那一步的投影。此前投影按运行铸造、
  文本跨步累加，而助手行是每次模型调用一条 —— 身份比投影的寿命粗，第一条助手行
  就足以退休整轮投影。
- **两个缺陷随之修掉**：多步（用工具的）运行里，网页端此前从第二步起不再流式
  （同一轮的 live 帧被“已取代”守卫丢弃），TUI 则重复显示第一步的文本（它把帧里的
  `turn_id` 丢掉，只能比较文本形状）。现在每一步都流式，两端都不再重复。
- **服务端不再给行打标**：`row` 帧上的 `turn_id`、`turn_labels` 与
  `carries_live_content` 整套删除；结算后落地的行与 baseline 里的行都按同一条
  序号规则处理，不再需要特判。
- **线上字段**：`live` 帧的 `turn_id`（字符串）→ `start_seq`（数值），契约随之
  重新生成；网页端手写的帧解析器同步。TUI 补齐网页端已有的两个直播守卫
  （已被取代的投影、`live_seq` 单调）。
- 决策记录见 [`docs/adr/0012-live-projection-is-identified-by-its-durable-position.md`]。

### Changed (载荷的读法归行词汇：消费者不再自己拼载荷的键)

- **词汇现在也管「这一行带了什么」的读法**：`GLOSSARY.md` 的「行词汇」条目早就写着「载荷带
  哪些字段」由它说了算，代码没兑现——TUI 有 14 处手写载荷读取、浏览器近 20 处、
  `uva-session` 内部 2 处，每个消费者各自重新实现一遍 provider 消息的形状。
- **一个真缺陷修掉**：崩溃修复（`repair`）把助手的 `data["content"]` 当**裸数组**读，而
  harness 写的是 `{"Single":{…}}`（`Content::parts` 决定形状，单个 part 是 `Single`）。
  于是「模型发了 `tool_use`、崩溃发生在 dispatch 行之前」这一种崩溃**拿不到那条合成
  closer**，下一次请求带一个没人答的 `tool_use`。夹具写成了 harness 不写的那种形状，
  所以测试是绿的、生产是坏的——夹具与生产形状对齐之后，测试第一次覆盖生产形状。
- **`SessionEvent::message()`**（新增，与它已有的十五个派生访问器同族）：这一行投影成的
  provider 消息。它与 `CurrentRow::message()` 共用同一套解码（provider 的完整编码、
  手写的 `{"role":…,"content":[…]}` 形状、带文本或调用 id 的兜底）——一份解码实现。
  `interrupted_turn_closers` 的签名不动；冻结面记的是 crate 根的 `pub use` 名字，
  新方法不是新名字，基线与 `docs/api-frozen.md` 都不用动。
- **消费者改问词汇**：TUI 的行折叠从「还原成 JSON 再手读」改成「还原成类型化的行与消息」，
  手写载荷读取 14 → 4（剩下四处是**自由格式**的袋：警告行、hook 行、崩溃修复 closer
  自己的标记、边界行的 `reason`——它们没有类型化字段可问）。`search/text.rs` 的手写
  `Single`/`Multi` 信封解析换成类型化解码。`uva-tui` 因此多一条依赖边
  `uva-provider`（不带 HTTP，不新增编译单元）。
- **写入者只写一个键**：三个挂起行（`turn_suspended` / `step_end_suspended` /
  `turn_end_suspended`）的 `data.phase` 改成 `data.kind`。`kind`/`phase` 这组封闭词汇此前
  有**三种优先级**（Rust：类型化字段 → `data.kind` → `data.phase`；TUI：`data.kind` →
  `data.phase`；Web：`data.phase` → `data.kind`）——今天值不冲突所以不会分歧，但写入者
  只写一个键，漂移的可能就没了。旧日志的 `phase` 由还原的宽松回退继续读。
- **provider 的消息类型进线上契约**：`uva-provider` 加 `schemars`，`Message` / `Content` /
  `ContentPart` / `Role` / `ToolUse` / `ToolResult` 与叶子类型派生 `JsonSchema`——契约从此
  发布 provider 自己的类型，浏览器不再手写信封的读法。**线上字节不变**，变的是生成的
  TS 类型。
- **`SessionTurn` 收成按标签的判别联合**：33 个分支——每个标签一个，**从 `RowTag` 自己的
  标签联合里读出来**，没有第二份清单要维持（契约测试把联合钉在词汇上：分支 = 词汇的
  标签）。`user_message` 与 `assistant_message` 的 `data` 是 provider 的 `Message`（联合按
  标签窄化，浏览器不再 cast）；`tool_result` **不窄化**——运行自己的行装工具消息、崩溃
  修复 closer 装自己的标记袋，一个标签两种载荷，说了哪种都是撒谎。
- **浏览器迁移**：`messageParts` 收 `Message` 出 `ContentPart[]`（不再猜 `Single`/`Multi`
  信封）；`applyContentPart` 按生成的联合分派；`tool_result` 的一次 cast 收在一处并注明
  为什么（那一行有两种形状）；`ChatPage` 的三份 `kind`/`phase` 回退缩成投影模块里的
  一份。

### Changed (会话写入者的类型化事实不再经 JSON 键绕一圈)

- **bound 搬到序列化之前**：`LogWriter` 收到类型化的 `SessionEvent` 后，此前**先序列化
  成 `Value` 再按 JSON 键把类型化事实读回来**——`Plugin` 变体上就有
  `kind`/`payload`/`original_bytes`，`bound_plugin_row` 却在序列化行上
  `remove("payload")`/`remove("kind")`；`ToolOutput` 变体上就有 `call_id`，
  `tool_output_call` 却读 `.get("call_id")`。现在 `append_event` /
  `append_event_nowait` 在序列化前 bound 类型化事件（`bounded_event`：只有两个带
  上限的行付一次拷贝，普通行原引用直通），Value 级的两个 bound 函数删除。
- **合并路由问类型化字段**：流式分片「进队列还是并进上一行」的判定从按键读
  `call_id` 改为问 `SessionEvent::tool_output_call()`（只匹配 `ToolOutput`；`tool_call`
  自己的 id 是另一个事实，从不路由进合并）。`enqueue` / `append_nowait` 里的同一判定
  删除——它们的唯一调用方就是两个类型化门，路由已经做完。
- **`append_copied` 不再重复 bound**：抄来的行来自本 crate 写的子日志，写入那一刻已
  bound（幂等），删掉逐行重跑；重映射的 `seq` 改问 `CurrentRow::seq()`。
- **不动**：`stamp()` 的 `seq`/`ts`/`ignorable`（信封是写入者自己的知识，`ignorable`
  本来就问词汇）、`merge_tool_output_row`（`data.output` 是袋里的自由格式，没有类型化
  字段可问）、批次缓冲的形状。线上字节不变；冻结面不变（删掉的都是 `pub(crate)`）。

### Changed (会话状态归一个模块所有：控制器状态与对外状态是两个枚举)

- **新模块 `crates/uva-app-server/src/session/status.rs`**：「这个会话现在是什么状态」从此
  只有一份答案。模块是纯函数，收调用方已经折好的事实（控制器的内存状态、日志里有无
  未决审批、日志空不空），窗口规则与错误策略留在调用方——各调用点能证明的窗口不同，
  而「状态形状的调用方没有错误通道」是仓里已有的规矩。
- **两个枚举，不是一个**：`SessionState`（控制器状态：空闲 / 运行中 / 等待审批 / 已取消，
  从 `session/controller/ops.rs` 搬进模块）与 `SessionStatus`（会话状态：从未开始 /
  进行中 / 等待审批 / 已取消 / 已完成，对外报告）。合成一个被否：控制器写「空闲」时
  不知道该写 `unspecified` 还是 `completed`——它不关心日志空不空。
- **降级规则以日志为准**：内存记着「等待审批」而日志里没有未决审批时，报**空闲**。
  这条规则此前只有 `follow` 的 baseline 用（`reported_state`），`sessions/list` 与
  `sessions/get` 的两处推导（`derived_live_state` 与磁盘扫描路径）都不降级——于是同一个
  会话在两个响应里可以答得不一样。现在四处统一到同一条规则。
- **行为变更两处**：①审批在别的节点被答掉、内存标记陈旧时，`sessions/list` 与
  `sessions/get` 由 `input_required` 改为 `completed`；②运行中的会话在
  `approval_asked` 行落盘与挂起标记翻转之间的那个窄窗口里，`sessions/list` 由
  `input_required` 改为 `working`——`follow` 的头今天就报 `running`，这一刀让两个端点
  在同一时刻说同一句话。
- **9 个标签是筛选词汇，不是状态词汇**：状态产出者只覆盖 5 个；`submitted` /
  `failed` / `rejected` / `auth_required` 今天没有产出者，但作为筛选词保留——线上
  breaking，且砍掉的是别人在用的筛选词。筛选词汇与状态词汇分开声明。
- **`OperationState` 不动**：它答的是「这一次 agent 循环跑到哪了」，与控制器状态是两套
  不相交的词汇（中间没有转换函数），且在冻结面里；浏览器不消费它。
- **旧实现删除**：`infer_status_from_parts`、`derived_live_state`、`reported_state`、
  `state_label`、`parse_status_filter` 与 `SessionState` 在 `controller/ops.rs` 的定义
  一并删掉。它们的单测搬进 `src/session/status/tests.rs`，按新接口重写，并补上一张
  穷尽表——其中「内存标记陈旧」那一格此前**一个测试都没有**。
- **文档改正**：`api/v1/page_query.rs` 的「规范集合」说的是 5 个连字符标签，代码认的是
  9 个下划线标签；`SessionSummary.status` 的注释说「9 个标签的封闭集合」，字段值集其实
  是 5 个。两处都改成代码真正在做的事。
- **线上收紧成封闭 union**：`SessionSummary.status` 与 `SessionDetail.status` 从裸 `string`
  变成 `SessionStatus` 的五值联合（`unspecified` / `working` / `input_required` /
  `canceled` / `completed`），**拼写逐字不变**。变体名即线上标签——照行词汇的规矩
  （ADR-0006），`rename_all = "snake_case"` 派生一切，`as_wire()` 是它手写的镜像，
  两者由一个测试钉住。`FollowHeader.state` 仍是 `string`——那套 4 标签词汇下一刀退役。
- **前端的死类名清掉**：`ChatPage.css` 给 `.status-submitted`、`.status-unspecified`、
  `.status-rejected`、`.status-auth-required`、`.status-unknown` 定了样式，而消息态
  只产出 `working` / `completed` / `failed` / `canceled` / `input-required` 五个值——
  那五个类名是照着会话状态的词汇写的，却挂在消息状态上。
- **线上只剩一套会话状态词汇**：`follow` 的头原来自带一套 4 标签词汇（`idle` /
  `running` / `awaiting_approval` / `cancelled`），与列表的 5 标签词汇并立——同一个会话
  在两个响应里用两套说法，取消一个作 `cancelled` 一个作 `canceled`。这一刀把
  `follow` 头的 `state` 换成**会话状态**本身（`unspecified` / `working` /
  `input_required` / `canceled` / `completed`），4 标签词汇退场，控制器状态不再上线。
  **这是线上 breaking 变更**：读 `header.state` 的客户端要跟着改，而且字段的**含义**
  也变了——本节点没有活着的运行，过去报 `idle`，现在报 `completed`（日志有行）或
  `unspecified`（日志空）。仓内的读者只有一个：TUI 的 `turn_from_state`（`working` →
  运行中，`input_required` → 等待审批，其余 → 空闲）；浏览器不读这个字段。

### Changed (服务端发布当前行：浏览器不再见到方言)

- **线上只发布当前行**：`sessions/get` 的 `history`、`follow` 的 baseline 与逐帧行
  都经行词汇的新原语 `publish_row` / `publish_rows` 发布——当前行原样（零拷贝移动），
  旧方言的行改写成它的当前对应物（标签、载荷拼写、附件引用抬到行顶层、失败标志与
  调用 id 补进顶层），没有当前形式的行（`Agent`、未知标签、无类型）**不发布**；
  `sessions/events` 的 `legacy` / `unknown` 桶继续报告它们。发布的行是**原始行，
  只在词汇说它不对的地方改**：序号与时间戳连同它们的缺席原样保留，绝不重序列化
  事件（`SessionEvent` 的必填 `seq` 表达不了「这一行没有序号」，捏造 0 会被评分
  目标查询拒绝）。
- **浏览器方言表退场**：`SessionTurn.type` 收窄为 33 个当前标签，生成的
  `SessionRowDialect` / `SessionRowTag` 联合、`DIALECT_ROW_TREATMENT` 表、四个
  `applyLegacy*` 处理器、`isDialectTag` 与 `ChatPage` 的两处 `UserInput` 比较全部
  删除；还原行走当前行同一条分派路径。
- **归一补上三个会丢内容的缺口**：`Model`+推理行此前整行被丢（浏览器今天画得出
  thinking 段）；只带附件无文本的 `UserInput` 此前整行被丢（文件气泡）；`Message`
  方言的宽松 `{"role":…,"content":[…]}` 拼写此前解不出（当前行同款宽松回退现在
  也归方言用）。规则：归一必须接受浏览器今天渲染的每一种载荷形状，且不得接受
  浏览器丢弃的形状。
- **还原事件带上行自己的事实**：`dialect_event` 此前硬编码 `seq: 0` / `ts: ""` /
  空附件；现在取原始行的序号、时间戳、`source`、附件引用与工具结果的失败标志。
  这修复了 `sessions/events` `typed` 桶的行身份，也让引用方言行真序号的 `compaction`
  替换校验通过——上下文计价器对这类旧日志从多算改为算对。
- **带序号的旧方言助手轮从此有评分目标**：此前浏览器不给它们渲染评分控件；
  现在 `feedback_target` 能把还原后的 `Model` 行解析成 `AssistantMessage`，点击有效。
- **门禁扩到浏览器**：`check-row-vocabulary` 新增 Web 半边——`uva-web/scripts/
  check-row-vocabulary.mjs`（TypeScript compiler API）扫描 `uva-web/src` 的四个方言
  拼写，走同一张审计例外表与同一只棘轮；33 个当前标签在 Web 仍合法（`tsc` 已用
  生成的 union 强制那张 `Record` 记账）。
- **冻结面**：`uva-session` 135 → 137（增 `publish_row`、`publish_rows`）；
  `RowDialect` 留在公开面（服务端仍要能指名旧行才能归一它），只是不再上线路。

### Changed (行词汇在读边界归一一次：读者不再见到方言)

- **归一在读边界**：会话日志的任意一行在那里被还原成**当前行**——它在当前
  词汇里的标签、它投影成的模型消息、它在表面里的位置。旧方言于是不再是运行期
  类别，而成了归一的**输入**：读者只见当前行，不见方言。
- **方言按载荷判定**：`Message` 与 `Model` 的对应物取自载荷里的角色或 part
  种类，不由方言本身决定——这是 omp 那个「标签说不是用户消息、载荷说是用户
  消息」的真缺陷所在。类型化行以**标签**为准：载荷的角色不推翻标签，于是两个
  折叠路径对同一行给出同一个答案。
- **未知类型 fail-closed**：一行的类型当前构建不认识，且那一行没有「可忽略」
  标记 ⇒ **这份会话日志读不了**，而不是「跳过这一行」。静默跳过一条必需事件
  会重建出一个错误的会话，所以宁可拒读整个日志。「可忽略」是逐标签声明的，由
  会话写入者盖在行的顶层（`type` 的兄弟）；今天 33 个标签全部不可忽略。
- **读点不再有两分支 match**：`routes/follow.rs` 的活内容判断此前漏掉了方言
  分支（旧方言日志里的助手行拿不到回合标签）；现在「这一行是什么」只有一处
  答案，漏分支不再是可能。
- **表面成员资格进词汇**：`RowTag::surface_role()` 公开；「进不进表面」由行词汇
  声明，「进不进转录」仍由每个读模型自己决定。
- **两个行为变化随同一刀落地**：旧方言的行现在算「活内容」，TUI 与工件集的
  转录会开始渲染它们今天跳过的旧方言行；`sessions/list-events` 的四分桶里，
  能还原的旧方言行从 `legacy` 移到 `typed`。
- **冻结面变动**：删 `RowClass` / `RowReading` / `carries` / `classify_row`；增
  `CurrentRow` / `RowDefect` / `UnreadableLog` / `restore_row` / `SurfaceRole`。
  `uva-session` 134 → 135。
- **决策记录**：[ADR-0007](docs/adr/0007-row-vocabulary-restores-at-the-read-boundary.md)；
  ADR-0006 的后果段就地修正（它的「不再在每个读点各写一遍双标签匹配」当时没兑现）。

### Changed (profile 文档 = 插件树的唯一真源：dsh entry 模型移植)

- **行形状**：profile 文档的一行现在是一份 **manifest**（dsh
  `EntryOptions` 的直译）：`{ id, name, config, capabilities, group,
  children, disabled, inject, intercept, isolate }`。`id` 是行身份、`name`
  是它装的插件（工厂目录的键，取代了旧的 `runtime` 字段——`[core]` /
  `[ui]` 段已决定装载器）；`disabled` 是布尔位，取代旧的**三态** `enabled`
  （`true` / `false` / 判定表 `{ service, check, arg }`）与其判定器、
  开机两波挂载机制（判定服务由插件提供的「鸡蛋问题」随之消失）；组行
  （`group = true`）的子行写在 `config` 数组里，`disabled` 沿祖先链禁用
  整棵子树。
- **依赖由文档声明**：每行的 `inject` 是它**额外**声明的服务键；有效边 =
  插件 manifest 自己的 `inject` ∪ 行的 `inject`（同名键以行为准，文档只能
  加、不能挖掉插件的固有最小依赖）。内核的静态「缺依赖即整组拒绝」删除：
  缺依赖的行挂起（PENDING），开机**审计**逐行列出挂起的行与它缺的服务名，
  只有 `agent-loop` 与 `model-adapter` 挂起才致命。
- **组是运行时插件**：`group` 行像 dsh 的 `Group` 那样挂载成一个 fiber，
  子行挂在它下面（内核新增子挂载原语与按行派生上下文：`isolate` 标签作用域
  与 `intercept` 配置叠加沿子树组合）。
- **运行时写回**：一行因失去 provider 而不再能运行时，文档把它写回
  `disabled = true`（dsh 的自禁用写回）。
- **wire 形状**：`profile/list` / `profile/get` 的行带上 `name` /
  `group` / `disabled` / 有效 `inject` / `intercept` / `isolate`，不再有
  `runtime` 与三态 `enabled`；`profile/set` 的参数从 `enabled` 改为
  `disabled`（布尔）；自管 MCP 的 `profile` 工具同形。
- **`model-adapter` 行配置摊平**：provider 目录（`providers` /
  `default_provider` / `default_model`）成为行配置的**顶层键**，不再嵌在
  `workspace` 下（`WorkspaceConfig` 本身不变）。
- **新门禁** `check-profile-document`：入库的 `.agents/profile.toml` 必须
  复述每个插件 manifest 的依赖边（有效边与文档声明不一致即红）。
- **迁移**：旧形状文档在解析时点名报错（未知字段 `runtime` / `enabled`），
  不做双形状兼容；`uva-app-server translate --config config.yaml --out
  .agents/profile.toml` 重新生成完整文档（内置装配 + 迁移后的两行，每行
  列全有效边）。

### Added (子代理生命周期：停放/唤醒、工件集、名册真源)

- **停放（parking）**：`uva-tool-task` 的句柄表开始自己管内存生命周期。
  空闲满 `PARK_TTL`（5 分钟）的子代理 `idle → parked`：转向通道、取消
  令牌、会话写入者与 peer **四个句柄全部释放**，行留着（`parked` 的定义
  就是「本进程不再持有」）。运行中、已停放、已终止的子代理都不老化；
  表的所有者（主会话）永不停放。`ControlRegistry::tick` 用注入的 `Clock`
  量 TTL（测试用可推进的时钟），返回本次停放的 `(父会话, 子 id)`。
- **唤醒接管**：`parked` 子代理的 `message` 走冷唤醒（读子日志头 → 解析
  peer → 子写入者再跑一轮，进入 `running`）；唤醒把行**接管**成新一轮的
  句柄，旧轮已释放的通道不再复用。`kill` 也在进入 `aborted` 时释放句柄。
- **自走时钟**：`ControlSurface::tick` 是状态机的推进入口；
  `ControlSurface::set_park_loop(spawner, sleeper)` 装宿主的运行时 seam，
  首次 tick 起一条**只持弱引用**的循环（`PARK_TICK` = 60 秒），brick 卸载
  或 surface 析构即结束。`tool-task` 行在 `uva-app-server` 里装上 tokio 的
  `Spawner`/`Sleeper`；没装 seam 的宿主仍会在每次控制面调用前老化一次。
- **名册真源**：`agents_list` 改为从句柄表读取（工具形状不变：仍是
  `{children:[{id,name,status,activity}]}`）。读之前先与会话对账——
  本进程不持有的子代理（上一个进程派发的）按 `parked` **收养**进行表，
  所以重启后名册不空、给它发消息直接走冷唤醒；活行永远压过日志副本。
- **工件集**：每轮结束（正常结束、取消、唤醒后的轮都算）在**子会话目录**
  写 `artifacts/<id>.{md,jsonl,patch}`：`.jsonl` 是本轮结束时子日志的冻结
  副本、`.md` 是同一转录的可读渲染、`.patch` 只在隔离轮（worktree）确实
  改了东西时出现（`worktree::worktree_patch` 读 `git diff <base sha>`，在
  worktree 清理前取）。约定写进 `docs/log-format.md`；写入是 best-effort，
  没有持久会话的宿主不会得到工件集。
- **seam 变化**：`SessionIndex` 增加带默认实现的 `session_dir(id)`——宿主
  若不把会话放文件系统，返回 `None` 即不写工件；`uva-app-server` 的
  `ServerSessionIndex` / `SessionIndexSlot` 按 `<sessions root>/<user>/<id>`
  实现。
- **测试**：四态迁移全表（本已存在）之外，新增老化→停放（TTL 边界、
  只有 idle 老化、主会话不停放）、唤醒→running 与接管、aborted 终态、
  收养与活行优先、停放循环自走、工件集落盘/`patch` 条件/无会话目录、以及
  `agents_list` 收养重启前的子代理与给 `parked` 子代理发消息的用例。

### Changed (行词汇预重构：变体名即线上标签)

- **变体改名**：`SessionEvent::LifecycleShutdown` 改名 `SessionShutdown`——
  线上标签仍是 `session_shutdown`，序列化字节逐字不变；Rust 侧这一变体的
  名字变了，是破坏性改名（冻结基线的名单是 crate 根名字，`SessionEvent`
  未动，故基线不变）。
- **serde rename 清零**：33 个显式 `#[serde(rename)]` 全部删除，标签由枚举级
  `rename_all = "snake_case"` 从变体名派生——「变体名即线上标签」此后对标签与
  方言两个枚举都成立。

### Changed (工件集渲染：一张穷尽 match 替九个手写标签)

- **穷尽匹配**：`uva-tool-task` 的工件集渲染器不再拿九个手写标签字符串
  分派，改成对 `uva_session::RowTag` 的一张穷尽 `match`——渲不渲染这一行
  仍是工件集这个读模型自己的决定，但「整个词汇都被表态过」改由编译器证明：
  词汇新增一个标签，这个 crate 先编译不过（`error[E0004]: non-exhaustive
  patterns`）。
- **取值走类型化事件**：九个标签的值不再从原始行手抄——「顶层优先、
  `data/` 兜底」的 `string()` 助手删掉，改从 `SessionEvent::from_value`
  得到的类型化事件读：顶层字段缺失时由 `uva-session` 的 `normalize` 从
  `data` 补齐，读取点不再各写一遍兜底。
- **覆盖断言**：新用例遍历词汇里的每个标签（从词汇自己发布的 schema 读，
  不抄一份手写表），断言工件集对它有表态——渲染，或显式省略；少一个即红。
  另有新用例断言词汇外的行（旧方言、版本偏斜的未知标签、没有标签的行）
  仍旧被丢弃，语义不变。
- **渲染输出**：既有渲染断言（工件集落盘那条与 `tests/control_surface.rs`
  集成套件）全绿。一处差别：turn 行的边界名改读类型化的 `kind` 字段——
  载荷把边界写作 `phase` 的停摆 turn（`turn_suspended` /
  `turn_end_suspended`）此前渲染成空的「> turn 」，现在渲染出它的边界名。

### Added (行词汇：会话行是什么，一处声明)

- **词汇模块**（`uva-session` 纯扩展）：`RowTag` 封闭标签枚举（33 个
  变体，变体名即线上标签，`rename_all` 派生一切，`JsonSchema` 发布
  `oneOf` of consts——线上契约标签 union 的唯一来源）；`RowDialect`
  封闭方言枚举（`UserInput`/`Model`/`Message`/`Agent`，只读、永不再写）。
- **一次问答**：`classify_row` 对任意输入全同、零分配——当前词汇 / 旧
  方言 / 未知标签（携带原字符串）/ 无标签四分；`Rows` 借用读视图先比
  标签再解码（`of`/`first_of`/`last_of`/`typed`），被排除的行不付解码
  代价；`readings()` 一次读完整本日志（位置 + 分类 + 解码结果，每行最多
  解码一次），`RowReading::unreadable()` 是「这一构建读不动的行」——
  已知标签载荷解不动 ∪ 无标签——而 `broken()` 仍是更窄的缺陷查询（只报
  已知标签），与 `foreign()`（方言与未知）分账。app-server 的会话详情
  分桶与 resume 的 provider 快照读点都走这条读视图；冻结面新增
  `RowReading`（133 → 134）。
- **一处声明**：`surface_role` 与行种挪到 `RowTag` 上声明（crate 内
   私有），`SessionEvent::surface_role()` 与折叠的 `row_view` 读同一份；
  `from_value` 的 33 字面量改读词汇；`SessionEvent::tag()` 与
  `RowTag::empty_event()` 两个方向的穷尽 match 由平局测试钉住（换一处
  配对即红）；新旧对应 `carries`（用户提示 ↔ `UserInput`、模型输出 ↔
  `Model`）由模块拥有。
- **读点迁移**（`uva-app-server`）：七处生产裸标签读点（follow 路由三处、
  tasks 路由一处、`run_task` 两处、会话状态一处）改经词汇——标签比对走
  `classify_row`，新旧方言对应走 `carries`（`user_message` ↔ `UserInput`
  的双标签规则不再在每个读点各写一遍）。该 crate 生产代码零裸标签字符串。
- **会话详情四分**（`GET /api/v1/sessions/{id}/events`）：响应不再把
  「读不动的行」全叫 `legacy`——`typed`（当前词汇且载荷可解）/ `legacy`
  （旧方言）/ `unknown`（本构建不认识的标签，即版本偏斜）/ `broken`
  （已知标签而载荷解不动，或无标签行）各一桶，各带计数；版本偏斜不再与
  历史方言共用一个名字。
- **新构造器**（`uva-session`）：`assistant_message(data)`——助手轮的
  类型化构造器，与 `user_message` 同形；子会话开启者的写入从此走类型化
  构造器，落盘行形状不变。冻结面 134→135。
- **裸写入口关闭**（`uva-session`）：`LogWriter::append` /
  `append_nowait` 从固有公开方法收成 `pub(crate)`——行形状（标签与载荷）
  从此不再是对仓外调用方的知识：仓外的唯一**单行**写入口是按事件命名的
  `append_event` / `append_event_nowait`，保留历史原样的**批量**复制仍走
  `append_copied`（它把一批既有行写进本日志：保留各行自己的 `ts`，只给它们
  分配本会话的序号；拼一个新标签与载荷的自由没有回来）。所有既有调用点随之迁移
  （app-server 的四处测试、harness 的生命周期与压缩测试、`durable_hitl`
  示例）；确实要写旧方言信封的测试改用 `append_copied`，不为测试重新
  打开裸门。冻结面名字不变（`LogWriter` 本身仍在，收窄的只是方法）。

### Changed (契约换源：标签 union 来自词汇)

- **手写镜像枚举删除**：`crates/uva-app-server` 的契约目录不再自带一份
  33 变体的镜像枚举（`api/v1/session_view.rs` 的 `SessionEventType`），
  改为收录 `uva_session::RowTag`——变体名即线上标签的那份封闭枚举。
  线上标签的 union 从此由词汇派生，镜像与真源之间的漂移无处可生。
- **TS 导出名不变**：目录的既有改名机制（`SessionHeader` →
  `FollowHeader` 那种 Rust 名→TS 名）把 `RowTag` 以 `SessionEventType`
  发布，浏览器的 `Record<SessionEventType, …>` 逐标签记账表一行不用改。
- **对账测试删除**：契约目录与 `KNOWN_SESSION_EVENT_TYPES` 的「集合
  相等」对账测试没了——union 由词汇派生，人工对账的前提消失。
- **前端类型重新生成**：`uva-web/src/api/types.ts` 重新生成。union 的
  成员一个不多一个不少；变的只有文档注释的出处——union 的说明现在来自
  词汇模块自己的文档（真源），`SessionTurn.type` 的说明改指词汇的
  类型名。

### Changed (行词汇契约：方言 union 发布进生成的线上类型)

- **方言发布**：`uva-session::RowDialect` 进入线上契约目录，TS 名
  `SessionRowDialect`（四个字面量 `UserInput` / `Model` / `Message` /
  `Agent`），与标签 union 同源——两种 union 各自就是词汇自己的 schema。
- **行类型拓宽**：`SessionTurn.type` 从「标签 union 或空」改成
  `SessionRowTag | null`——一个由 `RowTag` 与 `RowDialect` 组成的并集，
  目录里的 schema 视图 `RowTagOrDialect` 经既有改名机制发布为
  `SessionRowTag`。线上类型终于能表示真实会话里出现的旧标签；服务端
  运行时行为不变（类型只是视图，路由照旧写 `serde_json::Value`）。
- **生成物**：`uva-web/src/api/types.ts` 由生成器原样重生成，新增
  `SessionRowDialect` 与 `SessionRowTag` 两个导出。

### Changed (TUI 的转录取舍：一张穷尽 match 替两张字符串表)

- **穷尽匹配**：`uva-tui` 的 `apply_row` 不再拿 `APPLIED_ROW_TYPES` 与
  `IGNORED_ROW_TYPES` 两张手写字符串表分派，改成对 `uva_session::RowTag`
  的一张穷尽 `match`——收不收这一行仍是终端这个读模型自己的决定，但
  「整个词汇都被表态过」改由编译器证明：词汇新增一个标签，这个 crate
  先编译不过（`error[E0004]: non-exhaustive patterns`）。
- **一次问答**：行先经 `classify_row` 问一次「这是什么行」；词汇外的行
  （旧方言、版本偏斜的未知标签、没有标签的行）仍旧早退，语义不变。
- **无 `unreachable!`**：那支「表里声明了却没有分支」的兜底分支删掉——
  穷尽匹配没有漏网的臂，编译器接管了它的职责。
- **会计测试**：全词汇覆盖那条（`every_known_row_kind_is_applied_or_
  ignored_by_design`）随穷尽匹配自然消失——它的两条断言（每个标签都被
  表态过、不会既应用又忽略）改由编译器保证；「每个应用标签真的过一遍
  匹配」那条保留，改用类型化标签驱动，并补一条词汇外的行确实被丢弃
  （版本偏斜与旧方言）的新用例。
- **依赖**：`uva-session` 从 dev-dependency 变成 `uva-tui` 的正常依赖——
  投影的生产代码按行词汇读行，不再自己认标签字符串。

### Changed (Web 的行方言识别：读生成的 union，`Message` 行不再被丢弃)

- **零手写方言**：`uva-web/src/lib/session-to-messages.ts` 的
  `EVENT_USER_INPUT` / `EVENT_MODEL` / `EVENT_AGENT` 三个常量与
  `legacyRowType()` 拓宽函数删除（`ChatPage` 的两处读点改为直读
  `row.type`，类型由生成的 `SessionRowTag` 收窄）。分派先判方言、再查标签表：
  `DIALECT_ROW_TREATMENT: Record<SessionRowDialect, Handler>` 的键就是
  生成的方言 union，`isDialectTag()` 拿表的键做运行时判断，负分支由
  TS 收窄成 `SessionEventType`（原 TS7053 消失）。词汇或方言增长而表不
  跟随时，`tsc` 直接红。`TYPED_ROW_TREATMENT` 仍是
  `Record<SessionEventType, Handler | null>`。
- **`Message` 方言表态：渲染**。此前浏览器静默丢弃 legacy `Message` 行，
  而服务端的折叠把它读进表面。现在按 `role` 投影，与服务端 `kind_of`
  对齐：`user` / `system` → 用户气泡，`assistant` → 助手轮（复用
  `assistant_message` 的 part 分派），`tool` → `tool_call_id` 配对的
  工具块；其余 role 两侧都视为坏信封而忽略。`Agent` 仍是父转录显式不收
  （子代理 trace 只在子会话里展开），但现在是记账表里一条有注释的条目。
- **测试**：`WireRow` 拓宽类型与 `legacyRow()` cast 删除，fixture 直接写
  生成的 `SessionRowTag`；补 legacy `Message` 的角色投影、工具配对与坏
  信封忽略用例。跨接缝方言分歧（issue 32）随之关闭。

### Removed (行词汇收口：旧公开形态删除)

- **两个零生产调用方的公开项**：`uva_session::KNOWN_SESSION_EVENT_TYPES`
  （33 个线上标签的字符串表）与 `uva_session::empty_event_of`（按字符串
  造空事件）删除。词汇自己的 `RowTag` 已是标签的事实来源，骨架由
  `RowTag::empty_event` 给出；契约目录与前端 union 从来读的是 `RowTag`
  的 schema，不是那张表。
- **旧的标签访问方法**：`SessionEvent::type_tag()` 删除，统一由
  `SessionEvent::tag().as_wire()` 回答；全仓调用点（events / builders /
  policy-plugins 测试）随之迁移。
- **源码文本与散文角色解析**：`log_surface` 测试里 `include_str!` 读
  `events/mod.rs`、从变体名推导标签、从文档散文解析角色的解析器与两条
  测试整体删除；枚举变体文档末尾的「surface message / log-only」散文
  标记也一并删掉——角色只在 `RowTag` 上声明一次，由测试对行为与分类器
  做对账，不再有第二份可被机器读的事实。
- **虚假声称**：删掉那张表「持久化层拒绝未知类型」的说法；文档改为
  按事实描述：写侧原样落盘，未知 `type` 是读侧的分类（`RowClass::Unknown`），
  不是持久化层的拒绝。
- **冻结面**：`scripts/frozen-api/baseline` 与 `docs/api-frozen.md` 同步
  删除两个名字，`uva-session` 冻结名 135 → 133。

### Added (行词汇门禁：生产代码不得自己拼标签)

- **新门禁** `make check-row-vocabulary`（`scripts/check-row-vocabulary.sh`）：
  生产代码（`crates/*/src/**`；内联 `#[cfg(test)]` 模块、`tests/` 目录、
  `tests.rs` 与被 `#[cfg(test)] mod NAME;` 拉进来的文件除外）不得在**标签
  位置**写出 33 个标签或 4 个方言字符串——`"type": "…"` 的裸写、
  `== "…"` / `!= Some("…")` 的比较、`Some("…")` / `json!("…")` 的包装、
  `"…" =>` / `"…" |` 的匹配臂、`=> "…"` 的变体映射都算。读行一律问
  `uva_session::classify_row`（或 `RowTag` / `RowDialect`），写行走类型化
  构造器。词表本身与 `events/vocab/mod.rs` 对账：改名后残留的旧拼写会让
  门禁先红。
- **审计例外表**（脚本内，一行一个 `文件|字面量|理由`）：不是行标签
  的同名拼写在标签位置之外本来就不触发；落在标签位置但并非行标签的拼写
  （operation 种类、memory 种类、`AgentEvent` 种类、provider 内容块种类、
  TUI 的消息 part 种类）才进表，不做目录级豁免。两处**范围**排除：词典
  模块 `events/vocab/mod.rs` 是拼写的定义处（按文件），`uva-test-support`
  是测试基础设施（按 crate——它的 `src/` 解析录制的 `events.jsonl` 夹具，
  且这个 crate 不依赖 `uva-session`）。
- **收尾迁移**：门禁第一次跑就抓到 8 处仍在按拼写认行的生产代码，一并迁
  到词汇上——`uva-approval::merge_grants` 的 grant 行过滤；`uva-session`
  的 `plugin` / `tool_output` 行边界（`bound_*_row`）、`writer` 的分块合并
  过滤、`repair::parked_prompts` 的停放/消费分派、`log_surface` 的旧分派器
  （改成对 `events::classify_row` 的薄适配，方言**解码器**留在原处）。行为
  不变（`uva-session` 334、`uva-approval` 8 等既有测试全绿），生产代码里
  不再有一处自己拼行标签。
- **自测**：`--self-test` 用固定 fixture 证明检测器仍会报出标签位置
  （`Some("…")`、`"type": "…"`、`json!("…")`、匹配臂、比较），并证明注释、
  非标签位置（`"turn": 1` 这类数据键）、整行 raw JSON 与内联 `#[cfg(test)]`
  模块不产生误报；`--print` 按站点列出每处命中的处置，供审计例外表。
- **清单同步**：`Makefile` 的 `check-*` 目标与 `make ci` 聚合、`lint-sh`
  清单（18 个脚本）、`.github/workflows/shellcheck.yml`、`AGENTS.md`
  §3.6 的门禁表与 `lint-sh` 行。

### Fixed (取消的轮也收尾：footer、工件集与名册不再随 dispatch future 一起丢)

- **收尾挪出可取消的 future**：父 run 取消时 harness 停止 drain 工具流、
  **丢掉** `task` 的 dispatch future，于是排在子 run 之后的三件事一件都不
  发生——`subagent_exit` footer 没写（子会话永远停在 `subagent_enter`）、
  工件集没落盘（与上面「取消也算」的承诺相反）、句柄表那行永远 `running`
  且没有 `idle_since`（TTL 也老化不了它，`agents_list` 一直谎报活着）。
  新的 `RoundTail` 把这三件事从 future 里挪出：正常路径 `land()` 一次做完，
  future 被丢弃时由 `Drop` 兜底——roster settle 同步完成，写入半边（footer +
  flush + 工件集）detach 到工具的 `Spawner`（`TaskTool::with_spawner`，
  默认 runtime-free 的 `InlineSpawner`；`uva-app-server` 装 tokio 实现）。
  `agents_message` 的追问/唤醒轮同一条尾。
- **测试**：直接丢弃 dispatch future 的用例断言行落到 `idle`（activity
  `cancelled`）且 footer 仍然落盘；真实 run 的用例让子代理在取消后慢慢
  收尾（超过父侧 150 ms 窗口，取消赢下这场竞速），断言子会话仍有
  `subagent_exit`。两条都在去掉 `Drop` 兜底后失败。

### Fixed (委派消耗回到 `/api/v1/chat/usage`)

- **子代理的 token / turn 计入部署计数器**：子 run 跑在自己的 harness 上，
  它的 `Usage` 与 `SessionEnded` 事件只进子会话自己的流，永远到不了父会话的
  `SessionController`——删掉旧委派 seam 之后，一次 `task` 委派在
  `/api/v1/chat/usage` 上**一格不涨**。新增 `uva-agent::SubagentUsage`
  接缝：`task` 工具与控制面在 drain 子事件流时把每步的 token 与每轮结束
  交给它；`uva-app-server` 的 `DelegatedUsage` 把数字写进
  `AppState::usage_metrics`（与控制器同一个 `Arc`，于是端点仍是进程级总量）。
- **测试**：工具侧断言子 run 的 `(11, 5)` 与一次 turn 到达账户；app-server
  侧断言 `DelegatedUsage` 累加进 counters 快照。

### Added (task 的 L1 进度分片：父页子代理摘要)

- **`task` 覆写 `Tool::stream`**：子 run 期间向父会话的工具块追加一行一
  子的摘要——`[child_id] status: activity`（`running` / `idle` + 当前活动
  一行）。分片是普通 `ToolOutput`：文本给人读，`metadata.children` 带
  `{child_id, status, activity}` 三个字段；它走既有的
  `SystemEvent::ToolProgress` 活性帧与 durable `tool_output` 行，重载由
  该行重放同一份摘要。child id 同时是 L2 入口（`tool_result` 尾部仍带
  `[subagent session: <id>]`）。
- **~150 ms 合并窗口**（`progress::L1Feed`）：每个 child 在窗口内只保留
  **最新**一行，窗口至多产出一个分片，默认实现按 `uva_core::Sleeper`
  等待（`TaskTool::with_sleeper` 可换成本运行时的 sleeper）。子 run 在
  一个窗口内结束时仍 `flush` 一行收尾摘要，快子代理不会静默。一分钟
  6 万次子活动的高频压测断言产出少于事件汇 1024 上限，且同一窗口内不
  重复发送。
- **活动来源**：子 run 的事件流——`SystemEvent::Progress` 的 LLM / 工具
  步进、每轮首个文本增量（取首行）、工具调用、轮次结束——映射为一行；
  控制面的追问轮不占原 `task` 工具块。crate README 与根文档同步。

### Changed (插件拆分：删委派 seam，`task` 变普通工具)

- **委派通道退役**：`DelegatingTool` / `DelegationCall` / `Delegators`
  整个删除——`uva-harness` 的 `delegation.rs`、`ReActAgent::with_delegator` /
  `with_subagent_writers`、`Host::delegator`、`ToolCaps::delegators`、
  `EnvCaps::subagent_writers`、`RunConfig::{delegators,subagent_writers}`、
  `route.rs::try_delegation`、`AgentInput.subagent_{depth,writers}` 全部移除。
  `task` 是普通 `Tool`：`uva-tool-plugins::task` 全量迁入独立 crate
  `uva-tool-task`，不依赖 harness——执行时查 `AgentRegistry` 解析 peer、
  用 `SubagentWriters` 开子档、内联驱动 `agent.run()`，注册/曝光/限制/参数
  校验一律走 `ToolRegistry`。深度上限（3）与每会话并发（8）由工具自查，
  取消经 `uva_tool::Context::cancel` 传给子 run。
- **事件与父日志**：`AgentEvent::Agent(AgentMeta, …)` 变体删除，子 run 的
  事件不再进入父事件流；父会话只以带 child id 的普通 `tool_result` 行收口，
  子会话保留完整、可续聊的转录——写者半边保留：
  `SubagentWriters::open_child(child, parent, depth, agent, prompt)` 照写
  `subagent_enter` + 派发词，路由半边（`SubagentRouter::route*` /
  `append_to_parent`、`persist.rs` 的事件拦截）删除。
- **冻结面**：`DelegatingTool` / `DelegationCall` / `AgentMeta` 三个名字离开
  `uva-harness` 的 curated surface（64 → 61），`scripts/frozen-api/baseline`、
  `docs/api-frozen.md` 与本文同变更更新；`make check-mvp-deps` 的
  `FAMILY_TOOL_CRATES` 改为 `uva-tool-plugins` 七 feature + `uva-tool-task`。
- **测试与示例**：delegation 测试族迁为 `uva-tool-task` 的
  `task_delegation` / `worktree_isolation` 集成测试（深度守卫改由工具自查
  驱动），两个示例随工具搬入该 crate。

### Added (task 注册为普通工具：`uva-tool-task` 与工具上下文取消通道)

- **`task` 进入工具注册表**：派生子代理改走普通 `Tool`——`task` 经
  `ToolRegistry` 注册（定义 / 曝光 / 限制 / 并发模式全走注册表现成机制），
  执行时解析同伴 `AgentRegistry`、开子会话、内联驱动子 run。本票落地时旧委派
  seam 原样并存、全绿；它随后由上面的「删委派 seam」整体退役。
- **`uva-tool-task` 成为第 31 个 workspace member**：`gate`（子 run 后的验证
  命令）与 `worktree`（git worktree 隔离）随工具从 `uva-tool-plugins` 迁入。
  crate 无 feature，依赖固定为 agent / 事件词汇 + 工具 trait，不依赖
  `uva-harness`。
- **`uva_tool::Context` 加取消令牌**：每次工具派发都带 `cancel`
  （`Arc<dyn CancelToken>`）；`Context::new` / `default` 构造未取消的令牌，
  `with_cancel` 是链式注入点。`task` 把父 run 的令牌交给子 run——父取消连带
  子终止（子会话收尾行）。形状变化记入 `docs/api-frozen.md`。
- **并发与深度**：一个 `task` 调用起一个子代理；工具声明
  `ExecutionMode::Sequential`，批次调度器把同一批里的 `task` 调用串行化（委派起整个
  run，批次内并发会改动取消与批次语义），因此没有每会话信号量；嵌套深度上限 3（工具自查，
  超限返回模型可读的错误结果）；child id 在派发前铸造，供子会话、工件与控制面
  共用。
- **父侧行**：子会话仍以 `subagent_enter(child, parent, depth, agent)` 开档；
  父会话只落 `tool_call` / `tool_output` / `tool_result`，`tool_result` 带
  child id 收口。

### Fixed (translate 产出可开机的完整 profile 文档)

- **迁移输出带上整个装配**：`translate` 原先把迁移结果写成**只有
  `model-adapter` 一行**的文档。文档层是权威装配——文档没点名的行就是
  不挂载的插件——因此那份文档开机即失败：`tool-registry` 缺失 ⇒
  `required service "tools" is unavailable`，依赖链与 agent loop 一起消失。
  现在输出 = 内置插件集合（22 行，`native_profile()`）+ 迁移后的
  `model-adapter` 行，行序与设置面写回的形状一致（`ProfileStore::to_toml`）。
- **`agent-loop` 行也进文档**：默认 agent 的 strategy 别名、allow/deny
  工具列表、compaction 策略按**与 boot 相同**的解析器（
  `state::boot::{load_default_tool_restriction, load_default_compaction,
  agent_loop_row_config}`）物化进该行；`agent_loop_row_config` 因此从
  `state/app_state/boot.rs` 迁到 `state/boot.rs`，与其它 `load_default_*`
  读取器同处，`state::boot` 升为 `pub(crate)`。
- **报告同步**：mapped 行列出 agent-loop 的迁移；unmapped 不再把
  `default_agent` 当未迁移项；另加一行说明「内置插件集合被带过去」。
- 回归：`tests/translate.rs` 新增「输出带全装配 + `agent-loop` 行携带
  策略 + 其余行不带 config」用例；等价性由真二进制验过（新输出的文档
  `core --directory` 起得来、22 行全 mounted）。

### Changed (profile 文档层落到工作区配置层，不再写进仓库根)

- **文档层缺省路径**：`<workspace>/profile.toml` → `<workspace>/.agents/profile.toml`。
  profile 文档是设置面的**生成物**（整篇重写），落在工作区根时，每个跑
  server 的源码 checkout 都会多出一个未被 `.gitignore` 覆盖的
  `profile.toml`；新落点与 provider 回退输入 `<workspace>/.agents/config.toml`、
  种子技能 `<workspace>/.agents/skills/` 同一层，`.agents/` 本就是仓内已忽略的
  本地层。
- **读、写、HMR 三处一致**：`ProfileInputs` 的 `resolve`（读）、`store`
  （设置面写回的提交目标）、`watched_paths`（HMR 轮询）共用同一个
  `document_path()`；`--profile` 显式指定时仍然优先。
- **首次写入自动建层**：`ProfileStore::commit` 先建文档所在目录，再走
  「临时文件 + rename」的原子替换。
- **旧位置给出告警**：工作区根仍有 `profile.toml` 时，boot 在 stderr 上点名
  它不再被读、并给出当前文档层路径，不再静默按内置装配启动。
- **home patch 不动**：`~/.agents/profile.toml`（或 `UVA_HOME_PROFILE`）仍是
  全局层的 patch 文档，与项目层的文档层靠根目录区分。

### Added (子代理控制面：`agents_list` / `agents_message` / `agents_interrupt`)

- **`uva-tool-task` 的 `control` 模块**：`task` 派发之后的三个模型工具，
  与 `task` 同属一块 brick（`TaskToolPlugin` 一并登记，卸载一并撤走），
  定义/曝光/限制/并发模式全走工具注册表。

  - `agents_list`：列本会话子代理的 id / 名 / 状态 / 当前活动。注册表是
    P6 交付物，本期用日志扫描兜底——子会话以
    `subagent_enter(child,parent,depth,agent)` 开档，读各会话头部即可反查
    本会话的孩子（含上一个进程派发的）；扫描逻辑在 crate 内，宿主经
    `SessionIndex` seam 提供会话访问（`session_ids` / `read_head` /
    `read_tail` / `writer`）。
  - `agents_message`：按目标态分流——running → steer（写子 run 的转向
    通道，子日志留下 `source: "steer"` 的用户行）；idle（在内存）→ 追问
    （同一会话写入者再跑一轮）；本进程不持有 → **冷唤醒**（读子日志头取
    agent 名 → 解析 peer → 用该子会话写入者再跑一轮，进入 running）。
  - `agents_interrupt`：点火该子保留的取消令牌，当前轮终止 → idle（不是
    杀死，仍可续聊）；硬终止才是 `aborted` 终态。
- **生命周期状态机**（`control/state.rs`，纯逻辑）：`running` / `idle` /
  `parked` / `aborted` 四态与迁移表以原型 reducer 为准；非法迁移被**拒不**
  并给出模型可读错误，既不静默生效也不挂起。全表（state × operation）
  有测试。
- **句柄表**（`control/registry.rs`）：每子保留转向通道发送端、取消令牌、
  会话写入者与 peer；`task` 派发时入表、子 run 结束时结算，冷唤醒的实例
  也归它所有。子 run 的取消令牌改为 `ChildCancel`（父令牌 + 子自己的触发
  器）：父取消仍即子终止，`interrupt` 只停一轮。每一轮都换新令牌——被
  打断的轮留下的令牌是已点火的，不能被下一轮继承。
- **服务端装配**：`uva-app-server` 的 `ServerSessionIndex` 实现该 seam
  （注册表 + 各用户目录扫描，重启后仍能找到子会话），`SessionIndexSlot`
  在挂载后由 `boot` 填好；`tool-task` 行把 slot 交给工具。
- **门禁与文档**：`uva-tool-task` 范式面基线 6 → 7；crate README、
  `docs/architecture.md` crate 表与 crate 根文档同步。

### Changed (工具流式：活性追加语义 + durable `tool_output` 行)

- **活性投影追加不结算**：`LiveTurn::apply` 收到
  `SystemEvent::ToolProgress` 不再经 `settle_call` 结算，而是把分片**追加**
  到该调用的 `output` 并保持状态 `running`（词表仍是 `running` /
  `completed` / `failed`）；帧指纹累计运行中调用的输出长度，因此等长分片
  也推进 `live_seq`。终态只由 `tool_result` 决定——真实结果落地前，调用不
  会显示为完成。
- **新 durable 行种 `tool_output`**（log-only，部分输出）：会话词汇、
  `type_tag`、`surface_role`、`empty_event_of`、builder、线类型并集、
  重生成的 `types.ts`、web 行处理表、TUI 忽略表全部就位；行经会话写入者
  落盘（`check-single-writer` 不变），落库点在工具路由的分发处
  （`drain_tool_stream` 的 `Progress` 分支）+ `EventSink` 的新追加路径。
  重载由该行重放：web 帧投影把分片追加进待定工具块，终态
  `tool_result` 到达后整体替换。
- **`shell` 真正流式**：覆写 `Tool::stream`，`spawn_reader` 的 8 KiB 读取块
  逐块产出 `Progress`（stdout 与 stderr 都在内），命令结束时仍恰一个
  `Result`；`run` 返回后先排空已缓冲分片再结算，分片不会因结果先就绪而
  丢失。`call` 不变（无 progress sink）。
- **`tool_output` 行有界且按调用合并**：写入者 ingress 给每行的
  `data.output` 一个尾部窗口（`MAX_TOOL_OUTPUT_BYTES` = 64 KiB，按转义后的
  形式度量），超出时保留**最新**字节并把被概括的字节数记进
  `data.original_bytes`；同一 `call_id` 在两次 flush 之间的分片合并成一行
  （写者每调用只持一条尚未落盘的待写行）。一段输出 N 字节的命令因此不再
  把日志放大到 ≈N 字节（这些行是 log-only，compaction 遮不住），写者内存里
  也不再驻留整段原始输出。界对自身幂等：重写的行不会被二次裁剪。
  四类事实写进 `docs/log-format.md` 与 `SessionEvent::ToolOutput` 的文档。
- **运行期有 flush 边界**：分片只在 flush 时落盘，而此前唯一的 flush 在整次
  工具结束后——长跑工具中途崩溃就把分片全丢了。`drain_tool_stream` 现在每
  `TOOL_OUTPUT_FLUSH_CHUNKS`（32）个分片 flush 一次（失败只记日志，行仍欠在
  spill 里由下一次 flush 重试），进程死在工具中途只丢最后一个边界之后的分片；
  `EventSink::append_tool_output` 的措辞同步为「从下一个 flush 边界起持久」。
- **live 帧有界**：`LiveToolCall.output` 运行时只保留**尾部窗口**
  （`MAX_LIVE_TOOL_OUTPUT_BYTES` = 32 KiB，结算文本同窗口封顶），帧大小不再
  随调用累计输出线性增长——N 字节输出的 follow 流量从 O(N²) 降为 O(N)；帧
  指纹新增「累计流式字节数」维度（不上线），窗口满了之后等长分片仍推进
  `live_seq`。
- **冻结面**：`uva-session` 的 `pub use` 名单新增 `tool_output`
  （127 → 128），基线、`docs/api-frozen.md`、日志格式文档与
  `CHANGELOG` 同变更更新。

### Added (hook 实现族：`uva-hook-plugins`，一模块一 feature)

- **新 crate `uva-hook-plugins`**：hook 实现族，`default = []`，每个
  feature 只开一个模块与其专属依赖。首批三个模块：

  - `reflection`（自 `uva-policy-plugins` 迁入）：仍实现
    `uva_hook::Hook`，安装路径与行为不变；原策略族的 feature、
    re-export、测试与 example 一并移出。
  - `redact`：`ResultTransformer`，在循环提交工具出参前脱敏。
    默认规则（`Bearer ` / `sk-` / `ghp_` / `github_pat_` / `AKIA` /
    `ASIA` / `xoxb-`）写成可配置的 `RedactConfig` 并在模块文档里
    列表说明，规则只匹配 token 边界与足够长的尾串；文本 part 与
    `ToolOutput::metadata` 的字符串值都覆盖。改写由循环写一条
    `tool_result_transformed` 溯源行（`transformers=["redact"]` +
    前后 sha256），语义仍是 provenance。
  - `traffic-light`：`Gate`，在 `tool/pre-execute` 查询工具注册表并
    调用该工具自己的 `Tool::risk(&arguments)`，按 `TrafficPolicy`
    放行 / 阻断；默认阻断 `Risk::High`，`Unknown` 默认放行，可显式
    加入阻断集。阻断走 `Block`，harness 写 durable `hook_block` 行。
- **内建五 guard 不入此族**：它们仍住 `uva-steering::guards`，循环
  启动时经 `Host::default_policy` 装一次。
- **门禁与文档**：crate 进 `PLUGIN_CRATE_ROOTS`（plugin-shape 预算
  重算）、`check-publishable` 的 `ORDER`、`RUNTIME_FREE`、范式面
  基线（`pub use` 3 条）、markdownlint 清单、`check-mvp-deps` 的
  all-features 禁依赖断言与 standalone 构建；架构表 / `AGENTS.md`
  §1.1 / `docs/plugins.md` / 根 README 同步，workspace member
  28 → 29。
- **facade**：`reflection` feature 改指向 `uva-hook-plugins`，新增
  `redact` 与 `traffic-light` feature / 模块。

### Changed (agent 词汇下沉：uva-agent，冻结面路径保持)

- **词汇迁入 `uva-agent`**：`Agent` trait、`AgentInput`、转向通道
  （`RunInbox` / `ChannelInbox`）、peer 注册表（`AgentRegistry` /
  `AgentEntry`）、子会话写入者来源 `SubagentWriters`，连同 prompt manifest
  （`PromptContext` / `PromptSection` / `DEFAULT_SYSTEM_PROMPT`——`AgentInput`
  的 `prompt_context` 字段命名它，所以 manifest 随输入一起下沉）与注册表
  测试族整体自 `uva-harness` 迁入这个新 crate；依赖只有
  `uva-core` / `uva-provider` / `uva-session` / `uva-approval` /
  `uva-events` 与 `async-trait` / `futures` / `parking_lot` / `tracing`，
  lib 依赖树不含 tokio——runtime-free。
- **`AgentCaps` 与具体循环留 harness**：`ReActAgent`、`AgentCaps`、
  `Host`、策略分派与 sink 不动；`AgentInput` 的 `subagent_*` 字段本阶段
  保留（删除归子代理轴删 seam 的票）。
- **路径保持**：`uva-harness` 根仍以同一条合并 use 树导出这十个名字，
  `input` / `prompt` 模块保留为 re-export（harness 内部 `crate::input::…`
  引用零改动），`agent` 模块继续 re-export `Agent` / `AgentEntry` /
  `AgentRegistry`，`agent::host::PromptSection` 深路径照常解析；crate root
  的 `pub use` 语句数仍为 1，`check-frozen-api` 基线与
  `docs/api-frozen.md` 一行未改。跨 crate 使用的两处由 `pub(crate)` 放开为
  `pub`：`AgentInput::to_message`、`PromptContext::has_renderable_sections`。
- **facade 与部署侧**：`uva` 新增 `agent` feature/模块（`harness` 依赖
  它）；app-server 的注册表构造、搜索插件、prompt manifest 与 peer 测试
  改指 `uva::agent`。
- **门禁与文档**：`uva-agent` 进范式面基线（4）、`RUNTIME_FREE`、
  `check-publishable` ORDER（`uva-events` 之后、`uva-harness` 之前）、
  markdownlint 清单（29 → 30）、架构文档 crate 表与 `AGENTS.md` §1.1，
  workspace member 29 → 30；
  `docs_truth` 的深路径计数按 rustdoc 重算（36 条 `agent` 路径 / 22 条
  门面名 / 14 个深路径名，加 `uva-hook` 七个挂载点键仍为 21）。

### Added (hook 注册表接通外部插件：`hooks` 服务、可撤销注册、对外门禁)

- **`uva-hook::HookRegistry`**：按七个挂载点键注册 / 撤销的四类拦截
  （`Hook` / `Gate` / 请求与结果 transformer），返回可撤销句柄；
  仲裁为 dsh 单调拒绝——首个 `Block` 短路、`Ask` 收集全部理由、
  `Terminate` 只走 `HookEvent::accepts` 的五点表，折叠只有拒绝没有
  放行。`Arbitration` 从 harness 移到契约 crate，harness 路径
  （`uva_harness::Arbitration`）由 re-export 保持。
- **`HookPlugin` 发布 `hooks` 服务**：`uva-runtime` 的
  `AgentLoopServicePlugin::resolve` 在挂载时用 `ctx.get` **机会式**解析该
  注册表进 `AgentLoopDefaults`，每次 run 的表穿进 `Seams`；`Seams` 的每次
  分派先跑注册表再跑 host 的 typed 注册，共用同一仲裁。`hooks` 不是
  `AGENT_LOOP_INJECTS` 的边——注入键是 kernel 静态校验的硬依赖，缺 provider
  会拒绝整组，等于把 `hooks` 行变成必选、让写在它之前的 profile 文档与
  把行设为 `Off` 的部署在 boot 期失败。原生 profile 的 `hooks` 行仍排在
  `agent-loop` 之前，在场时先挂载。
- **外部否决写行**：注册表在 `tool/pre-execute` 判 `Block` 时，由
  harness 写 `hook_block` durable 行，收口"外部短路不写行"的旧缺口；
  `Terminate` 在同一挂载点拒了调用时写同样的行（拒绝的形状有两种，
  重载会话都该知道是谁拒的）。
- **gate 失败真的 fail-closed**：`Err`/panic 不再一律合成 `Block`
  （那个决定在 5 个挂载点被 `HookEvent::accepts` 丢弃，等于放行）——
  按挂载点合成该点接受的决定：`tool/pre-execute` 用 `Block`，其余能
  结束 run 的点用 `Terminate`。`llm/request` / `llm/result` 不接受
  任何拒绝，失败改由 `RegistryPass::failures` 上报（harness 落 `hook`
  warning），绝不折成 `Allow`；两者都是 `tracing::error` 级别诊断。
- **样板与门禁**：`docs/examples/hook-only-consumer` 以 `uva-hook` 为
  唯一 normal 依赖（harness 只进 dev-deps），dev 测试驱动真实 run
  断言 handler 读到工具名 / 调用 id / 入参、`Block` 短路、注册可撤销；
  新门禁 `make check-hook-external` 在样板内跑
  `cargo tree -e normal` 断言无 `uva-harness` 并跑其测试。判据与
  接线见 ADR 0004。
- **facade**：`uva` 新增 `hook` feature / 模块（`harness` feature
  依赖之）。

### Changed (事件词汇下沉：uva-events，冻结面路径保持)

- **词汇迁入 `uva-events`**：`AgentEvent` / `SystemEvent` / `WarningKind` /
  `AgentMeta` / `SessionEndReason` 连同 `events` 测试族整体自 `uva-harness`
  迁入这个新 crate；依赖只有 `uva-provider` / `uva-tool` / `uva-session` /
  `uva-approval` 与 `serde` / `serde_json`（`Suspension` 载荷在
  `uva-approval`），lib 依赖树不含 tokio——runtime-free。
- **路径保持**：`uva-harness` 根仍以同一条合并 use 树导出这五个名字，`events`
  模块保留为 `pub use uva_events::{…}` 的 re-export，`uva_harness::events::…`
  深路径与全部消费方 import 零改动；crate root 的 `pub use` 语句数仍为 1
  （预算未动），`check-frozen-api` 基线一行未改。harness 内部直接引用改指
  `uva_events`；`SessionEndReason::as_str` 因跨 crate 使用由 `pub(crate)`
  放开为 `pub`。
- **`AgentEvent::Agent` 变体保留**：删除归子代理轴后续票，本票只搬家。
- **门禁与文档**：`uva-events` 范式面基线 0 → 1；facade 的 `events` 模块从
  doc-only 换成真 re-export；架构表 / `AGENTS.md` §1.1 / crate README 同步；
  `docs_truth` 的深路径计数按 rustdoc 重算（事件词汇搬出后 rustdoc 不再在
  harness 下记录它们的路径；与 hook 下沉合流后为 40 条 `agent` 路径 /
  26 条门面名 / 14 个深路径名，再加 `uva-hook` 的七个挂载点键）。

### Changed (hook 契约下沉到 `uva-hook`：路径保持，`HookContext` 删 sink 字段)

- **拦截契约搬到循环之下**：`Hook` / `Gate` / `RequestTransformer` /
  `ResultTransformer` 四个 trait、七个挂载点键、解耦后的 `HookContext` 与
  waterfall 载荷 `HookPayload` 住进新 crate `uva-hook`（依赖
  `uva-steering` / `uva-context` / `uva-core` / `uva-tool` / `uva-kernel`，
  不含 `uva-harness`）。判据是依赖树，不是键的可见性——见 ADR 0004。
- **冻结面不动**：`uva-harness` 以合并 use 树 re-export 全部名字，crate 根
  （`uva_harness::Hook` 等，use 树语句数不增）与既有深路径
  （`uva_harness::agent::host::*`、`uva_harness::seams::TOOL_PRE` 等）全部
  保持；七个键在 `seams` 模块内部 re-export，不新增 crate 根名。
- **`HookContext` 删 `events` 字段**：上下文只剩 `state` 与 `cancel`。
  `hook_block` 否决行改由循环显式传入 sink 写入（`Seams` 的各分发方法
  与 `Host::dispatch_hooks` 多了该参数）；写入者、行内容与行序都不变。
  `SeamPass` 由 `HookPayload`（`HookEvent` + 上下文）替代，`SeamRuling`
  作为 waterfall 短路值留在 harness 内部。
- **行为零变化**：既有 hook / gate / transformer / guard 测试全绿；
  `reflection` 插件改指 `uva_hook::Hook`，安装路径不变。

### Changed (审批自成一 crate：uva-approval，冻结面路径保持)

- **新 crate `uva-approval`**：审批词汇（请求 / 策略 / 授予 / 恢复令牌 / 决定）、
  从会话行推导的决策函数（开放集 / 覆盖检查 / 授予折叠），以及发布 `approval`
  服务的内核插件，整体自 `uva-harness` 下沉为独立 crate。依赖只有
  `uva-tool` / `uva-session` / `uva-kernel` 与序列化 / 摘要件——依赖树不含
  harness，runtime-free（进 `check-no-runtime` 名单与可打包顺序）。
- **冻结名路径保持，基线零变化**：`uva-harness` 的 crate root 以**一条合并 use
  树** re-export 全部 21 个审批名（`pub use` 语句 5 → 1，语句预算只降不升），
  `check-frozen-api` 抽名字不抽路径，`scripts/frozen-api/baseline` 与
  `docs/api-frozen.md` 一行未改，消费方零 import 改动。harness 深路径上的
  `agent::approval` 模块随之消失（深路径本不在承诺内），`docs_truth` 的深路径
  计数同步重算（57 → 39，跨 crate 深名已不存在）。
- **`ApprovalGate` 暂留 harness 作薄适配器**：它实现 harness 的 `Gate` trait
  （其上下文仍带 harness 私有的 `EventSink`），待拦截契约（P2 `uva-hook`）下沉后
  整体迁入 `uva-approval`；插件侧的 `ApprovalPlugin::gate`（生产零调用，且返回
  harness 类型）随插件半边迁出而删除。挂载行、服务键、审批问 / 决 / 挂起 /
  恢复全链路照旧，生命周期测试族全绿。
- **facade 与门禁同步**：`uva` 新增 `approval` feature / 模块（`harness` feature
  依赖之，app-server 的内核工厂与 wire 契约目录改指 `uva::approval`）；
  `uva-approval` 进范式面基线（`PARADIGM_CRATES` + 语句数 2）、markdownlint
  清单、架构文档 crate 表与 `AGENTS.md` §1.1。

### Fixed (活性投影认得工具结果：被拒的调用不再是 running)

- **被拒 / 失败的工具调用在活性转录里一直读 `running`，直到持久行抵达才退役**：
  harness 把每一次工具结果（成功、失败、否决）作为 `ContentPart::ToolResult`
  发在活事件流上，而活性投影 `LiveTurn::apply` 没有这一条臂——它只认
  `ToolProgress`（只有改写了 `stream()` 的工具才发）。于是被守卫否决的调用在活性
  投影里永远停在 `running`，web 把它渲染成「写入中…」，而重载后的时间线读同一条
  `tool_result` 持久行说的是「失败」——活发与重载讲的不是同一件事。
- **投影现在认出工具结果**：加了 `ContentPart::ToolResult` 臂，被回答的调用立刻
  落成 `completed` / `failed`，输出文本也随之到位。活性帧与持久行出自同一次提交，
  两边说的是同一件事。线的形状不动（`LiveToolCall` 早有 `status` 与 `output`），
  模型可见表面与持久行格式都不动。
- **帧指纹不再把状态当长度数**：活性帧的计数器此前把 `status` 的**长度**并进和里，
  而 `running` 与 `failed` 只差一个字母——一条只回一个字答案的失败调用会让指纹不变，
  帧计数器不涨，客户端就留着那条写着 `running` 的旧帧。状态现在单独占一个维度。
- **TUI 无需配套改动**：它的活性路径只读文本 / 推理 / 运行态，工具活动一律从持久行
  渲染，没有「活着的工具调用」这回事。

### Changed (反馈目标改成服务端可推导的序号)

- **`chat/feedback` 的目标从「客户端标签」变成「被评价那一行的序号」**：web 此前发
  浏览器端 `crypto.randomUUID()`（从不落库），TUI 发 `msg-<助手消息计数>`（它自己视图里
  的计数，不同会话可能都是 `msg-1`），服务端照单全收——这个字段什么也标识不了。现在
  客户端发那一行的**序号**（`seq`，十进制），服务端在本会话的日志行里解析它，且要求
  那行是 **assistant 消息**；解析不到（或解析到的不是助手行）就拒绝，机器码
  `feedback_target_unknown`。对得上的行照旧落库。
- **字段名与类型不动**（`Feedback.message_id: String`）：日志格式的版本闸门「不迁移、
  只拒绝」——版本号一动，所有现存会话日志就读不出来了。改的是**含义**：文档写明它是
  被评价那一行的序号（十进制）。行形状逐字节不变，`CURRENT_SCHEMA_VERSION` 不动；
  改动前写下的反馈行带着客户端标签，读者不得把它当序号读。
- **web 没有序号的行不给反馈按钮**：活性的那条（`live:<turn_id>`，从来不是日志行）与
  旧日志里没有序号的行（web 的合成计数器与服务端编号不一致）此前要么发不出反馈、
  要么发一个对不上任何行的标签；现在显式不给按钮，比报错更诚实。服务端拒绝时弹
  toast，不吞。
- **TUI 把行的 `seq` 穿进 `TranscriptLine`**：`resume_material` 发**本轮** assistant
  行的序号，不再发计数。轮次从操作员最后一次提问起算——评的是这一轮的答案，更早
  一轮的答案属于别的轮次，不再被指名；本轮只写了思考内容（没有可评价的答案）时，
  两个表面都不给反馈目标。web 的投影按同一条边界拆气泡，两个界面因此指认同一行。

### Changed (TUI 的行文字把多段文本隔开)

- **助手行与用户行的多个文本段不再粘连**：此前一段接一段直接拼起来
  （`part onepart two`），读起来像一个词；现在段与段之间留一个空格——那些段是一条
  消息的几块，不是粘在一起的一个词。工具输出的那一行上一轮已经这样处理。
  `content_text`（逐段原样拼接）保留给活副本的比对：服务端的活性文本就是逐段拼接
  出来的形状，比对必须用同一个形状，它的行为不变。

### Fixed (两个人类表面不再无声丢行)

- **行词汇表进前端类型**：`SessionTurn.type` 从开放字符串收成生成的
  `SessionEventType` 联合——`uva-app-server` 的契约目录以
  `KNOWN_SESSION_EVENT_TYPES` 为源收录该枚举（测试钉住两者相等），
  `npm run generate:types` 把它落进 `uva-web/src/api/types.ts`。旧方言照旧
  可读：`UserInput` / `Model` / `Agent` 与无 `type` 的行走 `legacyRowType`
  放宽比较，路径不变。
- **web 渲染器逐标签记账**：`session-to-messages.ts` 的行分发改为
  `Record<SessionEventType, handler | null>`——渲染与「设计上忽略」都写成
  显式条目（step / usage 等日志行按设计不渲染），词汇表加标签而记账表不
  跟上，`tsc` 直接红。
- **TUI 同一套记账**：`apply_row` 区分 `APPLIED_ROW_TYPES` 与
  `IGNORED_ROW_TYPES`，测试 `every_known_row_kind_is_applied_or_ignored_by_design`
  对 `KNOWN_SESSION_EVENT_TYPES` 全量断言——新标签没有归属即测试失败。
- **`warning` 行渲染为转录通知**：严格折叠被拒（`strict_fold_refused`）与
  恢复接受漂移（`config_drift`）是关于会话的持久事实，此前操作员只看得到
  日志行，两个人类表面都无声。现在是一行置灰通知——web 的
  `warning-notice`、TUI 的 System 行——不是聊天气泡，也不是 toast。
- **TUI 看得见工具活动**：转录此前只渲染助手的文本——`tool_use` 部件被
  `part_text` 丢掉，`tool_result` 行在 `IGNORED_ROW_TYPES` 里，于是模型调过
  什么工具、成没成、失败在哪，操作员一个字也看不到（web 两个都渲染）。
  现在：助手行里的 `tool_use` 部件渲染成一行调用（`assistant tool call: <名字>`），
  `tool_result` 行渲染成一行结果（`tool <名字>: ok` / `: failed`），工具名由
  `call_id → 名字` 的连接得到（助手行登记，结果行按 `tool_call_id` 读回；结果
  先于调用到达时只说「unknown tool」，不 panic）。`hook_block` 从「设计上忽略」
  挪进已渲染的一侧，渲染成一行通知（`system hook: <原因>`）——行格式与模型可见
  表面都不变，改的是终端这一侧的本地转录。
- **`hook_block` 行有人写了**：策略否决工具调用时，harness 现在落一条
  `hook_block` 持久行——`data.hook` 是策略名（hook / gate / guard 的注册名）、
  `data.reason` 是它给的理由、`data.tool` 与 `data.call_id` 是被拒的调用。此前
  该行种只有解析臂没有写者：否决只活在**实时**的
  `SystemEvent::Warning{kind: Guard}` 里，重载会话后就没了——`tool_result` 行
  只说「有东西拦了」，不说是谁。该行是 log-only：折叠忽略它，模型可见的表面
  逐字节不变。
- **web 把 `hook_block` 渲染成自己的通知**：独立的 `hook_block` 段类型——不是
  `warning` 段，被策略拦下不是生命周期告警——一行置灰通知
  `hook-block-notice`，带 `⛔` 字形与自己的强调色。它是时间线注记，永远不会
  成为反馈 / 重生成的目标。

### Changed (生产日志量变了：策略否决每次都留一行)

- **装了 `Host::default_policy` 的部署，日志量多了一类行**：每一次 shell 拒绝、
  工作区边界、循环检测、工具预算、提示注入的否决，现在都会落一条 `hook_block`
  持久行（`data.hook` 策略名 / `data.reason` 理由 / `data.tool` 与 `data.call_id`
  被拒的调用）。行种早已存在，这不是格式变更——但生产日志按否决次数多了一行。
  web 把它渲成 `⛔ 策略拦截 · …`，TUI 渲成 `hook: <策略> blocked <工具> — <理由>`。
- **守卫否决的活性 `SystemEvent::Warning{kind: Guard}` 退役**：同一次否决现在由
  `hook_block` 行这一份事实承载，且该行在否决当步就落盘、`follow` 客户端运行中
  即可读到——实时时间线与重载后的时间线讲同一件事。守卫严重度仍在模型可见的
  否决消息（随 `tool_result` 行落盘）与循环的结构化日志里。工具限制 / 高风险
  委派 / 审批不可用等**没有**持久行的否决路径，活性告警保持不变。hook / gate 在
  `tool/pre-execute` 上的否决是这份清单外的已知重复：宿主注册的 hook / gate 被拒时
  **既有** `hook_block` 持久行、也仍发一条活性 `Warning{Guard}`——同一份事实两个
  词汇。它退役不了：外层内核中间件在同一 seam 上也能产出 `Block` 而**不写行**，
  循环分不出这两种 `Block`，退役就会让中间件那一路失去唯一的通告。这条活性告警
  没有任何表面渲染（web 与 TUI 都只读持久行），所以重复目前没有可见效果。

### Changed (压缩区间与折叠坐标对齐)

- **折叠把区间与它引用的行对上**（安全网，先行落地）：`validate_replace` 现在还
  检查 `source_event_seqs[i]` 必须是表面第 `start + i` 行的序号，且引用条数与
  区间宽度一致。对不上就是 `FoldError::SpanMismatch` / `FoldError::CitationCount`：
  严格折叠报错，宽松折叠跳过该次压缩并告警，模型看到更多历史。改动前，区间与
  引用各说各话时折叠照样通过——损坏是静默的。
- **折叠本身就是模型可见顺序**（主改动）：推迟规则（steer 夹在 `tool_use` 与
  `tool_result` 之间时先按住，等结果落地再释放）从 `history` 的投影搬进持久化
  折叠（`surface::ModelOrder` + `surface::fold::SurfaceFold`），原始折叠、类型化
  折叠、增量账本与 token 计量共用同一台引擎，不会各自漂移。持久化的
  `Replace.start/end` 的含义不变——「模型可见表面里的位置」，这本来就是
  `FoldedSurface` 写明的契约，现在实现追上了文档。行格式逐字节不变。
- **投影不再自己实现顺序规则**：`history::SurfaceProjection` 退化成解码器
  （只保留旧方言的 assistant 分片合并），与折叠不再各持半条规则。实时转录
  （`harness::Transcript`）改用同一条 `ModelOrder`，实时路径与恢复路径给出同一
  个顺序。
- **迁移失败即拒**：旧坐标写下的压缩行（区间与被引用行对不上）被严格折叠拒绝、
  被宽松折叠跳过并告警。被跳过的压缩只是让模型看到更多历史，比遮错区间诚实。
- **压缩不再遮住自己没有序号的行**：实时路径里宿主代写的 steer 没有自己的行号
  （占位 `0`），计划因此在它之前收口；转录还按住一个用户回合时，整次压缩被跳过
  ——折叠同样会拒绝这种压缩。
- **服务端恢复路径回退到宽松折叠，而不是丢掉整段对话**：严格折叠报错时，
  `uva-app-server` 原先吞掉错误、用**空历史**开始这一轮——日志里仍然持有的那些
  回合就此全丢，比它替换掉的情况更糟。现在它回退到宽松折叠
  （`fold_log_surface` + `surface_to_messages`）：被拒绝的压缩被跳过，它遮住的
  区间被重放，模型看到日志真正支持的那段历史。harness 路径不变：折叠失败仍然是
  `SessionEndReason::Error`。
- **账本的尾巴是暂定的**：`SurfaceLedger` 的文档原来说它「与 `fold_log_surface`
  一样」地应用 `compaction` 行，因此 `resolve` 给出的区间与序号就是持久化折叠会
  重现的那一份。差一处：折叠在日志末尾会释放被按住的回合，账本不会——还按住的
  用户回合不在账本的行里，`resolve` 对它报 `UnknownKey`。文档现在写明这件事：
  账本的尾巴在运行结束前是暂定的；要折叠自己的答案，就去折叠日志。
- **`system` 行按用户行的规则推迟**：旧的投影里 `Role::System` 的行从不推迟，
  新折叠把它归为用户行，于是夹在 `tool_use` 与 `tool_result` 之间的 system 行会
  挪到结果之后。这是「同一套规则、同一份字节」里一处没有点名的变化，现在点
  名了：`surface` 模块的不变式清单写明 system 行按用户行的规则推迟，规则写在
  **行的种类**上，类型化折叠与原始折叠因此不会给出两个答案。

### Fixed (the front-end method gate was green about a table it never read)

- **门禁不再对读不懂的 `METHOD` 表保持沉默**：`scripts/check-web-methods.sh` 的
  解析器只认一种写法（`const METHOD = {` 加上单引号的值），其余写法——带类型
  标注的声明、模板字符串的值——被**无声跳过**；其余九张表照样解析，门禁仍然报
  `OK`。评审在两棵含服务端不提供的方法的树上复现了这一点：模板字符串那张表报
  `OK: 38 front-end method names`，带类型标注那张表报 `OK: 36 …`，两次退出码都
  是 0。现在门禁对自己读过什么做出断言：`uva-web/src/api/` 下每个含
  `const METHOD` 的文件都必须至少产出一行，产不出就退出 1 并指名该文件；行的
  值同时接受单引号、双引号与反引号三种写法，表的声明同时接受带类型标注的写法。
- **契约清单那一半同样**：清单的行由 `response` / `request` / `generic_response`
  三个宏写成，换一个宏写的一行会被无声跳过。现在清单里的每一行都必须是被抽取器
  读到的一行：行数对不上、或出现抽取器不认识的宏，退出 1 并指名
  `crates/uva-app-server/src/api/contract.rs` 与那个宏名。
- **表里读不出值的一行，同样是读不懂**：一张表里五行是字面量、第六行是计算值
  （`key: computeMethod('x')`）时，那一行被解析器无声丢弃，文件仍然产出五行，
  逐文件的覆盖检查照样通过——门禁于是对一行谁都没检查过的数据报 `OK`。现在
  解析器把这样的行标成「读不懂」（`<file>|<key>|#unreadable|<原因>`），门禁退出 1
  并指名该文件与该 key。带插值的模板字符串（`` `agents/${id}/x` ``）同样算读不
  懂：它不是字面量，拿它与服务端的方法表比对会得出「服务端不提供这个方法」这种
  **错误诊断**——被检查的是一行没人检查过的数据。
- **解析器改成按表读，不再按行猜**：上一条修的是「值读不出来」的行，**key**
  读不出来的行仍然被无声丢弃——`[computeKey()]: 'agents/detail'` 不匹配任何 key
  的写法，整行连同它的值一起消失，文件照样产出它其余的行，逐文件的覆盖检查照样
  通过，门禁对一行谁都没检查过的数据报 `OK`（退出码 0）。现在解析器先把表的表体
  （`const METHOD … = {` 那一行到表字面量的闭括号之间的每一行）取出来，再从表体
  里读它的成员：于是**每个成员都产出一条记录**——读得懂的，或者读不懂的——没有
  任何成员会被无声丢掉。
- **两种新形状各自有名字**：计算 key 报「key 是计算出来的，不是标识符」，取值不
  在 key 那一行（`key:` 换行 `'value',`）报「取值不在 key 那一行」。两者的报告形状
  与既有的一样（`<file>|<key>|#unreadable|<原因>`），读不懂的行仍然**不与**服务端
  的方法表比对——拿一行没人检查过的数据去比对，得出的是**错误诊断**。
- **门禁有了自己的回归测试**：`scripts/check-web-methods.sh --self-test` 用仓内
  夹具钉住解析器的判定：三种字面量写法读得懂，计算 key、跨行取值、计算值、带
  插值的模板与未闭合的字面量各读不懂，且**每个夹具文件都产出一条记录**。
  `make check-web-methods` 先跑这个自测再跑门禁，所以解析器的行为由测试钉住，而
  不是靠评审的记忆。

### Fixed (the private-doc classifier counted the brackets inside strings)

- **方括号只按代码字符计数**：`check-harness-docs.sh` 的分类器按行文本数 `[` 与
  `]`，于是 `#[doc = "the [start, end) range"]` 里的那个方括号让属性看起来没有
  闭合，它后面的声明被当成「属性续行」吞掉，**公有项**的注释因此被记到私有
  预算头上——ticket 22 要杀的就是这种错记。现在计数走 `check-frozen-api.sh`
  已经有的那种逐字符扫描：字符串字面量、字符字面量与行注释里的方括号不算括号。
  （本仓库目前没有 `#[doc = …]`，所以这是潜伏的；错记的方向是多记，门禁会红，
  但红的是公有项的注释被记到了私有预算上。）
- **分类器有了自己的回归测试**：`check-harness-docs.sh --self-test` 用仓内夹具
  钉住分类器的判定——多行 `#[derive(…)]` 后的公有项记 0 分、同样属性后的私有项
  照记、一个普通私有项照记、字符串里的方括号不再错记。`make check-harness-docs`
  先跑这个自测再跑门禁，所以分类器的行为由测试钉住，而不是靠评审的记忆
  （`check-frozen-api.sh --self-test` 是同一个约定）。
- **花括号也只按代码字符计数**：方括号计数是懂字符串的，花括号计数却按行文本数
  `{` 与 `}`。而 `pub trait` 体块的豁免正是靠花括号深度算的，于是文档注释里出现
  一个 `{`（`#[doc = "…{…"]`）就会移动深度，文件结束时深度对不上，门禁报出一句
  莫名其妙的「brace depth ended at N」；反方向的花括号还能把一处真的不平衡藏起
  来。现在一行只扫一遍：同一次逐字符扫描同时给出开括号数、闭括号数与方括号余
  额，字符串字面量、字符字面量与行注释里的花括号都不算数。`o` 与 `c` 仍是两个
  独立的计数而不是净额——`o > 0` 意为「这一行开了花括号」，`pub trait Foo {}`
  那个空体用例靠的正是它。
- **自测多了花括号这一档**：`--self-test` 现在钉住三种新形状——文档字符串里的
  `{`、`cfg` 特性名里的 `{`、行注释里的 `{`——修复前自测失败（三个用例都报
  「brace depth ended at 1」），修复后通过。
- **块注释里的花括号也不算花括号**：上一条把字符串、字符字面量与行注释排除在外，
  块注释却漏了——`/* … { … */` 里的那个 `{` 照样移动深度，于是文件结束时深度对不
  上，门禁报出「brace depth ended at N」。现在扫描器多带一个**块注释深度计数**：
  Rust 的块注释是**可嵌套**的，`/* 外层 /* 内层 */ 仍是外层 */` 里那个内层 `*/`
  不关闭外层注释，所以这里要的是计数器而不是布尔标志。行注释先判：`//` 之后的
  `/*` 不开任何注释，`/*` 里的 `//` 只是注释文本。
- **自测多了块注释这一档**：`--self-test` 现在钉住三种新形状——块注释里的 `{`、
  嵌套块注释、行注释里的 `/*`——修复前自测失败（两个用例报「brace depth ended at
  1 / 2」），修复后通过。

### Fixed (行种在模型可见表面上的角色由编译器决定)

- **一行在模型可见表面上做什么，现在只声明一次**：`SessionEvent` 的每个变体此前
  在三个地方各被说一遍——变体文档里的标记、`log_surface.rs` 里 `classify_row` 的
  白名单、以及折叠分给它的 `RowKind`。三份清单会各自漂移：新加一个变体而没人决
  定模型该不该看见它，它就是 log-only，模型看不见本该看见的东西；把一个变体加进
  白名单而没人注意到，模型就多看见了一样东西。现在这个决定由
  `SessionEvent::surface_role()` 一处声明，角色有三个：`Message`（贡献一条模型可见
  的消息）、`OpOnly`（带一个表面 op，但自己不贡献消息——今天的 `compaction` 就是
  它）、`LogOnly`（只进日志）。
- **加变体不声明角色就编译不过**：那个 match 对变体是穷尽的，所以新增变体必须先
  声明它的角色，编译器才会放行。这是这道缺口的全部要害。
- **`is_surface_eligible` 改为从角色读出**：它原先是另一份白名单，现在由角色推
  导，折叠与这个谓词不可能各说各话。折叠与 token 计量遇到一个「有表面角色、但
  这一版折叠放不下」的行时**跳过并告警**，而不是 `unreachable!` 崩掉。
- **文档标记统一成同样的话**：每个变体的首行文档现在用同一组措辞说出自己的角
  色（`surface message` / `surface op only` / `log-only`）。角色住在方法里，标记是
  它人读得懂的那一半；一个测试把两者连同 `classify_row` 对同类原始行的处理钉在
  一起，三者对不上就红。
- **模型看到的表面一个字都没变**：这道缺口只是把既有的决定写明，并让编译器守住
  它。

### Fixed (折叠放不下的行不再被无声吞掉)

- **折叠给的行种现在对变体穷尽**：`surface::fold::row_view` 此前用 `_ => return None`
  收尾，于是它对 `SessionEvent` **不穷尽**——新加一个角色是 `Message` 的变体照样编
  译得过、角色检查也过得了，然后被折叠**无声吞掉**：那一行永远到不了模型面前。
  上一轮把「跳过并告警」留作运行时行为（不崩比崩好），但那不该是**编程错误**被
  抓住的地方。现在那个 match 列出了每一个变体：带行种的四个变体给出各自的
  `RowKind`，其余二十八个 log-only 变体给出「没有行种」。新增变体不写清楚它落在
  哪一边就**编译不过**。
- **角色与行种被一个测试钉在一起**：编译器的穷尽只保证「新变体必须表态」，不保证
  「表态摆对了地方」——把一个 `Message` 角色的变体放进「没有行种」那一组，照样编
  译得过，只是那一行会被吞掉。交叉检查的测试因此多断言一条规则：**有表面角色的
  变体（`Message` 与 `OpOnly`）必须带行种，log-only 的变体必须不带**。规则是
  从代码里读出来的，不是猜的：折叠与 token 计量都只对表面合格的行调用 `row_view`，
  而表面合格正是「角色不是 `LogOnly`」。
- **模型看到的表面一个字都没变**：同一份日志折叠前后逐字节相同，每个变体的行种
  决定也逐行相同。

### Fixed (载荷与表面操作的 match 兑现了它的文档)

- **载荷与表面操作现在由一个穷尽的 match 决定**：`SessionEvent::surface_payload` 与
  `SessionEvent::surface_op` 此前各自以 `_ => None` 收尾，而 `surface_payload` 的
  文档却说「match 是穷尽的」——**文档承诺了代码没给的保证**。新加一个角色是
  `Message` 的变体照样编译得过、角色检查也过得了，然后被折叠在拿到行种之前就
  **无声吞掉**（只留一条告警）。现在一个 match 决定两样东西：新增变体不写清楚它
  带不带载荷、带不带操作，就**编译不过**。
- **行的种类被钉住，不只是「有没有」**：交叉检查的测试此前只断言「表面合格的变体
  带行种、log-only 的不带」，没有钉住**哪一种**：把 `UserMessage` 映射成
  `RowKind::Assistant` 照样编译得过、每一条断言都过得了——而那会改变模型看到的
  顺序（用户回合与助手回合的推迟规则不同）。夹具表现在为每个变体写出它必须折
  叠成的行种，测试断言折叠给的行种与之相同。
- **模型看到的表面一个字都没变**：同一份日志折叠前后逐字节相同。

### Fixed (线上词汇表最后一份手写清单钉在了枚举上)

- **`KNOWN_SESSION_EVENT_TYPES` 与枚举比集合，不再数数**：这份 const 是线上词汇表
  里最后一份没有任何东西钉住的手写清单——往 `SessionEvent` 加一个变体、补齐
  `from_value`，却忘了这份 const，此前能编译、能过全部测试：const 自己的测试只
  断言条目数（`seen.len() == 32`），而枚举长大时那个数字不会动。交叉检查新增
  一条：const 的内容必须**等于**枚举各变体在 `#[serde(rename = "…")]` 里声明的
  线上标签（与 `type_tag` 由既有往返钉在一起）的集合——缺一个、多一个都红，
  且消息点名标签。手维护的计数随之删除。
- **`events` 测试里那份 28 个 log-only 标签的手写清单同病**：新变体不入清单就
  悄悄少检查一行，谁也不报。该测试改为遍历 const、按变体声明的角色筛出
  log-only 的标签——清单不再手写，枚举长大时它自动跟着长。
- **探针证明**（临时变体，已移除）：探针变体补齐编译器要求的一切、唯独不动
  const → 新断言以 `probe_row` 点名报错；const 补上而 `empty_event_of` 仍缺
  → 既有往返测试接住；const 删掉一个既有标签或塞进一个编造的 → 新断言分别
  以该标签点名报错。模型看到的表面一个字都没变：同一份日志（旧方言、类型化、
  压缩、未盖戳四种形态）改动前后折叠结果逐字节相同。

### Fixed (服务端回退到宽松折叠时，客户端看不见)

- **被拒绝的折叠写进会话日志**：严格折叠拒绝这份日志时（压缩行引用的行与它遮住
  的区间对不上），`uva-app-server` 回退到宽松折叠，让模型看到日志真正支持的那段
  历史。这个回退原先只有一行 `tracing::error!`——**客户端看不见**：会话日志是客
  户端看会话的那份记录，而它有现成的载体：`SessionEvent::Warning`（harness 已经
  在生产里写它）。现在回退时追加**一行** `warning` 行，`data` 带稳定的 `kind`
  （`strict_fold_refused`）与 `message`。
- **每个会话一行，不是每轮一行**：拒绝是**日志**的属性，不是这一轮的属性；行已经
  在手（就是日志自己的行），所以追加之前先看有没有同 `kind` 的告警行。
- **告警行不改变折叠出来的表面**：`warning` 不在 `classify_row` 的白名单里，映
  射成 `LogRow::Ignored`，所以折叠前后两个表面相等（有测试钉住）。
- **缓存的序号不撒谎**：折叠缓存以日志的最后一个序号为键，追加一行会把这个序号
  挪走。现在追加之后**再读一次日志**再折叠，缓存键取的是**这次读取之前**的序
  号——它覆盖这次折叠，也覆盖刚追加的告警行；读取期间落地的行会让缓存失效，而
  不是被下一轮悄悄丢掉。

### Fixed (the private-doc budget gate charges a run to the wrong item)

- **`make check-harness-docs` 在本仓库从未通过过**：基线是 0，而门禁把
  `crates/uva-harness/src/agent/approval.rs` 里的 6 行 `///` 记在了私有项上。
  那 6 行其实在 `pub enum ResumeStatus` 上——门禁的分类器跳过了 `#[derive(`
  那一行，却没跳过这条属性**剩下的续行**，于是整段注释被记在了
  `Debug, Clone, Copy, …` 头上，而按门禁自己的规则，不以 `pub` 开头的名字
  就是私有项。门禁的头部写明它会「跳过属性行找到注释真正描述的那条声明」，
  并且 `pub` 项上的文档不在范围内；多行属性把这两条都破坏了。
  分类器现在按方括号深度跳过**整条**属性：一段注释因此记在它真正描述的声明
  头上——多行 `#[derive]` 后面的 `pub` 项不再被收费，同一属性后面的私有项仍然
  被收费。基线仍是 0，棘轮只许降不许升。
- **两处私有项的注释按门禁的尺度清理**：`wants_session_grant` 的注释复述了
  签名，删除；`merge_grants` 的约束（deployment 作用域的授权不来自日志行，
  所以只有 `scope: session` 的行才被采纳）原样保留为普通 `//` 注释——门禁只数
  `///`，普通注释不在范围内，这条知识因此既没有消失，也不再占预算。

### Changed (the controller holds the typed log seam)

- **`SessionController::spawn` / `spawn_with_lease` 收 `Arc<dyn SessionLog>`**（刀 A ·
  Task 6 收尾）：控制器不再看得见具体写者——之前它是「只有一个句柄」，现在是「那个
  句柄就是类型化接缝」，sink 与 writer 配错在类型上不可表达。控制器里的裸 `Value`
  行入口 `ControllerInner::append_durable_row` 删除：停放提示词行、`park_consumed`
  行、反馈行与关闭标记全部走 `append_durable_event`。`session/controller/run_log.rs`
  的 `RunLog`（三个方法、两个纯转发）随裸入口一起删除，回合直接握
  `Arc<dyn SessionLog>`；它的两条账本测试搬进 `tests/writer.rs`，覆盖不变。
- **`SessionLog` 长出 `close(reason)` / `set_ledger(ledger)` / `seed_ledger(rows)`**：
  控制器原本经 `LogWriter` 的固有方法关日志、挂运行账本、用现有历史播种账本；搬到
  `dyn` 之后这三件都是回合需要的能力，必须在接缝上，否则控制器只能再握一个具体
  写者。`LogWriter` 是唯一实现，三个方法都委托它的固有实现；行格式逐字节不变。
- **写者入口一路放宽到接缝**：`uva-harness` 的 `RunConfig::log_writer` 与
  `ReActAgent::with_log_writer`、`uva-runtime` 的 agent-loop 默认值、
  `uva-app-server` 的 `AgentRunConfig::log_writer` 都从 `Arc<LogWriter>` 改成
  `Arc<dyn SessionLog>`。行为不变，只是调用点不再能命名具体类型。

### Changed (停放提示词与反馈行的类型化归属)

- **最后两处裸行写入收进类型化接缝**（刀 A · Task 2 收尾）：回合自己的提示词行
  （`session/controller/run_task.rs`）改写 `SessionEvent::UserMessage`——`data`
  是 provider `Message`，`surface_op` 与附件 ref 是变体字段；反馈行
  （`session/controller/dispatch.rs`）写新的 `SessionEvent::Feedback`。两处都
  经 `append_event` / 新增的 `ControllerInner::append_durable_event`，调用点不
  再手写任何 `"type"` 行。`RunLog::append` 这个不再有生产调用方的裸入口删掉。
  `is_consumption_marker` 与 `repair::parked_prompts` 的判定点没动：消费仍只
  认 `park_consumed` 的 `park_id`，旧日志的 `UserInput` 兼容分支保留——普通
  用户轮次的 `user_message` 行既不消费停放的提示词，也不被当成读取窗口的消费
  前沿（各有测试钉住）。
- **`UserMessage` 带上 `attachments: Vec<AttachmentRefRow>`**：回合的附件引用
  从 `data` 里的自定义键变成行上的类型化字段，`#[serde(default,
  skip_serializing_if = "Vec::is_empty")]`，老行照旧反序列化。折叠行为逐字节
  不变（打字化行的 `Message::user(text)` 与旧 `UserInput` 行折出同一表面行）。
- **`SessionEvent::Feedback` 是新的第 32 个类型化标签**（log-only，不进模型
  表面）：`KNOWN_SESSION_EVENT_TYPES` / `from_value` / `type_tag` /
  `empty_event_of` 与 `repair::event_ts` 同步；「一标签一变体」的钉住测试从
  31 改到 32。前端 `session-to-messages.ts` 连带认识 `user_message` 行（文本
  取自 provider message，附件 chip 取自 `row.attachments`），regenerate 的
  replace 语义对两种方言都生效。

### Changed (the two profile read models have one way in)

- **`AppState::current_profile()` 从公开面退场**（刀 B · Task 2，见
  `docs/design/2026-11-13-appstate-assembly-plan.md`）：它生产零调用点，20 处
  全在 `src/state/tests/`，名字却像个产品接口。现在它是 `Assembly::raw()` 的
  测试别名（`#[cfg(test)] pub(crate)`），读的仍是**原始** profile——调用方改写
  后重新解析的底稿；换成生效的那份（`Assembly::current()`）会把被 `enabled` /
  检查过滤掉的行永久丢掉，`judge_flip_reads_as_a_presence_change` 会立刻红。
  产品面只剩两条口子：`Assembly::current()`（生效集，HMR diff 的「上一次」）
  与 `Assembly::rows()`（原始 profile 的读模型，设置页用）。
- **`assembly` 字段只在子状态内部被读**：`mod.rs` 与 `profile_apply.rs` 原先
  的两处直读已随 `Assembly` 收口——`apply` 的 diff 前态与写回都在
  `assembly.rs` 内；`grep -rn "state\.assembly" crates/uva-app-server/src`
  只剩对子状态方法的调用，没有对两份 `Profile` 的字段直读。
- **回归测试落在两份读模型的接缝上**：`enabled_false_unmounts_the_row_and_keeps_it_in_the_profile`
  追加一条断言，钉住「重新解析的底稿是原始声明」——先把它改成读生效集会红
  （另一条 `judge_flip_reads_as_a_presence_change` 同时红），改回原始 profile
  后绿。

### Added (the frontend's wire types are generated, not written)

- **`uva-web/src/api/types.ts` 由 Rust 侧生成**（刀 C · Task 6）：
  `crates/uva-app-server/src/api/contract.rs` 是一份**手写清单**——每个浏览器
  读得到的具名类型一行，写明 RPC 方法、TypeScript 名字、以及它由哪个 Rust
  类型派生；`examples/emit-contract.rs` 把这份清单打印成一份 JSON Schema
  draft-07，`uva-web/scripts/generate-types.mjs` 用
  `json-schema-to-typescript` 编译成 TypeScript。重新生成一条命令：
  `npm run generate:types`（或 `make generate-web-types`）。产物**提交入库**，
  且**逐字节可复现**——清单键排序、无时间戳、无文件系统路径，所以门禁可以
  重新生成再 diff（`make check-web-contract` 即为此）。
- **清单覆盖浏览器读到的具名响应类型（52 行），缺口是显式的**：`models/list`
  与 `ui/schema` 的七个类型（`ModelsResponse` / `ModelEntry` /
  `UiSchemaResponse` / `UiSlot` / `UiComponent` / `UiSchema` / `Cardinality`）
  也进了清单，`catalog.ts` 不再手写它们。剩下的手写类型不是 Rust 响应类型的
  镜像：RPC 请求参数信封（`ApprovalRequestParams` 等）与流式帧形状
  （`chat-stream.ts` / `follow-frames.ts`）描述的是传输协议本身，没有对应的
  序列化响应类型可派生，因此不在清单里（见 `contract.rs` 的
  「What the catalog cannot say」）。
  唯一的泛型是列表信封 `List<T>`——JSON Schema 没有类型参数，于是 Rust 侧
  按 `List<serde_json::Value>` 描述它并标记 `generic: "T"`，生成器单独编译
  这一行、断言语义后再补上类型参数。
- **`serde_json::Value` 一律是 `unknown`**：schemars 会把字段的文档注释并进
  schema，于是一个 `Value` 字段到生成器手里是 `{ description: … }` 而不是
  `true`，编译器就把它当对象类型、发出 `{ [k: string]: unknown }`——而契约是
  「可以是标量、数组或对象」。生成器把**没有任何约束关键字**的子 schema
  归一成 `true`，于是 `config: unknown`、`arg?: unknown`、
  `input_schema: unknown`。没有后处理改写文本。
- **`profile/changed` 也进了生成集**：广播的载荷从此是具名类型
  `ProfileChangedEvent`（`api/v1/profile.rs`），`audit_and_notify` 序列化它而
  不再是 `json!` 字面量，于是前端订阅用的类型与服务器发出的类型同源。
- **沿着生成结果修掉了四处真实漂移**：`AgentDetail` 线上的字段是
  `model_hint`（不是 `modelHint`）；`JobKind::Interval` 带的是
  `interval: {secs, nanos}`（不是 `interval_secs`）；`POST /evals` 收的是裸
  `EvalSuite`（不是 `{ suite }` 包装）；`surface_op` 是 untagged 联合——追行
  的形态是字符串 `"append"`，替换形态才是 `{start, end, source_event_seqs}`。
  顺带消失的 TS-only 字段：列表信封的 `has_more`（Rust 侧从来没有）、
  `TaskDelegation.output`、`ScheduleTick.next_deadline`、`EvalTestCase.metrics`。
- **`JsonSchema` 派生落在类型定义处**：`uva-harness`（`ApprovalRequest` /
  `ResumeToken` / `ResumeStatus` / `ResumeDecision`）、`uva-session`
  （`SurfaceOp`）、`uva-eval`（`EvalSuite` / `TestCase` / `TestResult` /
  `EvalReport`）、`uva-scheduler`（`Job` / `JobKind` / `JobStatus` / `JobId`）、
  `uva-tool`（`Risk` / `RenderKind` / `ToolOutputDefinition`）与 app-server 的
  路由类型。这些都是**形状不变、只多一个 trait 实现**——7 个冻结 crate 的
  crate 根公开名一个没动（`make check-frozen-api` 423 个名字不变），
  `SurfaceOp` 的派生属于同类。工作区 `schemars` 打开 `chrono04`，让
  `DateTime<Utc>` 有 `JsonSchema`（线上是 RFC 3339 字符串）。
- **两个响应字段仍是 untyped JSON**：`SessionDetail.history` / `artifacts` 与
  `FollowSnapshot.rows` 是 `Vec<Value>`（会话日志是开放的行词汇）。它们的形状
  由新模块 `api/v1/session_view.rs` 具名，并用 `#[schemars(with = …)]` 挂到
  那三个字段上——视图不是凭空发明：行信封就是 `uva_session` 的 writer 盖的
  `seq` / `ts` 加上 `SessionEvent` 的 `type` / `data` / `source` /
  `is_error` / `surface_op`。
- **漂移由 `make check-web-contract` 把住**：`scripts/check-web-contract.sh` 用
  同一个生成器把契约写到 `$HOME` 下的临时文件（`/tmp` 是 tmpfs，Rust 侧编译
  会填满它），与入库的 `uva-web/src/api/types.ts` 逐字节 diff；不一致时退出 1，
  指名该文件并打印差异。`generate-types.mjs` 为此支持 `--out`，门禁期间不碰
  工作区。`make ci` 现在同时跑 `check-web` 与 `check-web-contract`。
- **方法名漂移由 `make check-web-methods` 把住**：`uva-web/src/api/` 下各域
  客户端各有一张 `METHOD` 常量表，方法名是写死的字符串——写错一个字符照样
  编译通过（`tsc` 只认类型，`check-web-contract` 只比对 `types.ts`），要到
  运行时才以 JSON-RPC 的 "method not found" 暴露，此前唯一的防线是人工的
  MCP 冒烟。`scripts/check-web-methods.sh` 把每张表里的名字与服务端**真正
  提供**的方法逐个比对：路由的 `router.register("…")` 条目与通知常量。
  前端调了服务端没有的方法 → 红，指名文件、表键与方法名；服务端有而前端
  不调的**不算错**——服务端 64 个请求方法前端只用其中一部分，这是设计。
  比对的是服务端的方法表而不是契约清单：清单的 52 行命名 **31** 个唯一方法值
  （29 个具体值 + 2 个通配 `*/list` / `schedules/*`），而前端调用的 38 个方法里
  有 **15** 个在清单里没有字面条目（清单是类型目录，不是方法目录）——这 15 个里
  有 3 个落在通配族内（`agents/list`、`evals/list` 归 `*/list`，
  `schedules/delete` 归 `schedules/*`），其余 12 个不在任何一行里；清单里那些
  具体的 `method` 值也一并比对（通配的 `*/list` / `schedules/*` 不是一个方法名，
  跳过），生成出来的 `types.ts` 不能宣称一个已经不存在的接口。

## [Unreleased]

### Fixed (two `loadMore()` calls in one tick appended the same page twice)

- **`useCursorList` 的同 tick 双追加竞态**：`loadMore()` 的守卫读的是
  `loading` / `nextCursor` 的影子 ref，而它们只在重渲染时刷新——同一个 tick
  里连调两次 `loadMore()`，两次都看到「上一次渲染时」的状态。第二个请求照样
  发了出去（同一页取两次）；若它落在「第一页已经落地、重渲染还没跑」的窗口
  里，还会用旧 cursor 把刚取回的那一页**再追加一次**，列表里出现重复项。修法
  是把分页状态变成 hook **同步持有**的值——`uva-web/src/lib/cursor-list.ts`
  新增 `CursorPaging` 与两个迁移：`beginCursorRequest` 在 `fetchPage` 的第一个
  `await` 之前 claim 在飞标记，`settleCursorLoad` 在响应落地的那一刻写下下一页
  的 cursor，两者都不等重渲染。`UseCursorListResult` 与 `useCursorList`
  的公开形状不变。
- **回归测试落在 hook 自己的接缝上**（`uva-web/tests/use-cursor-list.test.ts`）：
  node 环境没有渲染器，`tests/react-hook-runtime.ts` 因此提供一个最小的 hook
  运行时——四个原语，外加两条 React 语义：「状态更新不同步重渲染」与「ref 由
  组件体自己写」——用它驱动真正的 `useCursorList`，调用方传入的
  `FetchCursorPage` 型取页函数换成可脚本化的假件。两条断言在修复前都红
  （同一页被请求两次；第二页之后再要一次 `c2` 并把
  `a2` 追加两遍），修复后绿。两条时序先在真 React 上用一次性探针复现过，请求
  序列与被重复的页完全一致。这把 ticket 08 留下的「hook 本身不进覆盖清单」
  补上了——纯核接缝盖不到的 React 时序，现在由这个运行时接住。

### Added (the frontend has its first automated test surface)

- **前端装上了第一条自动化的「接口即测试面」防线**（刀 C · Task 1，见
  `docs/design/2026-11-13-frontend-contract-plan.md` 与
  `docs/adr/0001-frontend-test-policy.md`）：`uva-web` 有了 **vitest@4**（node
  环境，**不装 jsdom**），`npm test` 跑 `vitest run`，测试文件只落在
  `uva-web/tests/`——**不进 `src/`**（`tsconfig.json` 的 `include` 只有 `src`
  且没有 `@types/node`，`src/` 下的 `*.test.ts` 会同时炸 `tsc --noEmit` 与 CI
  的 Node 20）。`make check-web` 与 CI 的 web job 自此多跑一步单元测试。
- **版本钉在 4.x 是 CI 的硬约束**：CI 的 web job 固定 **Node 20**
  （`.github/workflows/rust-quality.yml`），vitest@5 的 `engines` 是
  `^22.12.0 || ^24 || >=26`——装不上；vitest@4 的 `^20 || ^22 || >=24` 才过。
  jsdom@31 同理（要 `^22.22` 起），而本切面只测纯函数，vitest 的默认环境
  就是 `node`，根本不需要它。
- **前端纯函数测试的第一批落地**（刀 C · Task 2，见
  `docs/design/2026-11-13-frontend-contract-plan.md`）：`uva-web/tests/` 下
  两个文件把规则最密的两块纯函数钉住——`src/lib/session-to-messages.ts` 的
  8 个导出（日志行 → 消息的投影：两种行方言、`tool_use`/`tool_result` 的
  配对、surface 的 replace op）与 `src/settings/receipt.ts` 的 2 个导出
  （四态回执，四种状态不许塌缩成「已保存」）。期望值来自已知的行形状与
  手算的例子，不从被测代码反推；不碰 DOM、不碰网络、不碰 React，碰
  `localStorage` 的 `seedChatFromSession` 不是纯函数，不在这一批。
  `vitest.config.ts` 的 `passWithNoTests` 自此是 `false`——套件找不到自己
  的测试时必须红，不能再报绿。

### Added (the cursor pagination rules are pure, not hidden in the hook)

- **`useCursorList` 的两段纯逻辑抽成了纯函数**（刀 C · Task 3，见
  `docs/design/2026-11-13-frontend-contract-plan.md`）：hook 是 React 组件，
  node 环境里没有渲染器，测不了；但「一次响应怎么并进 `items`」与
  「`loadMore()` 什么时候是 no-op」这两段规则值得钉住。两段搬进
  `uva-web/src/lib/cursor-list.ts`（无 React 依赖）——
  `mergeCursorPage(current, incoming, mode)` 折叠一页（`replace` 丢弃旧
  items，`append` 把新页接在已加载的 items 之后），`nextCursorToLoad(loading,
  nextCursor)` 给出该请求的 cursor，没有就 `null`（请求在飞，或列表没有下一
  页）。hook 改为调用这两个函数，自己不再算；`UseCursorListOptions` /
  `UseCursorListResult` / `useCursorList` 的公开形状不变。
- **两段规则各有断言**（`uva-web/tests/cursor-list.test.ts`）：合并的顺序、
  `replace` 丢弃旧 items、空页不丢 items、不改入参；守卫在请求在飞、没有下
  一页、cursor 为空串时都返回 `null`。期望值来自手算的例子，不从被测代码
  反推；hook 本身仍不进覆盖清单（它要渲染器）。

### Changed (the assembly is its own sub-state, not eight fields on `AppState`)

- **装配这一摊收成了一个 `Assembly`**（刀 B · Task 1，见
  `docs/design/2026-11-13-appstate-assembly-plan.md`）：`kernel` /
  `boot_services` / `mount` / `profile` / `assembly` / `profile_store` /
  `profile_write` / `ui_tree` 八个字段搬进
  `crates/uva-app-server/src/state/app_state/assembly.rs`，`AppState` 只剩一个
  `pub assembly: Assembly`——结构体字段 40 → 33，公开字段 39 → 32。接口是四个
  口子：`current()`（生效的那份，HMR diff 的「上一次」）、`rows()`（原始
  profile 的读模型，设置页用）、`service::<T>(key)`（从 kernel 取挂载好的
  服务）、`apply(..)`（一次装配变更：重挂 diff 的 id、更新两份读模型、更新
  ui_tree）。`apply` 留在 crate 内：它需要的 workspace root / clock / MCP
  运行时由调用方借出（`AssemblyContext`），子状态不复制这三个句柄。
- **`kernel` 不再是 `AppState` 的公开字段**：15 处直读改走
  `Assembly::service::<T>(key)`（`attachments` / `search` / `memory` /
  `agent_loop` / `live_catalogue` / `live_default_provider` / 调度器的 leader
  tick 与测试），`build_prompt_context` 改为收挂载好的 `skills` 服务而不是
  `&Context`——kernel 句柄不再漏出装配。
- **行为逐条不变**：`reload_profile` / `apply_resolved` 的 diff（只重挂 diff 的
  id）、`hmr = false` 时的「需要重启」、`enabled = false` 把行从装配里滤掉而
  原始 profile 仍留着（`state/tests/reload.rs`）——`cargo test -p
  uva-app-server` 全绿。`current_profile()` 仍是**原始** profile：它是调用方
  改写后重新解析的底稿，换成生效的那份会把被过滤的行丢掉。

### Changed (the frame validators are a module of their own)

- **四个 follow frame validator 独立成模块**（刀 C · Task 4，见
  `docs/design/2026-11-13-frontend-contract-plan.md`）：`readRowFrame` /
  `readInterrupts` / `readBaseline` / `readLiveTurn` 从
  `uva-web/src/api/chat-stream.ts` 挪到新模块 `uva-web/src/api/follow-frames.ts`
  并导出——它们原先是**模块私有**的，**根本 import 不到**，那约 150 行防御性
  解析因此无从测起。函数体**逐字照搬**（只多一个 `export`），语义不变：形状
  不对的帧仍然答 `null`，调用方静默丢帧，服务端发来的坏帧仍然打不崩页面。
  `chat-stream.ts` 的公开面**不变**——`FollowEvent` 随 validator 一起搬走，
  再从这里转出，调用方照旧从这一个模块 import。
- **四个 validator 各有断言**（`uva-web/tests/follow-frames.test.ts`）：合法帧、
  缺字段帧、错类型帧各覆盖一遍，外加每条守卫的边界——空串的 `turn_id`、非对象
  的 `row` / `header` / `live` / `context`、不在清单里的 `risk` 与 `status`。
  期望值是手写的线上形状，不是从被测模块反推出来的。

### Changed (the transcript owns its provenance)

- **`RunState` 里其实是同一个对象的四个字段收进了 `Transcript` 类型**（刀 D ·
  Task 1，见 `docs/design/2026-11-13-harness-loop-transcript-plan.md`）：
  `messages` / `surface_seqs` / `pending_tool_calls` / `deferred_user` 搬进
  `crates/uva-harness/src/agent/re_act/transcript.rs`，**字段私有**——「消息与
  其序号下标并行」从此是类型的性质，而不是五处调用点各自记住的约定。接口是
  `append_message` / `open_call` / `answer_call` / `deliver_user` /
  `release_deferred` / `splice_compaction`，加 `from_surface` / `empty` 构造
  与 `messages()` / `surface_seqs()` / `is_citable()` 读出。
- **旧别名与字段名一并清掉**：`state.rs` 的 `type Transcript =
  Arc<Vec<Message>>` 消失，请求侧（每步投影、用完即弃）的消息列表另起名
  `RequestMessages`，`RunState.messages` 改为 `RunState.transcript`。压缩的
  replace 从两处并列的 `splice` 收进 `splice_compaction(range, replacement,
  seq)`——span 与它的来源一起动。
- **种子对不齐时压缩跳过，与旧长度检查同一结果**：`derive_history` 合并旧的
  一行一消息方言，而折叠保持行分开，两份长度可能不一致——`from_projection`
  检测到就保留消息、序号归零并记 `is_citable() = false`，压缩对这种表面直接
  跳过（旧代码里它走到 `plan_compaction` 的 `MismatchedSurface` 才被拒）。
  不变量在自己那一层测：`uva-harness` 新增 8 个单元测试，逐条 append 路径后
  断言表面与来源等长、窗口开着时被挡的用户轮次不落表面。

### Changed (the iteration is one module)

- **`iteration_body` 的七步脚本与它守的持久化边界收进 `step` 模块**（刀 D ·
  Task 3，见 `docs/design/2026-11-13-harness-loop-transcript-plan.md`）：
  `crates/uva-harness/src/agent/re_act/loop/step/` 的接口是 `run`——跑一次迭代、
  给出结果。`loop/mod.rs` 的自由函数从 16 个降到 6 个；`prepare` / `sample` /
  `tools` 搬进 `step/` 成为它的私有实现，`suspend` 的边界并进 `close()`，
  `provider_id` 随它唯一的使用者进 `prepare`。
- **`step_end` 只在一处写**：普通一步的 `step_end(action)` 与挂起一步的
  `step_end_suspended` 都由 `step::close` 决定，`Step` 变体、它携带的标签和
  写出的行从此在同一个模块里——`state.rs` 里「事件与关闭决定不会漂移」的
  说法是代码事实，`Step` 也随之搬进 `step/mod.rs`。
- **`turn_open` 不再跨文件**：变成 `step::TurnState`（字段私有）的三个具名
  操作——`open`（新运行开场，resume 复用它重开被续的那一轮）、
  `closed_by_suspension`、`close`（finalize 的收尾）。`drive` / `resume` /
  `close` / `finalize` 只调方法，布尔量本身只在这一个文件里读写。
- **行为逐条不变**：日志行格式与顺序（含挂起时的 asked →
  turn_suspended → step_end_suspended → turn_end_suspended → flush → 事件）
  原样保留；`tests/lifecycle/step_pairing.rs` 与 `one_row_per_turn.rs` 一行
  未改仍然绿。

### Changed (the durable row's vocabulary is typed, not hand-written)

- **`SessionEvent` 的结构变体带上了类型化字段**（刀 A · Task 1，见
  `docs/design/2026-11-13-session-log-typed-seam-plan.md`）：`Turn` / `Step`
  / `ToolCall` / `ToolResult` / `SubagentEnter` / `SubagentExit` /
  `RequestHeader` 各自的载荷键（`kind` / `turn` / `step` / `call_id` /
  `name` / `tool_call_id` / `child_session_id` / `parent_session_id` /
  `agent_name` / `depth` / `status` / `provider` / `model`）现在是变体上的
  可选字段，全部 `#[serde(default, skip_serializing_if)]`——老日志反序列化
  不受影响，新行两处同写（类型化字段 + `data` 里的同名键），因为 TUI 折
  叠的是裸行、从不反序列化。
- **`from_value` 是两套视图的唯一汇合点**：一行只带 `data` 的老行在解析时
  由 `normalize` 把载荷键回填进类型化字段，于是 20 处原本各按各的字符串
  键读载荷的生产读取点改为调语义访问器——`boundary_kind()` /
  `boundary_turn()` / `tool_call_id()` / `delegation_child()` /
  `delegation_parent()` / `delegation_agent()` / `delegation_depth()` /
  `delegation_status()` / `request_provider()` / `request_model()`
  （`crates/uva-session/src/events/mod.rs`）。兼容回退只在 `uva-session`
  一处，不再散在三个 crate 的 20 个调用点上。
- **生产代码里 4 处裸 `json!` 行写入清掉 3 处**：Anthropic 兼容端点的种子
  历史（`routes/messages/mod.rs`）与子代理派发提示词的两条路径
  （`session/subagent_router.rs`）改走 `user_message` 构造器 +
  `append_event` / `append_event_nowait`；`classify_user_input` 与
  `classify_typed_message` 折出的是同一个 `Message::user(text)`，表面行为
  逐字节不变。当时剩余 1 处是停放进度的 `UserInput`
  （`session/controller/run_task.rs`）——它的 `attachments` 与 `surface_op`
  让迁移与 `repair::parked_prompts` 的读取端缠在一起；见下面
  「停放提示词与反馈行的类型化归属」一节，那一处与反馈行现在都已收口。

- **两个日志接缝收窄**（刀 A · Task 4/5）：`SessionSink::read_from` 从必实现降为
  默认实现（默认体是整读后跳过前缀——JSONL 后端本来就是整文件读，I/O 逐字节
  相同）；`SessionSink::read_from_index` 整体删除（全仓零重写、零调用）。`SessionLog`
  的 `append_request_header` 保留：`LogWriter` 对它有信封记忆（`initial` / `same` /
  `change`），且 harness 是它的真实调用方——计划里「收窄到 7 个方法」这一条写
  错了，8 个方法各有其主。
- **`SessionController::spawn` 只收一个句柄**（刀 A · Task 6）：`session_store` 与
  `writer` 两个参数合并为 `writer: Arc<LogWriter>`（7 → 6 参），
  `ControllerInner` 不再持有 `session_store`——关闭改走新增的
  `LogWriter::close`（先落盘再委托它自有的 sink），历史读取改走
  `LogWriter::read`，缓存失效检测改用 `LogWriter::last_seq`（写者计数器连未落盘
  的行也看得见，比 sink 的快照更灵敏）。**sink 与 writer 配错从此不可表达**：
  配对是这个值自身的事实，不再是调用点的两个参数。17 处测试调用点同步。
- **`docs/api-frozen.md` 的名字表按基线重写**（刀 A · Task 7）：表格从
  43/41/64/92/121/43/48（共 452）改为基线实际的 19/41/64/93/126/36/44（共
  423），各 crate 的名单逐一对齐 `scripts/frozen-api/baseline`，并在页首注明
  「名单与基线不一致时以基线为准」。基线本身逐字未动——刀 A 没有增删任何
  crate 根名字。

### Changed (the profile seam answers with named types, not bare JSON)

- **`profile/*` 五个方法返回具名类型**（刀 C · Task 6 前半，见
  `docs/design/2026-11-13-frontend-contract-plan.md`）：`profile/list` 与
  `profile/get` 答 `ProfileRow`，`profile/set` / `profile/create` /
  `profile/delete` 答 `ProfileChange`——类型落在
  `crates/uva-app-server/src/api/v1/profile.rs`（与其它 v1 线上形状同放），
  并 derive `JsonSchema`，生成链路因此能盖到它们，前端手写的
  `ProfileRow` / `ProfileChange` / `ProfileEnableCheck` 也有了源头。
  **线上 JSON 逐字节不变**：行还是 `id` / `segment` / `runtime` / `enabled` /
  `config` / `status` / `plugin_state`，回执还是 `state` / `reasons` /
  `diff`（`diff` 一直在线上，前端的 `ProfileChange` 只是不读它）。
  读模型不再是手搓的 `json!`：`profile_rows` / `profile_row` 直接答
  `ProfileRow`，域类型（`Enabled` / `ChangeOutcome`）经一次投影上网。

### Removed (the dynamic plugin channel and the workflow domain are gone)

- **动态插件通道整条删除**（`crates/uva-plugin-runtime` 全部消失）：三个断面
  ——`protocol`（serde-only 词汇）、`host`（拉起插件进程说 JSON-RPC over stdio）、
  `wasm`（WebAssembly 组件运行时）—— 连同 facade 的 `plugin-protocol` /
  `plugin-host` / `wasm` feature、app-server 的 `process-plugins` 与 `wasm`
  profile 行、`[plugins]` 部署配置段、`plugins/list` 方法与路由、会话的
  plugin 事件通道、`uva-core` 的 bundle `code` 组件全部删除。
  **第三方工具与脚本改走 MCP server**：`mcp.json` 的 `mcpServers` 映射按
  `command` + `args` 声明，stdio 传输，server 发布的工具注册进同一个
  `ToolRegistry`（`crates/uva-app-server/src/state/app_state/boot.rs:370`）。
  MCP 不承载的角色（结果改写 / gate / authority 分级）随通道一起消失。
  顺带删掉仓里唯一那个 JIT 依赖（`wasmtime` 50）与它的门禁 `check-wasm-deps` /
  基准 `bench-wasm`。
- **`uva-workflow` crate 与 workflows 域整条删除**：声明式多智能体工作流运行时
  （`WorkflowSpec` / `WorkflowStore` / `WorkflowRuntime` / `WorkflowHost`）
  连同 app-server 的 workflows 域——RPC 方法 `workflows/list|create|get|replace|
  delete|run`、HTTP `/api/v1/workflows/*`、MCP 自工具 `workflows`、web 的
  workflows API 与「自动化」分区里的工作流块、TUI 的工作流目录项——全部消失。
  **已存在磁盘上的 `<workspace>/workflows/*.json` 不再被任何代码读取，可自行删除。**
  workspace member 从 27 个减到 **25** 个。

### Changed (docs: the front door and the agent contract, restructured)

- **`README.md` 重写为项目门面**：状态表 → 5 分钟上手 → 为什么是 Uva → 架构 →
  工作区分层 → 文档索引 → 社区与治理，读者从"这是什么"到"怎么参与"只走一条路。
  crate 清单不再在三张表里重复（改为分层表 + 指向 `docs/architecture.md`），
  逐轮"composition primitives"大表移到 [`docs/composition-primitives.md`](docs/composition-primitives.md)。
  事实校正：workspace member 按 `cargo metadata` 是 **25** 个（不是 29 / 40）；
  明确写出**尚未发布到 crates.io**（发布计划见 `RELEASE.md`）；
  SBOM / 签名 / provenance 归入路线图，不再当作现状承诺。
- **`AGENTS.md` 精简**：674 → 约 300 行。硬不变量留在正文并各自挂上门禁，
  理由下沉到 [`docs/architecture.md`](docs/architecture.md)；40 行的 Makefile
  目标表换成"`make help` 是唯一清单 + 门禁护什么"；新增 §3.9（异步与并发）
  与 §3.10（依赖与供应链）两节，对齐 `rust-dev` 的硬性要求。
- **新增文档**：`docs/architecture.md`（crate 全景 + 不变量与理由）、
  `docs/README.md`（文档树入口）、`docs/ROADMAP.md`（当前轮次与后续供应链路线）。
- **死链修复**：`MINIMAL.md`（`docs/architecture/adr/` → `docs/architecture.md`）、
  `CONTRIBUTING.md`（三个不存在的 `uva-tool-*` crate → `uva-tool-plugins` 的对应 feature）、
  `RELEASE.md`（`identity.rs` → `build_info.rs`；移除不存在的 `contract-closure.yml`）、
  `DEPLOYMENT.md`（缺失的 `nginx.conf` 改为内联 server block）、
  `docs/api-surface.md` 与 `docs/log-format.md`（相对路径修正）。全仓相对链接 0 死链。

### Changed (repo hygiene: agent instructions consolidated)

- **`CLAUDE.md` 已并入 `AGENTS.md`**：仓库根不再保留独立的 `CLAUDE.md`。
  它的"怎么做"部分（Think before coding / Simplicity first / Surgical
  changes / Goal-driven execution / Skills / Task-centric execution）逐行
  迁入 `AGENTS.md` §5（`## 5.1`–`## 5.6`），与"做什么"的 §1–§4 合流为
  单一真源；内容与 Anthropic Claude Code 上游标准保持一致，只改了标题
  层级与两处交叉引用。`CODEOWNERS` / `.dockerignore` 中的对应条目同步移除。
- **工程 skill 配置落地**：新增 `docs/agents/`（issue tracker / triage labels
  / domain docs）与 `AGENTS.md` 的 `## Agent skills` 段。issue tracker 走
  本地 markdown（`.scratch/<feature>/`，已 gitignore），不依赖 `gh`。

### Changed (CI cadence: the periodic jobs run less often)

- **依赖检测从每周一次改为每 2 个月一次**：`security-audit.yml` 的 cron 从
  `0 3 * * 1`（每周一 03:00 UTC）改为 `0 3 1 */2 *`（每 2 个月 1 日 03:00 UTC，
  即 1/3/5/7/9/11 月）；`dependabot.yml` 的三个 ecosystem（cargo /
  GitHub Actions / npm）从 `interval: "weekly"` 改为 `interval: "cron"` +
  `cronjob: "0 9 1 */2 *"`，三者同日 09:00 UTC 跑，比 security-audit 晚 6 小时。
  **`interval: "cron"` + `cronjob` 是这里唯一可用的写法**：GitHub 的具名 interval
  只到 `monthly`，`quarterly` / `semiannually` / `yearly` 仅 GitHub Enterprise
  Server ≥ 3.19 可用。
- **其余周期任务同步拉长**：`stale.yml`（每天 → 每周一 02:00 UTC）、
  `markdown-lint.yml`（每天 → 每周二 04:00 UTC）、`shellcheck.yml`（每天 →
  每周三 05:00 UTC），三者错开在不同的工作日，不再同一天挤在一起。
- **push / PR 触发不受影响**：这五个 workflow 的 `push` / `pull_request` 触发器
  一个没动——变的是 cron 巡检的周期，PR 门禁的即时性完全不变。

### Changed (breaking: the project is renamed from elysper to uva)

- **crate / 包 / 目录全部改名**：`elysper` → `uva`、`elysper-core` → `uva-core`、……
  共 27 个 crate；三个二进制改名 `uva-app-server` / `uva-tui` / `uva-mcp-server`，
  前端包目录 `elysper-web/` → `uva-web/`。
- **对外可见的标识一起改名**（breaking）：环境变量 `ELYSPER_*` → `UVA_*`；
  wire 字段 `metadata.elysper_session_id` / `metadata.elysper_agent_name` →
  `metadata.uva_session_id` / `metadata.uva_agent_name`；tracing target `elysper.*` →
  `uva.*`；运行时目录 `~/.elysper/` → `~/.uva/`；Docker 镜像与 k8s 资源名同步。
- **facade 的公开路径**：`elysper::…` → `uva::…`（facade 包名本身就是 `elysper`）。

### Changed (crate topology: the tokio adapters are renamed to what they are)

- **`uva-harness-tokio` → `uva-core-tokio`**：这个 crate 的库面只有三个零大小
  适配器（`TokioSpawner` / `TokioSleeper` / `TokioBlocking`），实现的是
  **`uva-core`** 的三个 runtime-neutral trait（`spawn::Spawner` /
  `sleeper::Sleeper` / `blocking::Blocking`）——`src/lib.rs` 一行都没 import
  `uva_harness`，lib 的依赖只有 `uva-core` + `tokio(rt,time)`（harness 只在
  dev-dependencies 里给 example 用）。名字里的 `-harness-` 是历史遗留的误导：它不是
  harness 的运行时，而是 core 那三个 seam 的 tokio 实现。改名后名字回答了"这是谁的
  适配器"这个问题。
- **crate 本身仍然独立**：core 与 harness 都不得带 tokio（`make check-harness-features`
  与 `make check-no-runtime` 断言），适配器只能住在第三个 crate——上一轮
  （29 → 27）的审阅结论同样如此，本次只改名字、不动结构。
- **facade 的公开路径一字未改**：`harness-tokio` feature 与 `uva::harness_tokio::*`
  保持原样（沿用插件族聚合与 `uva-agents` 内敛两次先例：底层动、facade 不动），
  变的只有它转发的底层 crate 名。`docs/api-frozen.md` 的非冻结清单同步。
- 引用同步：`uva-core` 三个 trait 的文档注释、`uva-session` /
  `uva-policy-plugins` 的 seam 说明、`SEAMS.md`、facade 的 feature 表与
  `check-publishable.sh` 的打包序。历史设计文档（`docs/design/`、`docs/superpowers/`）
  按 R46 先例保持原样——它们记录的是当时的轮次。

### Changed (CLI surface: every binary's command line is clap's)

- **`uva-mcp-server` 迁到 clap**：手写的 `parse_argv`（`crates/uva-mcp-server/src/lib.rs`
  的私有 `config` 模块）删除，命令行定义落到 `crates/uva-mcp-server/src/cli.rs` 的
  `Args` / `Command` / `ServerConfig` 上。**对外契约不变**：`--workspace` / `-w` 与
  `--name` / `-n` 的拼写、默认值（`$PWD` 与 `uva`）与失败语义（workspace 必须是
  已存在目录）逐字保留；`parse_argv` 这个公开函数随之删除（crate 尚未发布到
  crates.io，这是最便宜的时刻）。
- **`--help` 的退出码从 2 修正为 0**：旧的手写解析把 `--help` 当成"用法错误"返回，
  任何用退出码判断"帮助是否成功"的脚本都会误判；现在 `--help` / `--version` 是成功
  路径（exit 0），用法错误仍是 exit 2。
- **协议模式的帮助文本改走 stderr**：`uva-app-server` 的 `core` 子命令与整个
  `uva-mcp-server` 的 stdout 是 JSON-RPC 帧通道，clap 默认把 `--help` / `--version`
  打到 stdout 会被对端读成坏帧——和一行日志落在 stdout 上是同一种故障。这两个模式
  因此把帮助与版本渲染到 stderr（`core` 的 `--help` / `--version` 是自定义 flag，
  clap 的自动帮助在该子命令上关闭）；`serve` / `translate` 与 TUI 保持 stdout 的常规
  行为。
- **命令行定义统一落在各 crate 的 `src/cli.rs`**：`uva-app-server` 与 `uva-tui`
  原来把 clap 的 `Args` / `Mode` / `ServeArgs` 写在 `main.rs` 里，现在搬进库的
  `cli` 模块——`clap::CommandFactory::debug_assert()`（查互相矛盾的参数定义）因此
  第一次进入 CI，`main.rs` 也只剩薄壳。
- **新增两道门禁与契约测试**：`make check-cli-surface` 断言库代码不再手写 argv 解析
  （`env::args` / `env::args_os`），且协议模式不使用 clap 的 stdout 帮助打印器；三个
  二进制各自的 `tests/cli_surface.rs` 真实 spawn 进程，断言 stdout 为空、stderr 有
  内容、退出码符合约定。

### Changed (crate topology: non-plugin crates inlined, 29 → 27)

- **`uva-agents` 内敛进 `uva-core`**：它不是 kernel 插件（不实现 `Plugin`），
  仓内唯一的消费方是 facade——而 facade 只是转发。真正决定它该住哪的是**它与 core
  共享的同一份概念**：`uva_core::AgentDescriptor`（身份）、`uva_core::layout`
  （`Domain::Agents` 的四源发现顺序）、以及它自己的文件格式（`agent.md` +
  `config.json`）。三者拆在两个 crate 里，"一个 agent" 就要跨 crate 编辑；现在它们
  同处 `uva-core` 的 `agent_def` 模块——身份、位置、格式同源，`Domain::Skills` /
  `Domain::Tasks` 未来也能沿用同一条缝。
  - **公开路径不变**：facade 的 `agents` 模块改为转发 `uva_core::agent_def`，
    `uva::agents::AgentDefinition` / `write` / `discover` 一字未改，app-server
    源码零改动（663 测试全绿）。
  - **`uva-core` 新增 `tracing` 依赖**：发现流程要 warn 掉一个解析失败的定义。
    `tracing` 是日志门面而非运行时（无 reactor、无 worker pool），不破坏 core 的
    tokio-free 契约；`make check-no-runtime` 仍然通过。
- **删除 `uva-macros`**（578 行 proc-macro + 286 行测试 + 89 行 example）：仓内
  **零使用**——九个工具插件全部手写 `impl Tool`，连"第三方插件作者"的参考样例
  （`docs/examples/plugin-author`）也是手写。它还带来三重代价：
  1. **覆盖不全**：宏只生成 `name` / `description` / `parameters` / `call` 四个方法，
     而真实工具还要写 `output_definition` / `annotations`——省下的样板比失去的多；
  2. **隐含约定**：`execute` 方法名、doc comment 当描述、snake_case 当工具名、
     `mode` 字符串映射——正是"少约定、少隐含逻辑"要减的东西；
  3. **依赖画像**：`proc-macro2` + `quote` + `syn` 进了默认 feature 集。
  facade 的 `macros` feature 与 `uva::macros` 模块随之删除。
- **判定表（本轮审阅的结论）**：其余非插件 crate 逐一核对后**全部保留**，理由分别是——
  `core` / `kernel` / `provider` / `tool` / `harness` / `session` / `context` /
  `steering` / `search` / `telemetry` / `test-support` 是多方消费的范式基座；
  `uva-harness-tokio` 必须独立（core 与 harness 都不得带 tokio，适配器只能住
  第三个 crate）；`uva-workflow` 有 69 个测试 + 3 个 example，是多 agent 编排的
  完整领域能力；`gateway` / `tui` / `mcp-server` / `app-server` 是二进制与跨 workspace
  契约。

### Changed (crate topology: plugin families)

- **按插件维度聚合成族 crate（45 → 29 个 crate）**：同构的插件实现不再一人一个 crate，
  而是"**一个族 = 一个插件维度**，族内**一个插件 = 一个模块 + 一个 feature**"：
  - `uva-tool-plugins` —— 九个工具插件（`read` / `write` / `shell` / `todo` /
    `web` / `task` / `scheduler` / `search` / `memo`）；
  - `uva-policy-plugins` —— 七个策略插件（`cache` / `resilience` / `budget` /
    `reflection` / `cot` / `plan-execute` / `best-of-n`）；
  - `uva-plugin-runtime` —— 插件通道的三个断面（`protocol` / `host` / `wasm`）。
- **插件专属依赖逐插件 optional**：一个工具的依赖由它自己的 feature 打开
  （`web` → `reqwest`、`task` → `uva-harness`、`scheduler` → `uva-scheduler`、
  `search` → `uva-search`、`memo` → `blake3`），族 crate 的默认构建什么也不带；
  `make check-mvp-deps` 改为按 **feature 组合** 断言，并反向断言 `web` 确实是打开
  `reqwest` 的那个开关。
- **消费方源码零改动**：facade 的 feature 名（48 个）与模块名（`uva::tool_read::…`）
  一字未改，消费方的 `Cargo.toml` 与源码都不需要动；仓内四个消费方（harness 的
  dev-deps / mcp-server / runtime / external-consumer）的 manifest 改指族 crate；
  三个独立 consumer proof 与 `make examples` 全部照旧通过。
- **Breaking（两处嵌套模块改名）**：内层模块与外层同名会触发 `module_inception`
  （deny-by-default），故 `uva::tool_task::task::*` → `uva::tool_task::core::*`，
  `uva::budget::budget::*` → `uva::budget::model::*`。**类型路径不变**
  （`uva::tool_task::TaskSpec` / `uva::budget::Budget` 仍在模块根部转出）。
- **依赖画像变化（消费方可见）**：工具族把 `tokio`（fs / process）带进了每个工具
  feature——此前 `task` / `scheduler` / `search` / `memo` 四个插件是 runtime-free 的，
  现在这四个插件的消费者会多一个 `tokio`。插件通道的 `protocol` 半边保持
  serde-only（默认 feature 只开它）。
- **范式 crate 不动**：`uva-tool` / `-provider` / `-core` / `-kernel` / `-harness`
  等公开面与冻结基线一字未改；`uva-harness-tokio` 保持独立（harness 不得带 feature）。
- **门禁按新拓扑改写**：`check-plugin-shape` 递归扫描族 crate 的模块目录（否则会静默
  失效）；`check-publishable` 的打包序 45 → 29 项；三个族 crate 各有 `pub use` 预算基线。
- **插件目录**：[`docs/plugins.md`](docs/plugins.md) 给出"我要 X → 装哪个 feature →
  实现哪个 trait"的路线，配套可运行的第三方插件样例
  （`docs/examples/plugin-author`，只依赖 `uva` 一个入口）。

### Added (SPA information architecture, knife 9)

- **SPA 信息架构重排（对话优先）**：主界面收成会话 / 对话 + 设置分区，旧目录页全部删除；
  侧栏换成工作区树（默认工作区常驻）+ 跨域搜索 + 用户入口，可 `Ctrl/Cmd+B` 折叠；
  对话流用户消息右对齐气泡、思考块输出中展开完成后自动折叠；输入区带待办进度与
  bash 风格 `❯` 提示符；设置面是解析后 profile 的反推（无 settings 文件），九个分区
  （账户 / 通用 / 插件 / 模型 / 智能体 / 技能与命令 / MCP / Hooks / 自动化）经
  `src/api/settings-profile.ts` 绑 `profile/list|get|set|create|delete`，开关如实显示
  `applied` / `restart-required` / `overridden` / `failed` 四态；`/chat` 不再在挂载时
  铸会话，首条消息才创建。

### Added (UI tree and slot contract, knife 8)

- **UI 树与插槽契约（刀 8）**：`ui-bundle` 行声明插槽（`chat.sidebar`、
  `session.detail.extensions`…，id 与宿主无关），UI 装载器以 Effect 语义装配
  （注册即句柄、drop 即回滚，未声明插槽 / 重复声明 / `one` 超基数在激活阶段
  失败）；`ui/schema` 新增 `slots`；浏览器（`SlotRegion`）与终端（ratatui
  右列 / 底部块）各渲染一次同一棵树，TUI 因此可扩展。v1 仍永不执行远端
  JS/CSS。

### Added (profile lifecycle: enablement, transactional writes, built-in MCP)

- **profile 生命周期**：profile 行自带 `enabled`（`true` / `false` / 插件判定
  `{ service, check, arg }`，判定不可用按 fail-closed 不装配；boot 因此分两波
  挂载——先静态行，再用挂好的 Context 求值判定行并挂第二波）；动态变更成为
  事务（内存重解析层栈 → 试装 → 原子替换文件），结果回报 `applied` /
  `restart-required` / `overridden` / `failed` 四态；创建 / 删除能力经内置
  MCP 通道与 `profile/*` JSON-RPC 方法走同一条写路径，agent 写动作按
  `Risk::High` 走既有审批；提交后广播无 `session_id` 的 `profile/changed`。
- `uva-core` 新增叶子模块 `enable`（`EnableChecks` 判定契约，不加
  crate-root re-export，frozen 基线不动）；`uva-skill` 的 `skills` 行
  发布第一个判定服务 `skill-checks`（读 SKILL.md 的 `metadata.enabled`）。
- `uva-mcp` 贯通协议原生的 `annotations`：`ServerTool` 可声明、
  `tools/list` 透传、`McpTool` 落到 `Tool::annotations`（旧服务器不带该字段
  时行为不变）。

### Added (TUI directory browsing, wave 2: the catalog domains)

- **The seven directory domains join the browser with table rows only**
  (`agents` / `skills` / `tools` / `tasks` / `schedules` / `workflows` /
  `evals`): each is a `DomainSpec` entry — list method, row pointer,
  columns, `get` method and operations (`agents/delete`,
  `skills/delete|reload`, `tools/unregister`, `schedules/tick|delete`,
  `workflows/run|delete`, `evals/run|delete`; `tasks` is read-only on the
  server). No new screens.
- **The bespoke-envelope domains** (`models` → `/models`, `plugins` →
  `/resolved`, `ui/schema` → `/schemas`) and `search` (required `q`,
  reachable from the sidebar's 🔍 / `s` and `:search <词>`) decode
  through the same `rows_pointer` table; their `Enter` flattens the row
  into the key/value detail page.
- **The `:json` gate** covers multi-field writes (`create` / `patch` /
  `replace` / `register`): `json <method> <params>` parses one JSON
  object, confirms like any write, and only sends methods in the
  client-side copy of the server registry (`KNOWN_METHODS`, which the
  tests keep in sync with every declared domain method). Approval
  `y`/`n` keeps its precedence over the palette.
- `docs/host-capabilities.md` §6 records the per-domain TUI landing
  (method coverage, evidence paths) — the plan's `docs/host-capabilities.yaml`
  and `make check-host-capabilities` do not exist in this tree; the md
  is the matrix's source of truth.

### Added (TUI directory browsing, wave 1: sessions + chat)

- **The TUI grows a directory browser.** `crates/uva-tui/src/domains.rs`
  is one column table per domain (list method, row pointer, columns, `get`
  method, per-operation param key); `views/list.rs` renders any domain's
  `List<T>` envelope through it, `views/detail.rs` flattens any object
  into a key/value page and draws the write-confirmation modal, and
  `views/catalog.rs` is the domain → list → detail state machine with the
  command palette (`:` / `Ctrl+k`). `RequestKind::Catalog(&'static str)`
  carries any directory method, and `main` intercepts its responses
  before the reducer so a directory answer can never overwrite the
  connection status line.
- **Wave 1 scope**: `sessions` (list / get / usage / status / export /
  fork / resume / compact / delete) and the conversation's `chat`
  operations (`regenerate`, `feedback`), with `resume`'s token assembled
  from the `sessions/follow` baseline. Writes confirm first (`x`/`dd`,
  palette operations); approval `y`/`n` keeps its precedence.
- **`o` opens the directory** from the sidebar; `Tab` cycles domains;
  `r` re-lists; D30's list motions drive the table. A narrow terminal
  shrinks the rightmost column instead of panicking.

### Added (the capability matrix: every domain traced to a profile row)

- **`docs/host-capabilities.md`** — the knife-4 capability matrix: 15
  capability domains, each with host landing / data source / owning
  profile row / behavior when the row is absent. Rows are derived from
  the resolved profile (`uva-app-server --dump-config`, measured on
  a synthetic deployment) and the `~/.agents` layout conventions, not
  hand-curated; the file also records the one-shot reconciliation (24
  rows / 20 referenced method names all present in the registry) and
  the four known design-doc ↔ repo drifts. Docs only — no new gates.

### Added (projection wave 2: the desktop shell)

- **The desktop entry (knife 3).** `desktop/` is a Tauri 2 shell in
  its own workspace — deliberately not a root-workspace member, so
  the webkit system stack never becomes a prerequisite for
  `make ci`. The shell does exactly four things, all through
  `crates/uva-gateway`: launch the `uva-app-server serve`
  sidecar, wait for `/readyz`, open a window on the gateway's
  `base_url` (the same `uva-web/dist` the browser serves), and
  re-navigate the window when a restarted generation lands on a new
  address / reap the sidecar when the window closes (no orphans). It
  speaks no protocol — no `tokio-tungstenite`, and the direct
  dependency list stays at `tauri` + `uva-gateway` + `anyhow`.
  The new `.github/workflows/desktop.yml` carries the shell's own
  fmt / clippy / test / `cargo deny` gates (closing the gap where
  the root `cargo deny` never sees the desktop lockfile's
  gtk/webkit tree).

### Changed (projection wave 2: the TUI joins the gateway)

- **`uva-tui` no longer talks to a core.** The transport layer now
  drives `uva-app-server serve` through `uva-gateway` — the
  same `/rpc` WebSocket the browser speaks — instead of spawning
  `core` and framing `Content-Length` over stdio. Process
  supervision (spawn, `/readyz`, the 1s → 5s backoff, reaping) moved
  to the gateway crate; what the TUI keeps is its own domain: the
  `RequestKind` ↔ method mapping, one spawned task per request (the
  `Response`-carries-its-`RequestKind` contract is unchanged), a
  catch-all notification subscription, and the re-issue of the
  recorded `sessions/follow` whenever the connection is new again
  (a core `ready` broadcast, a supervisor restart, or the first
  answered `health/ping` after an outage — the gateway never
  re-broadcasts a boot a late connection missed, so the liveness
  probe synthesizes the initial `ready`). `--attach <url>` connects
  to an already-running gateway without spawning or supervising one;
  a dropped gateway is reported as unreachable while the client
  keeps reconnecting on its own. The crate's dependency moved from
  `uva-app-server` to `uva-gateway`; the reducer and the
  ratatui renderer are untouched. The gateway crate learned two
  things the TUI needs: `GatewayClient::subscribe_all` (a local
  catch-all — nothing registered on the wire, nothing replayed) and
  the reaping of a generation whose `start` was abandoned before its
  outcome was delivered, so a host quitting during boot leaves no
  respawning orphan.

### Added (projection wave 2: the shared gateway crate)

- **`uva-gateway` — the foundation of projection wave 2.** A
  `publish = false` support crate the TUI and the desktop shell both
  build on: a supervisor that launches `uva-app-server serve`,
  waits for the `/readyz` **status line** (never just "the port
  accepts a connection"), restarts a dead gateway with the 1s → 5s
  backoff, and reaps the child on drop; plus a JSON-RPC 2.0 client
  for the gateway's `/rpc` WebSocket that correlates responses by
  `id`, fans notifications out to subscribers, and replays every
  live subscription after a reconnect without double-mounting a
  handler. The public surface is `futures`-only — `tokio` stays an
  implementation detail, so no runtime-exemption entry is needed.

### Added (plugin-spine stage 4: the Wasm channel)

- **The Wasm calling convention is dynamic.** `describe` / `execute`
  are looked up with `Instance::get_func` and invoked through
  `Func::call(&[Val], &mut [Val])`, so the exports are validated and
  lifted by name at mount rather than through a typed-func binding. A
  missing or mistyped export is still a mount-time
  `DescribeFailed` / `ExecuteFailed`, and an `execute` that answers
  with a non-string is the same clear error instead of a panic.
- **Capabilities are enforced at mount, with imports named.** An
  instantiation failure now classifies the component's imports: a
  capability the manifest does not declare is `MissingCapability`
  naming both the import (`http-request`) and the capability
  (`net:http`); an import the linker does not know is
  `UnsatisfiedImport` naming the import. In both cases no guest
  instruction executes.
- **`WasmToolPlugin::with_schema_override` / `ToolSchemaOverride`.**
  A deployment can rename a mounted component's tool or replace its
  advertised parameter schema without recompiling the guest.
- **The app-server has a `wasm` profile row** (opt-in: it is
  registered as a factory but not part of `native_profile`). Its
  config is
  `{ "component": "<workspace-relative path>", "tool": { "name"?, "description"?, "parameters"? } }`;
  a `.wat` text component works. The row's `capabilities` are copied
  into the plugin manifest, so a declared `fs:read` reaches the
  runtime's linker and an undeclared import fails the mount. The
  app-server enables the facade's `wasm` feature, the only feature
  that resolves `wasmtime`.
- `PluginFactory::build` now receives the profile row's declared
  `capabilities: &[Capability]`, threaded from `build_one`.

### Changed

- **app-server: capabilities are built by their rows** (plugin-spine
  stage 3). Every native profile row constructs its object in
  `apply` from the boot context — sessions, compaction (injects
  `llm.default`), sandbox, skills, mcp, eval, scheduler, attachments,
  the five tool bricks, memory and search. `KernelHost` shrinks to the
  shared container and the registries the search row reads; the boot
  no longer builds capability objects, only reads the published
  services back for the projection layer. The `model-adapter` factory
  no longer falls back to a host catalogue: the boot materializes the
  row's config from the workspace configuration.
- **app-server: `KernelHost` is gone** (plugin-spine stage 3). The
  host is now `BootServices` and holds **containers only**: the shared
  `ToolRegistry`, the peer-agent `AgentRegistry`, the harness `Host`,
  and the `[plugins]` composition's registrations. Every capability is
  built by the row that publishes it: `subagent-backend` builds the
  `task` delegator over the agent registry, `approval` publishes the
  default `ApprovalPolicy`, `search` builds its own session index over
  the boot context's sessions directory, and `agent-loop` builds the
  deployment's loop from its row config.
- **The `agent-loop` row is config-driven.** Its row config is
  `{ "strategy"?: "<alias>", "tool_restriction"?: { "allow"?: [...],
  "deny"?: [...] }, "compaction"?: { ...CompactionSettings } }`; the
  boot materializes it from the deployment's agent table
  (`agents.<default>.strategy` / `allowed_tools` / `denied_tools` /
  `compaction`) when the profile leaves the row empty, and a profile
  that already carries a config wins. An unknown strategy alias is
  logged and falls back to ReAct, as before.
- **The delegation backend is a service.** The `agent-loop` row
  resolves the `subagents` service the `subagent-backend` row
  publishes and installs it as the loop's delegator, and
  `AGENT_LOOP_INJECTS` names `subagents` — so the kernel mounts the
  loop after the backend, and a remount of the backend cascades to
  the loop. The deployment now has **one** `TaskDelegator` instead of
  two instances over the same registry.
- The prompt manifest is a live service snapshot
  (`AppState::prompt_context`), so a skills remount reaches the next
  session/agent build.
- Plugin constructors changed accordingly:
  `SessionStorePlugin::new(SessionStoreConfig)`,
  `CompactionPlugin::new(CompactionPluginConfig)`,
  `SandboxPlugin::new(SandboxPluginConfig)`,
  `SkillPlugin::new(SkillPluginConfig)`, `McpPlugin::new()`,
  `EvalPlugin::new(EvalPluginConfig)`,
  `SchedulerPlugin::new(SchedulerPluginConfig)`,
  `AttachmentPlugin::new(AttachmentPluginConfig)`.

### Changed

- **harness: seam dispatch runs on kernel waterfalls** (plugin-spine
  stage 2). The `Seams` layer owns seven kernel `Waterfall` chains and
  dispatches the four loop seams plus the two llm points through them;
  foreign middleware registers via `Host::waterfall(key)` and rules
  with `SeamRuling` (`Ask` collects, `Block` / `Terminate`
  short-circuit; anything unrecognizable fails closed as a block).
  `Host::hook` / `Host::gate` now land their registrations as chain
  middleware (the five observation points keep the residual hooks
  table) and `Host::request_transformer` lands on the `llm/request`
  chain, so the approval gate and process-plugin vetoes dispatch on
  `tool/pre-execute` by construction. Guard / hint / tracker /
  output / result transformers / prompt sections / tools / delegators
  stay on their tables — the home table is in the host module docs.
- `AgentState` derives `Clone` (an active seam pass snapshots the run
  state the chain adapters rebuild a `HookContext` from).

### Added (plugin-spine stage 1: everything-a-plugin assembly)

- **`uva-runtime`, the config-driven assembly crate.** The
  provider catalogue (`ProviderCatalogue`, tier-1 wrapping chain
  `wrap_tier1_providers`, `ModelAdapterConfig`) and the agent-loop
  factory (`server_agent_loop`, `AgentLoopDefaults`) move out of the
  app server into a library the facade exposes as the non-default
  `runtime` feature.
- **The `model-adapter` plugin is config-driven.** A profile row with
  a `ModelAdapterConfig` builds its own catalogue; the plugin
  publishes `llm` (the catalogue) and the new `llm.default` service
  key (the tier-1-wrapped default handle). An empty row config falls
  back to the deployment's `.agents/config.toml` catalogue. Boot no
  longer statically constructs the provider surface: `AppState`'s
  provider fields are backfilled from the mounted services and
  re-synced on every profile reload.
- **The `agent-loop` plugin is solidified.** All four assembly sites
  (the boot default agent, session run streams, registered agents,
  self-mcp) build through the mounted `agentLoop` factory; the
  deployment knobs (strategy, tool restriction, context manager,
  compactor, spawner, delegators) resolve in one place.
  `AgentAssembly` exists only inside the runtime crate now.
- **Profile reloads cascade to injectors.** A changed or removed
  provider remounts every plugin that injects its services,
  transitively — a provider swap reaches new runs without
  restarting, while in-flight turns keep their handles. Disposing
  `model-adapter` parks `agent-loop` until a remount recovers it.
- **`uva-app-server translate`** migrates a legacy `config.yaml`
  into a profile TOML's `model-adapter` row, reporting the sections
  it maps and the ones it leaves on `ServerConfig::load`.

### Changed (breaking)

- **The `config.yaml` translation layer is gone.** Its
  `[providers]` / `[agents]` / `[tools]` / `[plugins]` sections no
  longer translate into profile rows — run `translate` to migrate the
  provider sections. `.agents/config.toml` remains the fallback
  provider input for deployments without a profile row; a config
  change now requires a restart (HMR owns profile documents only).

### Added (the app-server boots through the kernel)
- **The native plugin set is mounted, not hand-wired.** Boot builds a
  `uva::kernel::Context`, registers the built-in factories
  (`tool-registry`, `model-adapter`, `session-store`, `sandbox`,
  `subagent-backend`, `context-compactor`, `approval`, `skills`,
  `agent-loop`, `mcp`, `eval`, `scheduler`, `schedule-tool`,
  `attachments`) in a `PluginFactories` directory over real host
  objects, and calls `build_plugins` → `mount_all`. `AppState` keeps the
  context and the `MountHandle`; `AppState::unload_plugin(id)` unloads
  one plugin, and `shutdown_kernel` unloads the rest on shutdown.
- **`MountHandle::dispose_plugin(id)` and `PluginState`** land in
  `uva-kernel`: one plugin's tracked effects and fiber are disposed
  in place, consumers that injected its services park as `Pending`, and
  every sibling keeps running. Disposal is idempotent.
- **The prompt builder reads the `skills` service.** Boot no longer
  registers the `skill` Core tool; `SkillPlugin` owns that token-scoped
  registration, and `<available_skills>` disappears with the service.
- **The MCP tick loop is boot-owned and stopped explicitly.** The loop
  re-reads `mcp` each wake; `McpSupervisor::shutdown` withdraws every
  generation's tools and drops the clients (and their transports),
  so remote tools do not outlive the service.
- **Capability consumers are wired to their services.** Eval runs
  resolve `agentLoop` and drive the `eval` service's runner; the
  scheduler driver ticks the mounted `scheduler` service and releases
  the leader lease when it stops; chat and Anthropic-messages turns
  refuse image / audio / stored-ref input while the `attachments`
  service is unmounted.

### Added (UI schema projection, HMR remounting, and the kernel facade)

- **`ui/schema` projects the `[ui]` profile segment as JSON schema.** A
  UI row must be in the `ui.` namespace, select the `ui-bundle` runtime,
  and describe `version` / `components[{slot, kind, props}]`; the
  loader rejects a core key and any executable field (`script`,
  `stylesheet`, …). v1 never executes remote JavaScript or CSS — the
  frontend renders only the kinds in
  `uva-web/src/components/ui-schema/` (`panel`, `list`,
  `key-value`) and shows an unknown kind as a placeholder.
- **The core remounts only the rows a profile change names.** With
  `hmr = true`, the core polls the profile layers once a second,
  resolves the new document, and diffs it against the effective mount:
  `removed` rows unload, `added` / `remounted` rows mount as one new
  kernel group, and a volatile-only change leaves the fiber alone.
  Streaming turns keep the service handles they resolved. `hmr = false`
  stops the loop and reports that a restart is required. An explicit
  `[core]` row set is authoritative (omission disables a plugin); an
  empty resolution (no profile document) boots the built-in set.
  Residual: remounting the `mcp` row republishes the service but does
  not restart the boot-owned tick loop or its dropped clients — a
  process restart re-supervises the configured servers.
- **`--dump-config` prints each row's real `provides` / `inject`.** The
  command now builds the registered `PluginFactories` (over a
  manifest-only host) and calls `dump_with_manifests`; a row with no
  factory prints explicit empty arrays, and the UI segment does too.
- **The facade's feature table names `kernel`.** `uva::kernel`
  re-exports `Plugin`, `PluginManifest`, `mount_all`, `MountHandle`,
  `Context`, `EffectHandle`, `PluginState`, and the `ui.` namespace
  boundary.
- **Deprecation direction (not removal).** `uva_harness::Host` and
  the app-server's `AgentAssembly` stay with a documented direction
  toward kernel effects / the `agentLoop` service: chat still assembles
  each run through `AgentAssembly`, so deleting the wrapper now would
  break chat. The target is the mounted `agentLoop` service (eval runs
  already use it).

### Added (the plugin kernel gains an opt-in Wasm component runtime)

- **`uva-wasm-runtime`** loads a WebAssembly **component** exporting
  `describe() -> string` and `execute(input: string) -> string`, calls
  `describe()` exactly once at mount, and caches the JSON schema.
  Components are cached by content hash, and a successful call reuses its
  store/instance; a trap discards the store, so the next call
  instantiates a fresh one instead of resuming inconsistent guest state.
  The cache holds `Weak` references, so dropping the last mounted
  component evicts the entry and releases the compiled JIT code
  (`WasmRuntime::evict` is the explicit form).
- **Capability gating is a linker decision.** `log` is implicit,
  `fs-read` is linked only when the manifest declares `fs:read`, and
  `http-request` is linked when the embedding host installs a
  `CapabilityHost` (`WasmRuntime::with_capability_host`) that supplies
  `net:http`. An undeclared import fails the mount with
  `missing_capability`, naming the capability, before the guest runs;
  a declared capability no host supplies fails with
  `unsupported_capability` instead of silently losing the import. Host
  functions are kebab-case component imports (`fs-read`, `http-request`);
  `fs-read` confines reads to the runtime's workspace root.
- **Per-call limits.** A fresh fuel budget, a linear-memory ceiling
  enforced by a `ResourceLimiter` (the refused growth is recorded so a
  following trap maps to `memory_limit`), and an epoch-based wall-clock
  deadline. Every failure maps to a stable `WasmErrorKind` code:
  `fuel_exhausted`, `memory_limit`, `timeout`, `invalid_json`, `trap`,
  and the mount-time kinds.
- **Kernel seams.** `WasmToolPlugin` registers the mounted component as a
  `Tool` under `tools` and withdraws exactly its registration on unload;
  `WasmSandbox` implements `SandboxBackend` and fails closed for confined
  OS policies it cannot enforce.
- **Opt-in.** `wasmtime` 50 is pinned with `default-features = false`
  (component model, runtime, Cranelift, std, WAT only). The facade's
  `wasm` feature is default-off, and `make check-wasm-deps` asserts the
  default dependency graph resolves zero wasmtime packages while
  `--features wasm` resolves at least one.

### Added (profile TOML assembly, a deterministic dump, and HMR diffing)

- **`uva-app-server` accepts `--profile`, `--bundle`, `--patch`, and
  `--dump-config`.** Layers apply in the documented order — empty →
  bundle patch → profile document → home patch (`~/.agents/profile.toml`
  or `UVA_HOME_PROFILE`, only when it exists) → CLI overlay — with
  whole-config replacement, `insert`, and `disabled`; an unknown patch id
  is an error. `--dump-config` prints the resolved tree as deterministic
  JSON and exits 0, and `dump_with_manifests` adds each row's
  `provides` / `inject` from the factory manifest. The legacy
  `config.yaml` `[providers]` / `[agents]` / `[tools]` / `[plugins]`
  sections are translated with deprecation warnings, and an id defined in
  both the profile and `config.yaml` fails the boot.
- **`PluginFactory` / `ProfileWatcher`** bind a profile id to a plugin and
  diff two resolutions so only affected fibers remount; volatile config
  fields are skipped and `hmr = false` reports `restart_required`. The
  boot wiring that registers factories, mounts the result, and performs
  the remount loop lands with the kernel cutover.

### Changed (breaking: `uva-app-server` — a headless JSON-RPC core, a WebSocket gateway, and the `~/.agents/` layout)

- **`uva-app-server` is now `uva-app-server`**, and one binary
  carries two subcommands. `core` (the default) is a headless process
  speaking JSON-RPC 2.0 on stdio — Content-Length framed envelopes
  (`Content-Length: <n>\r\n\r\n` plus exactly `n` bytes, default
  ceiling 8 MiB); stdout carries only protocol frames
  (logs go to stderr, selected by `uva-telemetry`'s new
  `log_writer`), and the process announces itself with one `ready`
  notification after boot. `serve` is the App Server gateway: it
  spawns `<current_exe> core`, forwards every browser WebSocket frame
  verbatim, and adds what a browser cannot do for itself — responses
  routed back by request id to the one connection that asked,
  `session/event` / `anthropic/messages/event` notifications routed
  by `params.session_id` to the connections following that session,
  approval interrupts forwarded from a frontend modal to
  `sessions/resume`, and core supervision (a dead core restarts with
  1s→5s backoff; a reconnecting client re-follows from its cursor and
  the fresh baseline rebuilds the timeline).
- **The HTTP surface is gone.** No `/api/v1`, no SSE, no auth wall,
  no CORS, no `track_metrics` middleware: the process is single-user
  local and mints every session for the one operator
  (`user_id = "dev"`). The gateway keeps exactly four HTTP probes —
  `/livez`, `/readyz` (core alive **and** `ready` seen), `/version`,
  `/metrics` (the Prometheus exposition via
  `uva::telemetry::gather_text()`; the HTTP RED middleware that
  used to fill it went with the HTTP surface, so the exposition is
  empty until gateway-level counters land) — and serves the web UI
  statically (`--web-dir`, or the
  `embed-web` feature, which bakes `uva-web/dist` into the binary
  with rust-embed). The management and chat surfaces live on ~57 RPC
  methods (`sessions/*` with `sessions/follow`, `chat/*`,
  `agents/*`, `skills/*`, `tools/*`, `schedules/*`, `workflows/*`,
  `tasks/*`, `evals/*`, `attachments/*`, `search/all`, `health/*`,
  `models/list`, `plugins/list`); the Anthropic Messages protocol is
  preserved as the `anthropic/messages` method with its events on
  `anthropic/messages/event`. Application errors are one shape:
  `error.code = -32000` with the machine code in
  `error.data.code` (`not_found`, `validation`,
  `follow_cursor_truncated`, …). The old HTTP integration tests are
  replaced by `tests/stdio_roundtrip.rs`, which spawns the binary
  and drives the wire end to end.
- **Definitions live in a two-layer `~/.agents/` layout.**
  `uva-core::layout` (std-only) defines the roots —
  `<workspace>/.agents` and `~/.agents`, with `~/.uva` as the
  legacy global — and the four-source precedence: 项目直定义 >
  项目插件包 > 全局直定义 > 全局插件包. Skills, agents and task
  declarations are discovered from it; the `~/.claude` compatibility
  root is gone. The new `uva-agents` crate makes
  `agents/<name>/agent.md` (+ optional `config.json`) the storage an
  agent definition is: file-as-storage, so what the operator edits is
  what the process reads, and the boot materializes each file-backed
  agent (the built-in `agent` name is protected from shadowing).
  `uva-scheduler` grows `task_file` — `tasks/<name>/task.md`
  declarations the boot syncs with the store by name, exporting
  undeclared active jobs back to files. `uva-mcp` grows
  `config_file`: MCP servers come from the layout's `mcp.json` files
  (a bundle's servers namespaced `<plugin>/<server>`), replacing the
  removed `[mcp_servers]` config section. A plugin bundle may also
  declare a **code component** in `plugin.json` — a subprocess the
  host runs over the existing plugin-protocol channel (the channel
  grows `env` overrides; bundle code installs with tools+observe
  capability, reduced authority, unclassified risk, and sits behind
  the `[plugins]` config in precedence). `plugins/list` answers a
  third array, `bundles`, so an operator can see what the layout
  declared.
- **The config chain gained a global fallback and a parse fix.**
  `~/.agents/config.toml` (then legacy `~/.uva/config.toml`) back
  the workspace config, and a config file is parsed by its extension
  (`.yaml`/`.yml` → YAML, `.toml` → TOML, otherwise JSON) — the old
  reader parsed every file as JSON.
- **The MCP supervisor is kept alive.** The boot dropped it on the
  floor after the first tick, so a server that died never came back;
  it now lives on `AppState` and the scheduler driver ticks it.
- **The web frontend speaks the one protocol.** `uva-web` gains
  `src/api/rpc.ts` (one WebSocket at `/rpc`, string ids, a
  notification bus, capped-backoff reconnect); `src/api/client.ts`
  keeps every exported call signature but translates the REST paths
  onto RPC methods internally, so the pages did not change with the
  transport. The chat stream is `sessions/follow` + `session/event`
  with reconnect re-follow; the dev proxy forwards `/rpc`
  (`ws: true`) beside the probes.

### Removed (breaking: the Redis cluster backend, `uva-hpc-batch`, and the deployment split)

- `cluster-redis` (the Redis pub/sub bus and lease backend; the local
  in-process bus stays, inert on one node), the `uva-hpc-batch`
  crate and the facade's `hpc-batch` feature, `Dockerfile.web`,
  `nginx*`, `docker-compose.prod.yml` (the release image is one
  artifact: `uva-app-server serve` with `embed-web`), and the
  `contract-closure/` scanner with `make contract-scan/report/check`
  and its workflow — its subject, the REST router, no longer exists;
  the wire contract is exercised by the stdio roundtrip test and the
  web e2e.

### Changed (breaking: harness 0.2 — one kernel, reversible extensions, a log-owned transcript)

- **One interception registry.** `uva_steering::{Steering, HookMap}`
  are gone. `uva_harness::Host` holds every extension point —
  `Hook`, `Gate`, `Guard`, `Hint`, `Tracker`, `OutputTransformer`,
  request / result transformers, prompt sections, tools and
  delegators — and each registration is reversible: dropping its
  handle withdraws it. `Host::new()` is neutral; `Host::default_policy(root)`
  installs the defensive set. `ReActAgent::with_host` now returns
  `Result<Self, Vec<host::Diagnostic>>`; `with_steering` and
  `with_checkpointer` are deleted.
- **The transcript is in memory; the log is read once.** A run repairs
  and folds the log at setup and then reads/writes an in-memory
  transcript, recording each message's row ordinal beside it. The
  step boundary no longer re-reads the log; `HistoryProjection`,
  `refresh_history` and `rebuild_transcript` are gone. `RunState`
  gains `surface_seqs` and `log_rows`; `HistoryProjection` is deleted.
- **The `Checkpointer` seam is deleted.** Resume is driven by the
  session log only; `CheckpointSnapshot`, `InMemoryCheckpointer` and
  `AgentInput`'s snapshot semantics are removed.
- **`uva-session` owns the log contract.** New `SessionLog`
  (object-safe: read / append / flush / repair), `RepairReport`,
  `repair_log`, and `CompactionSettings` (moved from
  `uva-context`). `begin_compaction` / `finish_compaction` take
  `&dyn SessionLog`; `plan_compaction` takes
  `(history, surface_seqs, compactor, budget)` and
  `plan_compaction_from_rows` is the rows-based entry. `CompactionCheckpoint`,
  `TypedEventRecord` and their views are deleted.
- **One compaction engine.** `uva-context`'s
  `SummarizingContextManager`, `DagContextManager`, `SessionMemory`,
  `events_to_messages`, `repair_session` and the compaction-settings
  module are deleted. A `ContextManager` only projects or trims the
  request it is handed; compression is the session's `Compactor`.
- **Live events have a soft cap.** `EventSink` drops live events above
  `AgentCaps.env.event_capacity` (default 1024) and reports the gap
  with `SystemEvent::EventsDropped`; the durable log is unaffected.
  `EventSink::append_plugin(kind, payload)` writes a durable
  `SessionEvent::Plugin` row from any registration, and `RunInbox` /
  `ChannelInbox` give a run a steer channel (`AgentInput::with_inbox`).
- **Delegated children run where they are told.** `runner` creates the
  worktree before the child starts and passes it as
  `AgentInput::with_workspace`; the `task` tool advertises `isolation`,
  and a high-risk delegated call now goes through `ApprovalPolicy`
  instead of being refused outright.
- **Server.** The server installs `Host::default_policy` and a
  `SummaryCompactor` built from the deployment's policy, writes
  `parked_prompt` rows with the turn's provider and model (a takeover
  resolves them instead of falling back to the default), forwards a
  remote submit's model to the owner, projects the live turn on any
  node from the owner's frames, and uses
  `uva_harness_tokio::{TokioSpawner, TokioBlocking, TokioSleeper}`
  instead of its own adapters.

### Changed (durable rows name the configured provider, not its model)

- **`parked_prompt` and `request_header` record the catalogue provider
  id.** An OpenAI-compatible provider reports its *model* from
  `ModelProvider::name`, so a row built from the handle named the model
  twice and the provider was unrecoverable from the log. The pin now
  carries the id the selection resolved from, and the harness stamps
  the run's configured provider id (`AgentConfig::provider`) instead of
  the handle's name. A takeover reads the same pair the turn was
  submitted under.
- **`GET /api/v1/sessions/{id}/follow` 404s for a session the server
  does not know.** A directory created a moment earlier still counts
  (its first row may not be written yet), so a fresh chat connects
  immediately; a typo has neither directory nor log and is refused
  instead of minting one. The web page therefore creates its session
  through `POST /api/v1/chat/sessions` and navigates to the id the
  server returned, instead of guessing one client-side.

### Fixed (an approval can be answered: the resume continues the same turn)

- **The resume token carries the envelope the turn actually ran
  under.** The follow stream's `request_snapshot` row now ships the
  request-envelope hash with it. A page that connected before the run
  started only knew the hash from its baseline — the hash of an
  earlier turn — and every resume was refused with `409 config_drift`.
- **A resumed turn samples the provider it was suspended under.** The
  controller resolves the provider and model from the log's last
  request snapshot, so the resumed run reproduces the envelope it
  continues. Before, it sampled the deployment default, which changed
  the envelope and was refused for the same reason.

### Fixed (a failed run is visible, and says why)

- **A failed turn records its message.** The turn-end row carries
  `data.message` with the text the run stopped with (the row's
  `data.reason` still says `error`), so the log states the cause
  instead of one word.
- **The chat page shows the failure.** A run that dies before it
  produced an answer had no bubble to carry the failure, so the page
  returned to "idle" as if nothing had happened; it now renders a
  `Run failed: …` banner (`data-testid="turn-error"`) carrying the
  same message.

### Changed (gates)

- `make check-test-layout` is back to its permanent shape: no inline
  test module may reach 400 lines, and the 300-line band is zero.
- `RUNTIME_FREE` (the `check-no-runtime` list) now also covers
  `uva-harness`, `uva-context`, `uva-steering`,
  `uva-plugin-protocol` and `uva-tool-task`, which the 0.2
  split made runtime-free.

### Removed (breaking: the `uva-safety` crate and the facade's `safety` feature)

- **`uva-safety` is deleted**, together with the facade's `safety`
  feature and `uva::safety` module. Nothing in the workspace ever
  constructed a `SafetyScanner`, so the crate documented a wiring that
  did not exist. Content scanning is a `Host::hook` registration a
  consumer installs; it was never a built-in feature of the loop.
  Release notes above that describe the crate are the historical
  record of the round that added it.

### Changed (breaking: the session log format is v2, and old logs are refused, not migrated)

- **`CURRENT_SCHEMA_VERSION` is `2`.** A log whose header names any
  other version — older *or* newer — is refused at read time with
  `UnsupportedSchemaVersion`, whose message states the version read,
  the version this build supports, and the operator's two ways out
  (export with a build that reads that version, or reopen it there
  after migrating). There is no lazy migration: a format bump is a
  file-level fact, and silently rewriting a log on open would make
  the durability story a lie. `docs/log-format.md` documents the v2
  shape.

### Added (the interface contract is scanned, checked, and committed)

- **`make contract-scan` / `make contract-check` are green, and their
  output is committed.** `docs/interface-contract/contract.yaml`
  (with `.json` and the human-readable `.md`) is the scanned union of
  the backend router and the web client's calls. The check reports
  the two ways the sides can disagree: a route the frontend calls
  that the backend does not register, and a route the backend
  registers that the frontend never calls. The scan writes into
  `docs/interface-contract/`, and the target now creates that
  directory — a fresh clone used to fail the scan with `ENOENT`.
- **Backend routes the web app deliberately does not call are
  curated, not drift.** An Anthropic-compatible client
  (`POST /v1/messages`), an orchestrator's probes (`/livez`,
  `/readyz`, `/version`, `/metrics`), the attachment store's own
  API, and the session-scoped variants the web app reaches through
  another route have no frontend call to pair with. Each one is
  listed in `contract-closure/curated/web-free-endpoints.json` with
  the reason it is web-free, and the contract carries it as
  `frontend_consumed: false` — so the check stays meaningful
  instead of crying drift over a deliberate omission.
- **The curated list is the only thing that can silence the
  check, and it has to say why.** The scanned artifact is output, not
  a declaration: the check strips the suppression flag and the reason
  from it, so a stale (or edited) artifact cannot silence the check
  that reads it. A broken curated file silences nothing — the
  endpoints are reported again — and an entry without a non-empty
  reason is ignored, so a suppression without a reason cannot be
  written at all.
- **The scanner sees the web client's real call shapes.** A fetch
  through an arrow-function URL helper
  (`const FOLLOW_URL = (id) => `/api/v1/sessions/${id}/follow``) and
  an `api.getRaw(path)` download are now recognised as the calls they
  are; before, both endpoints read as unpaired.
- **The scanned artifact records repository-relative paths.** The
  evidence the contract carries (which router line, which frontend
  file) used to be absolute, which tied a committed artifact to the
  machine that generated it; it is now relative to the repository
  root. A re-scan that finds the same contract also leaves the file
  untouched — the generation stamp moves only when the contract
  itself changed.
- **`docs/interface-contract/SMOKE.md`** — the reproducible
  end-to-end smoke path next to the scanned artifacts: boot a
  keyless deployment (`cargo run -p uva-app-server --features
  test-utils -- --directory …` — the `demo` provider only exists
  under that feature), send a message with an attachment, read the
  history back, send two messages and see three turns, restart the
  node and see the history again.

### Changed (`uva-app-server`: queued prompts are durable parks, and one message is one turn)

- **`Prompt` / `Steer` land as `parked_prompt` rows.** A queued
  plain-text turn used to live only in the process's in-memory
  `InputQueue`, so a node that died between the submission and the
  run lost the message. Every queued turn is now a durable park —
  the row is appended and flushed before the op returns — and any
  node that takes the session over recovers it from the log; the
  in-process `InputQueue` is gone (`SessionController::spawn` no
  longer takes one).
- **One queued message is one turn.** Several texts queued behind a
  run used to be folded into a single turn (all but the last became
  history, the last became the prompt). Each queued message now
  parks and runs as its own turn, in order. Cancelling spends the
  parks it drops (`park_consumed` rows), so a takeover does not
  replay turns the user just cancelled.
- **`200 {"queued": true}` means the turn is on disk.** The route
  used to answer as soon as the operation was queued, so the
  message became durable only later — when the controller loop
  parked it. A crash in that window lost it, and the answer
  promised more than the log held. The submit now waits for the
  controller's ack: the route answers after the `parked_prompt` row
  is appended and flushed, and a row that could not be made durable
  is an error answer, not a promise. The wait is bounded (30 s) and
  cannot deadlock — the ack is a oneshot the loop answers, and the
  operation channel is bounded, so a busy controller delays the
  answer rather than blocking the loop. When another node owns the
  session, the answer means the turn was forwarded to that owner,
  which parks it in its own log.

### Fixed (`uva-app-server`: a dead session controller is never reused or leaked)

- **The active-session cache refuses corpses.** A cache hit used to
  return whatever entry was there without checking it, so a client
  that reconnected after a controller exited (idle timeout,
  shutdown, a lost lease — or a panic) got the same dead handle
  back and its next stream ended immediately; the entry also sat in
  the cache for the process's life. The controller's loop now
  removes its own cache entry when it exits (a weak-on-both-ends
  notification — no reference cycle), a cache hit re-checks
  `is_alive()` and rebuilds over a dead entry, a panicking loop
  still flips its alive flag (a drop guard on the loop body), and a
  60 s reaper sweeps any corpse the notification could not remove.

### Added (the curated public API is frozen, with a gate)

- **`make check-frozen-api`.** Every name a frozen crate re-exports
  from its `lib.rs` is listed in `scripts/frozen-api/baseline` (435
  names); the gate compares the live curated surface against that
  baseline, so a name appearing or disappearing without the baseline
  changing fails it. Moving `docs/api-frozen.md` — the
  human-readable half of the same promise — in the same commit is
  the review convention the script's header states, not something
  the gate itself enforces. Removals happen only at a minor bump.
- **Handover docs**: `docs/api-surface.md` (the public surface as a
  tour), `docs/log-format.md` (the v2 shape),
  `docs/contributing/first-steps.md` (clone → build → first change).

### Added (`uva`: the tokio seam adapters, behind an opt-in feature)

- **`harness-tokio`.** `TokioSpawner` / `TokioSleeper` /
  `TokioBlocking` reach the facade through one opt-in feature. The
  feature adds no runtime to a consumer that opts in: it pulls
  `uva-harness-tokio`, whose only runtime dependency is `tokio`
  with the `rt` + `time` features. A consumer on `smol` / `async-std`
  / a bare `futures` executor reaches the adapters not at all, and a
  consumer on tokio gets them with the one dependency instead of a
  second. The facade's **default** features are a different matter:
  they include the two HTTP provider adapters, which carry
  `reqwest` → `hyper-rustls` → `hyper` → `h2` → `tokio`, so
  `cargo tree -p uva -e normal` measures four tokio packages
  there against zero for the seven-feature MVP subset. A consumer
  that wants a runtime-free tree says `default-features = false`.

### Fixed (resume is idempotent, and its failures are distinguishable)

- **A replayed resume decision answers `200 OK` instead of `409`.**
  Retrying a resume whose decision the log already holds is not a
  conflict: the server replies with the outcome the log already
  carries and never dispatches the approved call a second time.
- **`resume_conflict` and `resume_no_snapshot` are distinct codes.**
  A token whose `last_seq` is behind the tail, a token whose
  `pending` set disagrees with the log, and a log with no request
  snapshot at all were previously one undistinguishable
  `stale_resume`; the client can now tell "the log moved on" from
  "this session cannot be resumed at all".

### Fixed (writer durability: no double-writes, and the compaction surface is durable)

- **A retried flush does not write a row twice.** The fs layer now
  reports whether a refused batch's bytes reached the file
  (`AppendFailure { landed }`), the writer skips rows a failed flush
  already wrote, a reserved row stays ahead of the flush, and the
  owed barrier is honoured — the crash window between "bytes
  written" and "fsync proven" no longer duplicates rows on retry.
- **Every shadowable row has a durable ledger key**, assistant rows
  included, so the compaction checkpoint resolves and the durable
  `SessionEvent::Compaction` row lands: a cold replay folds the same
  compacted surface the live session shows (replay == live).
- **A `parked_prompt` row without a `park_id` is warned once and
  skipped** — position is never used to infer which park a row was.

### Fixed (the four crate suites that were red before this round)

- `uva-resilience` and `uva-tool-memo` compile and test green
  (tokio features declared per actual use, waits through the
  `Sleeper` seam); `uva-budget` and `uva-semantic-cache`
  doctests compile against the real signatures.
- **`make ci` now lists `make test-crates` among its
  prerequisites**, so a red member suite can no longer hide from
  the gate that guards the branch.

### Added (`uva-app-server`: the plugin composition is observable)

- **`GET /api/v1/plugins`.** Boot has always recorded which plugins
  installed (`Composition::resolved`) and which entries it skipped with
  the reason (`Composition::skipped`), and nothing read either set: an
  operator could configure a plugin, see `plugin skipped; boot
  continues` once in the log, and then have no way to ask what the
  deployment actually runs. The route returns both halves — resolved
  plugins with name, kind (static / remote), authority, risk and the
  tool names they claimed, plus each skipped entry's name and verbatim
  reason.
- **`ResolvedPlugin` carries the facts the route publishes.** It held
  only `name` and `source`; it now also records the granted
  `authority`, the `risk` the plugin's tools are judged at, and the
  claimed tool names. A remote entry reports its configured authority
  and risk; a static one reports `Authority::Full` (it is in-process
  code, so the escalation that names is already inherent) and the worst
  declared risk bound among the tools it registered.
- **The web shows it.** The session page renders the deployment's
  plugin composition: each resolved plugin with its channel, authority,
  risk and tools, and — always visible, with a count in the section
  header — every skipped entry with the reason boot recorded.
- **`docs/examples/harness-only-consumer`.** A standalone crate (not a
  workspace member) depending on `uva-harness`, driven by
  `futures::executor::block_on` over a `std::thread` spawner: its
  `cargo tree` contains **zero** `tokio`, which `make examples` asserts
  before running it. The harness closure is runtime-*free*, not merely
  runtime-tolerant, and the fixture is the tripwire if that ever
  changes.
- **Two tutorial chapters for a plugin author.** "Write a static
  plugin" (a crate: `impl Plugin` → `install` → `PluginHandle`, then
  the binary's `StaticPlugins` factory table and the two-field config
  entry) and "write a dynamic plugin" (a stdlib-only Python process
  speaking the `uva-plugin-protocol` method set, its config entry,
  and what `GET /api/v1/plugins` shows afterwards). Run transcripts are
  the observed ones.

### Fixed (`uva-session`: a retried flush does not write a row twice)

- **The sink recognises a row it already holds.** A flush that
  failed after its bytes reached the file used to be written again
  by the next flush, and the log ended up with two rows under one
  ordinal — the reader that replays the log saw the row twice.
  `JsonlSessionSink` now keeps the highest ordinal it has written
  and skips the rows at or below it. A row with no top-level `seq`
  cannot be identified and is still written: what cannot be
  recognised cannot be skipped.
- **A refusal says what it left behind.** The write and the
  durability barrier fail differently: a refused *write* is rolled
  back and the batch is **not** in the log, while a refused
  *barrier* comes after the bytes reached the file and the batch
  **is**. The sink needs that difference — it moves its mark for a
  batch that landed and leaves it alone for a batch that was rolled
  back, so the retry writes the rows that are missing and skips the
  ones that are there.
- **The batches reach the log in ordinal order.** The writer's
  drain holds the queue's sender while it takes the rows, so a
  producer cannot slip a row past the batch that was drained, and a
  copy reserves its ordinals under the flush lock. That is what the
  sink's mark rests on: a row that arrived after a later ordinal was
  written would otherwise look like a row the log already holds.

### Fixed (`uva-context`: automatic compaction keeps each tool call with its result)

- **The summarising manager no longer splits a `tool_use` from its
  `tool_result`.** It replaced the contiguous run of tool-result
  messages and left the assistant turn that issued their calls in
  place, so the provider rejected the next request (`400 … an
  assistant message with 'tool_calls' must be followed by tool
  messages …`) and the turn ended in error. Every seed run is now
  widened into its tool-call group before splicing: the start moves
  back to the nearest cut whose prefix has no unanswered `tool_use`
  (the same `balanced_prefix` predicate `uva-session` exposes for
  the manual path) and the end moves forward until the range answers
  every call inside it. The summary-size guard compares against the
  extended range, and `CompactionRecord` reports the extended span so
  the emitted record matches what left the history.
- **The assistant turn the widening swallows is archived.** Its text
  and thinking become a `ToolCallRecord` with the new
  `ToolCallRecordKind::AssistantTurn` discriminator and a synthetic
  id, so `context_tree_query` can still recover what the compaction
  replaced.
- **The tutorial no longer ships `context_window: 3000`.** It is
  below the fixed prompt cost (~9.3k tokens), so every turn compacted
  — a poor example that also hid how rare the bad boundary was. The
  tutorial uses the model's real window; 3000 stays as the
  `uva-context` regression value.

### Fixed (two examples had a tokio feature their code already used)

- `uva-test-support`'s library calls `tokio::time::sleep` for
  `ReplayProvider`'s optional `pace`, but declared tokio with only
  `sync`; the lib did not compile in a build that enabled nothing else
  (it worked only where another crate in the same build happened to
  turn `time` on). `uva-hpc-batch`'s `completer_batch_demo` had the
  same mismatch in its dev-dependencies. Both now declare `time`, so
  `make examples` is green from a clean tree.

### Added (`uva-app-server`: the per-node session state is a cache, not the truth)

- **A parked prompt is a log row.** `SessionEvent::ParkedPrompt`
  (`parked_prompt`) is appended — and flushed — the moment a multimodal
  prompt parks, so a crash between the park and the run no longer loses
  the turn. The row carries the prompt's text, the turn's agent
  selection, its rerun flag and `AttachmentRefRow`s; it never carries
  the bytes. `uva_session::repair::parked_prompts` reads back the
  prompts no run consumed, and a node that acquires the session lease
  re-queues them.
- **Attachment refs are re-readable.** A multimodal part's payload is
  stored in the deployment's `AttachmentStore` first (content-addressed
  by its SHA-256 — `save_image` keeps its image allow-list for images,
  `save_blob` stores audio and files), and the ref carries the `hash`.
  The metadata-only refs a `UserInput` row used to hold could say an
  attachment was there; they could not fetch it. A ref without a hash
  says *why* (`unstored`: never local, or the store's own refusal), so
  "unavailable" is not one undifferentiated state on the wire.
- **A hash is enough to read the bytes back.** `FilesystemBackend`
  resolved a ref through the MIME-derived filename, so a hash-only
  reader (the `GET /attachments/{hash}` route, the parked-prompt
  recovery) found nothing once the writing process's cache was gone.
  A hash-only probe now falls back to the stored file under the
  hash's directory: content addressing is the address.
- **One run consumes one park, by id.** `SessionEvent::ParkConsumed`
  (`park_consumed`) names the `park_id` of the row the run started
  from, and the run appends it before its own prompt row;
  `repair::parked_prompts` folds the pairs by that id. Order was not
  identity: a plain `Prompt` is never parked yet every run writes a
  `UserInput` row, so pairing the n-th consumer with the n-th park let
  an unrelated turn spend a park — and a crash before the park's own
  run then dropped a prompt whose row was intact. A park whose append
  reported failure is still spent by its own run (the id is minted
  before the append), and a `parked_prompt` row written before the link
  existed stays pending rather than being assumed spent.
- **`SessionSink::{read_tail, read_head}`.** `JsonlSessionSink` overrides
  both with bounded reads from the end (or the start) of the file, so a
  reader that wants the last N rows — or the session's first row —
  pays for those rows instead of the whole log. `read()` is unchanged:
  the transcript and the fold still need all of it.
- **`SessionOp::StartPending`.** Starts the next turn if the controller
  already holds work, appending nothing: the operation a takeover uses
  to run a prompt the previous owner had parked.

### Changed (`uva-app-server`: a lease takeover rebuilds from the log)

- **`follow`'s baseline reads the tail.** The route called
  `SessionSink::read()` and sliced the last `FOLLOW_WINDOW` rows in
  memory, so every subscription cost the session's whole history; the
  window now comes from `read_tail`, the header's two whole-log facts
  from `read_head`, and the durable poll resumes by ordinal from the
  same bounded tail. The three folds that are whole-log by nature — the
  context meter, the model window it is priced against, and the resume
  fingerprint — still read the full log **when the window cannot prove
  it holds the last turn** (a turn longer than the window writes its
  `request_header` before the window starts). Folding the window there
  priced the request against the default model and reported an empty
  fingerprint, which the wire reads as "no request envelope". The
  window must also carry a provider usage anchor: without one its fold
  has no baseline and prices the *window's* surface, while
  `GET /sessions/{id}/usage` prices the whole surface — two routes, one
  session, two numbers. The open-interrupt fold follows the same rule,
  from the log rather than from the controller, so a node that only
  *follows* another node's session keeps its `pending_interrupts`.
- **A cold open happens once per session.** Opening the controller is a
  read-modify-write of the cache that also rebuilds from the log and
  re-queues a parked prompt; a per-session gate serialises it, so two
  concurrent requests cannot spawn two controllers that both start the
  recovered turn. The gate is keyed by `(user, session)` and evicted
  with its controller (`AppState::forget_session`, called by `DELETE
  /sessions`), so the map holds one entry per *live* session rather than
  one per session the process ever opened; a waiter whose gate was
  evicted re-checks under the lock and retries on the current one
  instead of proceeding on a detached lock. The wait is bounded: a gate
  held by a wedged open (a filesystem read, a lease acquisition) fails
  the waiter with a diagnostic naming the session instead of blocking
  every request for it forever.
- **A takeover widening read finds a park behind a long run.** The
  parked-prompt read starts at 200 rows and grows until it reaches the
  consumption frontier (a `park_consumed` row, or a legacy `UserInput`
  one) or the log's start, so a prompt queued behind a busy run is
  recovered rather than left behind the window.
- **The controller's state is adopted from the log on takeover.**
  `AppState::get_or_create_session_controller` spawns over the existing
  log, derives the state (`repair` → fold → `SessionState`) and records
  it: a session suspended on the node that died reports the pause on
  the node that took over, and refuses a fresh run for the same reason.
- **A resubmitted resume is a no-op.** The token is derived from the log
  (last durable seq + undecided call ids), so once the decisions are
  rows a second submission of the same token matches nothing. The server
  still remembers no decision for the client (§5.11): the retry is safe
  because it cannot execute the turn twice. Only a *successful* empty
  read counts as "spent" — an unreadable log parks the resume and lets
  the harness validate the token, rather than dropping a resume the
  route already answered.
- **`[tools]` is applied after the plugin bind**, so a config entry can
  mark a plugin's tool deferred or hidden. Applied before the bind the
  registry did not hold the tool yet and the entry was silently skipped
  as unknown.

### Fixed (`uva-app-server`, `uva-plugin-host`, `uva-tool`: refused registrations)

- **The bind runs before any install.** A host already bound to a
  different registry refuses the bind, and installing first registered
  every plugin into the *foreign* registry while `resolved()` listed
  them here. A refused bind now installs nothing: every configured
  entry is skipped with the bind's own reason and `resolved()` is
  empty.
- **`Host::bind` returns its own refusals.** The composition folded
  `Host::diagnostics()`, a process-lifetime vector with no drain and a
  64-entry cap, so a second composition re-reported the first's
  refusals, misnamed them, and could drop them past the cap while
  `resolved()` still claimed the registrations. `bind` now returns what
  *that call* refused, and the composition folds the returned list.
- **A refused plugin is withdrawn whole.** A plugin whose second tool
  the registry would not take kept its first tool registered and
  callable while leaving `resolved()`, so the two lists no longer
  partitioned what was live. A refusal now withdraws the entire
  install — `PluginRegistry::withdraw` drops the handle and frees the
  claims — and the plugin is skipped naming the refused tool.
- **Duplicate static tool names are refused.** Only remote claims went
  through `PluginRegistry`, so two `static` entries could register one
  name and the later shadowed the earlier. Statics run the same claim
  check, against remotes and statics alike.
- **Tool registration is one write guard.** `POST /api/v1/tools` read
  the name and then registered it, so two concurrent requests for one
  name could both pass the check and the second would shadow the
  first. `ToolRegistry::register_entry_if_absent` decides and inserts
  under one write guard, and the route uses it.
- **Core tool names are queryable and creatable.**
  `ToolRegistry::core_owns` exposes the immutability predicate
  registration refuses on, so a caller that learns a name out of band
  can ask the same question; `ToolRegistry::register_core_entry` is the
  way to file a tool as `Core`.

### Added (`uva-plugin-host`: a plugin can be a process)

- **The host half of the dynamic plugin channel.** `uva-plugin-host`
  spawns a plugin with `std::process::Command`, speaks line-delimited
  JSON-RPC 2.0 to it over stdin/stdout — requests correlated by `id`,
  one reader thread per plugin — and turns the plugin's
  `uva/tools/list` descriptors into `uva_tool::Tool`
  implementations registered on the harness's `Host`. The wire
  vocabulary is `uva-plugin-protocol`'s, unchanged.
- **A remote tool's risk is the host's decision.** `RemoteTool::risk`
  returns `RemotePluginConfig::risk`, never
  `Risk::from_annotations(..)`: a plugin that advertises
  `destructive_hint: false` in its descriptor cannot exempt itself from
  the approval policy.
- **Authority has two levels and one default.** `Authority::Reduced`
  (the default) registers tools and — when both the config and the
  handshake announce `observe` — a read-only observer that forwards
  hook events as `uva/event` notifications. A reduced plugin gets
  no gate, no request transformer and no prompt section;
  `RemotePlugin::exposes_gate()` reports `Authority::Full`, which only
  an operator can grant. The gate's decision mapping and the audit row
  for a remote rewrite are in the section below.
- **The deadline covers the write, and expiry kills the plugin.** A
  plugin that stops reading its stdin fills the pipe and blocks the
  host's writer, so a deadline on the answer alone would stall the turn.
  Every request registers a slot with a monotonic deadline; one reaper
  thread per plugin expires the matured slots and **kills the process**,
  which releases the blocked `write_all` and the stdin lock. A
  notification registers a slot for the same reason. A plugin that dies
  fails every pending request with `PluginError::Unavailable`, later
  calls fail immediately, and `install` returns an error instead of
  hanging.
- **The crate is runtime-free.** No tokio in its dependency graph or
  its public API: the plugin is a `std::process::Command` and blocking
  protocol IO runs through `uva_core::blocking::Blocking`
  (`RemotePlugin::with_blocking`). The deadline is not a sleep — a
  delay seam's wait cannot be cancelled, so a reaper built on one would
  either spawn a thread per request or delay shutdown by a whole
  timeout — it is a condition variable in the connection's own reaper
  thread, which also means a constant two threads per plugin whatever
  the call rate.
- **Tool names belong to plugins.** `Host` has no notion of plugin
  identity: two dynamic tools may share a name, and the later entry
  wins it with *its* process and *its* host risk — capability
  substitution. The new `PluginRegistry` remembers which plugin claimed
  which name and refuses the install with
  `HostError::DuplicateToolName` before registering anything.
  `RemotePluginConfig` also rejects unknown keys: it decides `authority`
  and `risk`, so a misspelt key must fail loudly.
- The facade gained the `plugin-host` feature, which pulls `harness`,
  `tool`, `core` and `plugin-protocol` because the re-exported types
  name them. `TransportKind::Http` is part of the config surface but is
  not implemented — an HTTP client would put a runtime in the crate —
  so `install` reports `HostError::UnsupportedTransport` for it.

### Added (the remote authority matrix, and an audit row for a rewrite)

- **A `Full`-authority plugin can veto a tool call; a `Reduced` one
  cannot.** `RemotePlugin::install` registers `uva_harness::Gate`
  behind `exposes_gate()`, the one branch that decides the escalation,
  so a reduced plugin has no path to a veto. The gate answers
  `PluginDecision` over `uva/gate` and the host maps it onto
  `HookDecision` (`Allow` / `Block` / `Ask` / `Terminate`); a variant
  this build does not know reads as `Allow`, because an uninterpretable
  decision must not become a veto. Only `before_tool` is gated — it is
  a tool gate, not a policy engine — and every failure is fail-closed:
  a dead channel, an expired deadline or an answer that is not a
  decision is an `Err`, which the harness already treats as a block.
- **A remote plugin may rewrite a tool result at either authority.**
  The matrix grants "rewrite a tool result" to the default, because it
  changes what one result says rather than whether the call happens,
  so `capabilities.transform` registers a `ResultTransformer` for a
  reduced plugin too. The plugin gets the result as JSON and answers
  with the rewritten result; a rewrite that fails leaves the tool's own
  output and records a diagnostic.
- **A rewrite leaves a provenance row.** When the aggregate tool result
  changes, the harness appends one `SessionEvent::ToolResultTransformed
  { transformers, tool_name, call_id, before, after }` row — log-only,
  never in the model's history, carrying a `sha256` digest of each side
  (over the output's canonical JSON) rather than the payload.
  Transformers run *before* the loop commits the result, so the
  `tool_result` row already holds the rewritten text: the log and the
  model agree, and this row is provenance — which registered
  transformers altered the result on the way to that commit — not
  reconciliation. The change is *measured*: `Host::apply_result_transformers`
  snapshots the output, runs the chain and compares, so
  `ResultTransformer::transform` keeps its `Result<(), String>`
  signature and reports nothing. A run whose host holds no result
  transformer takes no snapshot and writes no row: the loop asks
  `Host::has_result_rewriters` first.
- **`gate` is a capability, not an inference.** `PluginCapabilities`
  gains `gate: bool` (absent key = `false`, so the wire stays
  compatible both ways). The host speaks `uva/gate` only to a plugin
  that is both promoted to `authority: full` and announces the flag, and
  `install` now refuses the promotion when a plugin does not announce it
  — naming the fix — instead of installing cleanly and then blocking
  every tool call in the host, one deadline at a time. Fail-closed stays
  the per-call default; silent at install it does not.
  **Operator action:** a plugin promoted to `authority: full` must now send
  `"gate": true` in its `initialize` reply; a pre-existing plugin that
  predates this key is refused at install rather than silently blocking
  every call. The reverse direction is safe — an older host ignores the key.
- `SessionEvent::ToolResultTransformed` joins
  `KNOWN_SESSION_EVENT_TYPES` (28 typed rows).

### Removed (five dead public items)

- Five public items with exactly one reference each — their own
  definition — are gone: `uva_session::TypedEventRecord::as_value`,
  `uva_session::SummaryCompactor::with_prune_threshold`,
  `uva_provider::WorkspaceConfig::create_default_provider`,
  `uva_provider::OpenAIProfile::openai_compat`, and
  `uva_context::SummarizingContextManager::with_archive`. Nothing
  in the tree called any of them, so nothing in the tree changes
  behaviour:
  - `with_prune_threshold` was the only writer of `prune_min_chars`,
    which made `SummaryCompactor`'s `prune` override a copy of the
    trait default (`prune_tool_results(…, DEFAULT_PRUNE_MIN_CHARS)`)
    once the setter was gone. Both the field and the override are
    deleted; the trait default is the same code.
  - `with_archive` was a second constructor for
    `SummarizingContextManager`. `new` still builds the (empty) archive,
    and `archive()` still hands it out.
  - `create_default_provider` was `create_provider(&self.default_provider)`
    spelled as a second entry point; `create_provider` takes the entry
    name explicitly.
  - `openai_compat` was an `OpenAIProfile` constructor for an arbitrary
    base URL and model id; `openai_default` and `openai_mini` remain.
  - `as_value` was the sole `serde_json` user in `typed_event_sink`;
    the writer takes a `SessionEvent` directly.
- `CacheControlMark::hash_to_u64` is **not** removed but no longer
  public: its ten references all sit in its own module's tests, so it
  is now a `#[cfg(test)]` private function. `ahash` follows it to
  `[dev-dependencies]`, so a consumer of `uva-provider` no longer
  compiles a hasher no production path calls.
- `uva-app-server` is marked `publish = false`: it is the application
  crate and its 40k lines are not a crates.io contract. The **facade
  `uva` stays publishable** — it is the documented `cargo add
  uva` entry point.

### Fixed (harness: a plugin's prompt section reaches the model)

- **`Host::prompt_section` registrations are rendered.** The
  registration API and its `Host::prompt_sections` reader have existed
  since the plugin host landed, but the assembler never consulted the
  host — the reader's only caller in the tree was a host unit test — so
  a plugin's contributed block was accepted and then silently dropped
  before the model ever saw it. A run now snapshots the host's sections
  into its prompt manifest once, at run start, and renders each one as
  a `## title` heading over its body immediately after the identity
  block. Registration order is preserved; a blank section renders
  nothing and does not enter the request-envelope fingerprint; with no
  sections registered the assembled prompt is byte-identical to before.
  A section registered *during* a run appears on the next run, not the
  current one — the prompt stays byte-stable within a run, which is
  what the provider's prefix cache needs.
- **`PromptContext::with_sections` / `PromptContext::sections`** expose
  that manifest to a caller that assembles a prompt itself; a
  per-dispatch `AgentInput::prompt_context` now carries the host's
  sections too, so a dispatch-level manifest cannot drop a plugin's
  contribution. The internal `prompt::resolve_prompt_context` resolver
  is deleted — the run manifest replaces it in every one of its three
  callers.

### Changed (server: one tool registry and one plugin host per node)

- **Plugins installed at boot are reachable from chat.** The runtime
  composition used to install into the boot agent's `Host` while every
  per-run session agent was assembled with no host and its own
  `ToolRegistry`, so a configured plugin's tools never reached the
  agent that serves a conversation. `AppState` now owns one `Host`;
  the composition installs into it, and every agent the server builds
  — boot, per-run session, eval, dynamically registered — receives a
  clone. A host clone shares its registrations, so nothing is spawned
  per run.
- **`AppState::tool_registry` is `Arc<ToolRegistry>`.** The
  `ToolRegistry` already carries its own interior lock, so the outer
  `Arc<RwLock<_>>` only forced agents to clone a snapshot. One handle
  per node is now threaded to every agent, and a plugin's tool
  registered at boot is visible to every later run.
- **`Host::bind` is idempotent by identity.** Binding the same
  registry again is a no-op; binding a *different* one records a
  `RegistryAlreadyBound` diagnostic and keeps the original binding,
  because the existing tool tokens name entries in it — moving the
  host would strand them.

### Added (a runtime-neutral delay seam)

- **`uva_core::Sleeper`** — the delay a timeout needs, with no
  runtime in the signature: `sleep(&self, duration) -> BoxFuture<'static, ()>`.
  Arming a timer is the one thing a library cannot do for its caller
  without naming a runtime, so the seam is a trait the consumer fills:
  `ThreadSleeper` (a std thread plus a oneshot channel) and
  `FixedSleeper` (resolves immediately, for tests) ship in-tree, and
  the trait is object-safe so a component holds an `Arc<dyn Sleeper>`.
  The wait starts on the future's first poll — the laziness
  `tokio::time::sleep` has — so a timeout that is abandoned before
  it is ever polled starts no timer at all.

### Added (a runtime-neutral blocking seam)

- **`uva_core::Blocking`** — the offload a blocking call needs,
  with no runtime in the signature: `run(&self, f) -> BoxFuture<'static, ()>`.
  Moving a file write off the caller's thread is the one thing a
  library cannot do for its caller without naming a runtime, so the
  seam is a trait the consumer fills: `ThreadBlocking` (a std thread
  plus a oneshot channel) ships in-tree, and the trait is object-safe
  so a component holds an `Arc<dyn Blocking>`. The closure starts on
  the future's first poll, and a closure that has to report a
  `Result` carries it out through a channel of its own — the
  awaitable counterpart to `Spawner::spawn_blocking`, which is
  fire-and-forget.

### Changed (provider and steering carry no tokio dependency)

- **`uva-provider`'s `tokio` is optional**, enabled by the
  `anthropic` / `openai` features — the adapter code (the SSE pump,
  the retry backoff) is the only production code that awaits a runtime
  primitive. A build with `default-features = false` now compiles no
  tokio at all, and `uva-provider` / `uva-steering` no longer
  turn on `uva-core`'s `tokio-util` feature from their normal
  dependency list: the `CancellationToken` bridge there is a
  test-time convenience in both crates.
- **`uva-app-server` cancels with `uva_core::AtomicCancelToken`**
  instead of `tokio_util::sync::CancellationToken` — the run-stream
  factory, the controller's in-flight run handle, and the binary's
  graceful-shutdown token. The server drops `tokio-util` entirely; the
  runtime-neutral token implements the same `CancelToken` trait and
  `Arc<AtomicCancelToken>` coerces where `Arc<dyn CancelToken>` is
  expected.

### Removed (provider: two dead retry helpers)

- **`uva_provider::retry_with_backoff` and
  `uva_provider::retry_with_retry_after`** — both had zero call
  sites; their only references were the `lib.rs` re-exports.
  `retry_with_classification` stays, now compiled only with an adapter
  feature (its only callers) and reachable as
  `uva_provider::retry::retry_with_classification` — it leaves the
  crate root so the retry vocabulary keeps its one-line re-export. The
  same-named helper in `uva-resilience` is unrelated and unchanged.

### Changed (`uva-session` is runtime-free)

- **`uva-session` compiles no tokio.** Its normal dependency list
  no longer names a runtime: the `tokio::sync::Mutex` over the file
  handle is `futures::lock::Mutex`, and the three `spawn_blocking`
  fs calls (append, batch append, read) go through the new
  `uva_session::fs::FsBackend` over the `Blocking` seam. The
  search index's store walk takes the same seam. Both default to the
  runtime-free implementation, so the sink works on a bare `futures`
  executor; a host on a runtime swaps its own in with
  `JsonlSessionSink::with_blocking` / `JsonlSessionSearch::with_blocking`
  / `SessionRegistry::with_blocking`. tokio remains a
  **dev**-dependency (the crate's own tests are `#[tokio::test]`).
- **`SummaryCompactor`'s timeout is armed by a `Sleeper`** instead of
  `tokio::time::timeout`: `with_sleeper(Arc<dyn Sleeper>)` races
  `cancel.cancelled()` against `sleeper.sleep(timeout)` with
  `futures::select_biased!`, keeping the `Cancelled` / `Timeout`
  outcomes and the cancellation-first priority. The default
  `ThreadSleeper` needs no reactor, so the deadline now works with
  `with_timeout` on any executor.
- **The server installs its own tokio adapters.** `uva-app-server`'s
  `spawner` module gains `ServerBlocking` and `ServerSleeper` beside
  `ServerSpawner` (same reason: R121 keeps the server on the facade,
  so the ~10-line adapters are local rather than a facade feature),
  and every site that builds a sink, the search index, the registry
  or the manual compactor installs them. `ServerSleeper::sleep`
  anchors its deadline on first poll, as the trait requires.

### Removed (`uva::session::manager::InputQueue`)

- **`InputQueue` is now the server's type**
  (`uva_server::session::input_queue::InputQueue`), not the
  session crate's: it is the application's bridge between
  `submit(Prompt | Steer)` and its run loop, not a property of a
  durable log. The facade path `uva::session::manager::InputQueue`
  is **gone, not aliased** — no alias, no shim. `uva-session` drops
  the type and its `PendingEntry`, and the queue is held by
  `AppState` (one per process, cloned into every controller) so a
  route or a test sees the same queue the controller drains. The
  queue is backed by `futures::lock::Mutex`.

### Changed (the web client reads `follow`; the old SSE route is gone)

- **The chat page's transcript is `GET
  /api/v1/sessions/{id}/follow`.** Every open replaces the window from
  the baseline (header, durable window, cursor, live projection, context
  meter, pending interrupts, request fingerprint), then applies rows past
  the cursor. The local `uva.messages.*` cache and the
  `uva.streamIndex.*` replay cursor are gone: a reconnect is a
  replace, not a splice, so partial text can neither duplicate nor go
  missing.
- **`GET /api/v1/chat/sessions/{id}/messages/stream` is removed**, not
  aliased. `routes::chat::stream_messages` and its replay/cursor helpers
  are deleted; the route table no longer mounts either path
  (`/api/v1/chat/...` or the `chat_router()` builder). The integration
  tests that only exercised it are removed, and the surviving route test
  asserts the old path answers `404`.
- **Composer: `Send` (queue) and `Steer` (inject).** A steer shows
  `accepted` (durable) → `injected step N` → `consumed`, or `next turn`
  when the run settled first; `Stop` returns un-injected steers to the
  editor. A queued turn shows as an outbox chip until the run logs its
  row.
- **Approval card bound to the held call.** Approve-once / reject per
  interrupt; the resume posts every decision and the token rebuilt from
  the follow baseline (`cursor`, pending call ids, request fingerprint).
  The composer is disabled while a decision is outstanding, and a
  rejection reads back as the model-visible `is_error` tool result.
- **Compaction is visible.** The durable `compaction` row renders a
  timeline marker with its summary and the shadowed surface/seq range;
  the footer shows the session's context occupancy plus its own token
  totals from `/usage`, and offers a manual `Compact` action.
- **`POST /api/v1/chat/sessions/{id}/messages` accepts attachment-only
  turns** (`text` may be empty when `attachments` carry the turn; a
  request with neither is `400 empty_message`), and a named agent now
  rides the prompt op, so the composer's agent selection is honoured for
  plain-text turns instead of falling back to the deployment default.

### Added (follow, resume, compact, and session usage)

- **`GET /api/v1/sessions/{id}/follow`** opens with a complete
  baseline — header, the last durable window, its cursor, the live
  projection of the in-progress turn (`live_seq`), the context meter,
  the open interrupts and the request-envelope fingerprint — and then
  streams `row` frames (durable, `seq`) and `live` frames (transient,
  `live_seq`). A reconnect replaces the client's window from the new
  baseline; a durable row for a turn supersedes that turn's live
  projection. The stream stays open across turns.
- **`POST /api/v1/sessions/{id}/resume`** takes
  `{ resume_token, interrupts: [{ id, status, payload }] }`. A
  `resolved` call executes once; `cancelled` / `rejected` writes the
  model-visible `is_error` tool result and no dispatch intent. The
  decisions must cover every open interrupt: partial coverage is `422`,
  a stale token or a changed envelope is `409`, each with a diagnostic.
- **`POST /api/v1/sessions/{id}/compact`** runs one manual compaction
  pass through the configured policy and returns the committed range
  and summary metadata; a running session, a suspended one, or a
  deployment with no compaction policy is `409`.
- **`GET /api/v1/sessions/{id}/usage`** reports the session's own
  token/cost fold (not the process-wide counters) plus turns and the
  next-request context projection.
- **Harness: per-call resume decisions.** `ResumeDecision` /
  `ResumeStatus` and `AgentInput::resume_decisions(token, decisions)`;
  `agent::approval::{open_requests, check_coverage,
  latest_request_snapshot_hash}` expose the log-derived paused state
  and token integrity to hosts. `SessionState::AwaitingApproval` is
  derived from the log, and `maybe_start_run` refuses a fresh run while
  it holds.

### Added (compaction seam: a summary that replaces a span, with provenance)

- **`uva_session::Compactor`.** `plan` decides which projected
  messages to shadow and `summarize` produces the replacement; the trait
  owns no provider, so a deployment can summarise with a cheap model
  without teaching the loop about it. `SummaryCompactor` is the default:
  it keeps a balanced recent tail (never splitting a `tool_use` from its
  result), prunes oversized tool results **before** the summary call,
  and asks its provider once.
- **The harness owns provenance.** A plan names a message range; the
  loop fills `shadowed_seqs` from the fold, so the durable `Compaction`
  `Replace` cites exactly the rows the model saw — each one before the
  compaction row and no later than the previous surface maximum.
- **Optional, boundary-only.** `ControlCaps.compactor` (default `None`)
  and `ReActAgent::with_compactor`. Only a writer-backed run compacts,
  only at a step boundary and never while suspended. A summariser
  failure warns and continues without compaction; the turn is not lost.
  The context manager's request-local pruning is unchanged and
  independent.
- **Live events.** `SystemEvent::CompactionStarted` /
  `CompactionFinished` (`kind`, `size_hint`, the shadowed indices and
  the first/last shadowed log ordinal) let a UI mark the timeline and
  address the summary's span; the log-only `compaction_start` /
  `compaction_summary` / `compaction_end` audit rows carry the rest.
- **The summary row names its author.** `CompactionOutcome` gained
  `provider` / `model` / `usage`, and the durable `compaction` row
  records them beside the summary and the shadowed token count. The
  harness always writes its own bound range and seqs, so a compactor
  cannot cite rows the fold did not hand it; the outcome's echo is
  advisory. A pass whose only shadowable row is the previous summary
  is skipped.

### Changed (the compaction lifecycle outcome is named for its lifecycle)

- **`uva_session::CompactionOutcome` → `CompactionLifecycleOutcome`**
  (breaking). `CompactionOutcome` now names the seam's result
  (`summary`, `shadowed_seqs`, `shadowed_tokens`); the old enum still
  serialises as `committed` / `failed` / `interrupted` and still drives
  `CompactionEnd`.

### Changed (`DelegatingTool`: name-keyed dispatch and a host registration)

- **`SyntheticTool` → `DelegatingTool`** (breaking): the seam for a
  plugin that runs work *inside* the loop was never "synthetic" — it
  delegates. `SyntheticCall` → `DelegationCall`, the module is now
  `agent/re_act/delegation.rs`, and `ReActAgent::with_synthetic_tool` →
  `with_delegator` (same fallible duplicate-name contract).
- **Dispatch is a name-keyed table.** `claims(name)` — a linear scan of
  every installed plugin on every call — is gone: the loop indexes the
  advertised definitions once per run, so a run with several delegators
  pays one map lookup.
- **`DelegationCall` carries `context`.** A delegator receives the same
  `uva_tool::Context` (workspace root, session id, output bound) a
  registry tool gets; `emit` / `cancel` / `depth` / `subagent_writers`
  are unchanged.
- **`DelegatingTool::mode()` / `annotations()`.** `mode()` defaults to
  `Sequential`, and the batch bucketer now reads it for a delegated name:
  before, the registry knew nothing about these names and filed them under
  its missing-tool default (also sequential), so a delegator could not
  declare `Parallel` at all. `annotations()` applies to the definitions
  that carry none of their own.
- **`Host::delegator`.** A plugin installs a delegator through the host
  and gets the same reversible registration as observers, gates and
  tools: dropping the `Registration` withdraws it from the next run.
  Installing one delegator through both the agent and its host leaves
  one entry.

### Changed (every strategy persists its turn, and the prompt is durable first)

- **Every reasoning strategy writes its assistant turn.** The write is
  one `EventSink` method (`write_assistant_turn`): the ReAct loop
  calls it where it always wrote the row, and the chain-of-thought,
  best-of-N and plan-and-execute strategies call it where they publish
  the model's text. Before, only the ReAct loop wrote the row — a run
  driven by any other strategy streamed a whole answer and left the
  session's log without it.
- **The run's prompt is on disk before the model sees it.** The loop
  flushes the session's writer at the step boundary, before the
  provider call, so the user's message (and a delegated child's own
  enter header and prompt) survives a crash in the window between the
  prompt and the answer. Before, nothing was flushed until the first
  row of the reply.
- **One `request_header` row per run.** The harness stamps it — the
  provider that ran, the model, and the hash of the tool set the model
  was actually offered — and the server no longer writes a second one
  before the run's prompts.
- **`make check-single-writer`.** A gate over the workspace's
  production code: outside the session crate's writer, no
  `SessionSink::append` / `append_many` call site. The invariant this
  round is named after now has something holding it.

### Changed (one assistant turn is one durable row)

- **An assistant turn is one row, not one row per delta.**
  The harness writes the assembled turn — its text and its tool calls
  together — as a single `assistant_message` row, and a tool's outcome
  as a single `tool_result` row. The per-delta `Model` rows are gone
  from the log: the deltas stay on the event stream, where the live
  rendering reads them, and only the finished turn is durable. Before,
  a reply of forty deltas left forty rows, and a resumed session read
  them back as forty assistant messages — the model saw its own last
  answer in pieces.
- **A tool result row says whether the tool failed.** The row's `data`
  is the tool *message* — the tool's output and the id of the call it
  answers — and the message carries no failure flag of its own, so the
  flag rides beside it as `is_error`. A row without the flag is a tool
  that returned, and the wire keeps the shape it had before.
- **The web reads the new rows.** The chat page's history projection
  and the session detail page understand `assistant_message` and
  `tool_result`, so a reloaded session shows the same assistant turn
  the live view showed: one message, its text complete, and its tool
  calls as tool blocks that carry their results.

### Added (the writer is handed to a run, and to a run's children)

- **`AgentInput::log_writer` and `AgentInput::subagent_writers`.**
  A run can name the session's writer for its own rows and the source
  that opens the sessions its delegated children run in, so a child
  writes its model facts into its own log through its own writer
  instead of the parent's router copying them over.
- **`SubagentWriters` / `ReActAgent::with_subagent_writers`.** The
  harness-side seam a host implements to open a child session: the
  delegation plugin hands the input down run by run, so every nesting
  level resolves its own child session.
- **`LogWriter::append_request_header`.** The writer stamps the run's
  request header and remembers the envelope, so the row says
  `initial` / `change` / `same` instead of guessing.

### Changed (the row's own `seq` is the ordinal)

- **A reader takes the ordinal from the row.** The fold, the ledger's
  seed and the writer's counter continuation read the `seq` a row
  carries instead of counting rows in the file: a stamped log is
  numbered by the numbers its writer stamped, and a compaction
  checkpoint's `source_event_seqs` cite real earlier rows. A log that
  carries no `seq` at all — an old one — is still numbered by
  position, exactly as before. Deciding row by row would mix the two
  schemes and produce duplicate ordinals.

### Changed (the context crate's write points go through the session's writer)

- **The memory tier writes through the session's writer.**
  `SessionMemory::new(writer)` replaces `SessionMemory::new(sink)`: the
  tier holds the session's `LogWriter`, reads the log through it, and
  writes the `"Message"` envelope through it. The row now carries the
  ordinal the writer handed out and the writer's wall clock — before,
  the tier's row carried no `seq` at all, so a log that held one fell
  back to positional numbering for the whole file.
  `SessionMemory::sink` is gone: the sink handle is the writer's now.
- **The crash repair writes through the session's writer.**
  `repair_session(writer)` replaces `repair_session(sink)`: the closers
  it appends carry the writer's ordinal and land in the writer's batch,
  so a repair is one flush instead of one append per closer. The
  repair still only appends — it never truncates the log.
- **The writer can read the log it owns.**
  `LogWriter::read` returns the session's rows in the order they were
  written, so a caller that holds only the writer — the memory tier
  and the crash repair — can still project the log.

### Changed (the harness's structural rows go through the session's writer)

- **The harness writes its structural rows through the session's one
  writer.** `EventSink` now holds an `Option<Arc<uva_session::LogWriter>>`
  and nothing else: `EventSink::new(tx, log_writer)` takes two
  parameters, and `emit_typed` writes the row through the writer's
  non-waiting path (`append_event_nowait`) — the loop must not block on
  a queue, and a row that does not fit waits in the writer's spill
  instead of being dropped. Without a writer the row goes nowhere: a
  structural row is a log fact, and a run with no writer has no log.
  `ReActAgent::with_log_writer` now takes the session crate's writer;
  the harness's own writer and its `log` module are gone.
- **The typed-event channel is gone.** `uva_session::TypedEventSink`
  and `TypedEventReceiver` are removed — the channel was fire-and-forget,
  so a full buffer dropped a structural row and left a hole in the
  log's ordinals that no later reader could repair. The writer replaced
  it. `TypedEventRecord` and `CompactionRecordView` stay: they are
  public API, and the latter is part of the server's own surface.
- **`Durability` is gone.** The flush cadence is the caller's business:
  the harness flushes the writer at the step boundary — before each
  provider call — and again when a tool result's row is written, before
  that result's event goes out. (The assistant turn's deltas and its
  `ModelDone` are emitted as they stream; its row is written and
  flushed right after the turn completes.) The server's own rows are
  queued and land on the same flush. With the enum gone,
  `uva::harness::{LogWriter, Durability}` no longer has a place in
  the public surface; the writer's path is
  `uva::session::LogWriter`.
- **The server writes the agent's structural rows through the same
  writer.** `AgentRunConfig::typed_event_sink` becomes
  `AgentRunConfig::log_writer`, and the run task installs the session's
  writer on the agent instead of draining a channel at the end of the
  run.

### Changed (one writer per session: the registry hands it out, the checkpoint writes through it)

- **The session registry hands out one writer per session.**
  `SessionRegistry::writer(user_id, session_id)` returns the session's
  `LogWriter` — cached per session, next to the sink it wraps, and
  dropped with the session. One session has one writer, so one ordinal
  source and one flush cadence: two writers over one sink would each
  hold their own queue and their own flush timing, and the rows they
  write could reach the log in an order their ordinals do not match.
- **The compaction checkpoint writes through that writer.**
  `CompactionCheckpoint::new(writer, ledger)` replaces the channel it
  used to hold. Its two callbacks are synchronous — the summarising
  manager fires them from the agent's own task — so they take the
  writer's non-waiting path: a row that does not fit in the writer's
  queue waits in the writer's spill and the next flush writes it. The
  checkpoint's rows are no longer dropped by a full channel, and the
  ordinal they carry is the one the reader reproduces.
- **The crash-repair closers no longer number themselves.**
  `interrupted_turn_closers` used to derive each closer's ordinal
  from the log's last sequence number — a second ordinal source, and a
  third once the writer existed. The closers now carry `seq: 0`, and
  the appender that writes them stamps the ordinal the sink hands out.

### Changed (harness v2 foundation: the log owns the ordinal)

- **The sink owns the ordinal; the writer stamps it.**
  `SessionSink::alloc_seq` hands out the next ordinal and
  `SessionSink::append` no longer assigns one (`-> Result<(), _>`).
  Every appender on a session — the server's run log, its
  out-of-band `Feedback` rows, the shutdown marker, the harness's
  `LogWriter` — draws from that one counter, so a second writer can
  no longer collide with the run's ordinals and make a compaction
  checkpoint's provenance unresolvable. `append_with_key` and
  `append_many` follow the same shape.
- **A writer stamps each row's ordinal.** (Superseded below: the
  writer this bullet announced first lived in `uva-harness` and
  selected its flush cadence with a `Durability` enum. The writer
  moved to `uva-session` — one per session, handed out by the
  registry — its cadence is the caller's, and `Durability` is gone.
  Read the "one writer per session" and "the harness's structural
  rows" sections for the shipped shape.)
- **Dead harness APIs removed**: the `RunInbox` family
  (`RunInbox` / `MpscInbox` / `RunInboxHandle` /
  `with_run_inbox` / `run_inbox`) and the loop's steering-drain and
  follow-up-revival wiring it fed; `ReActAgent`'s
  `with_tool_registry` / `with_model_hint` / `with_clock`;
  `SystemEvent::SteeringInjected`, `SteeringSource` and
  `WarningKind::Inbox`. `AgentCaps::defaults` is crate-private
  now; `CheckpointSnapshot::at`, `AgentInput::to_message` and
  `StrategyRegistry::names` shrank to crate-private, and
  `Checkpointer::list_keys` became an inherent
  `InMemoryCheckpointer` method.

### Changed (harness refinement: spawn-free dispatch, zero-feature paradigm crate)

- **`uva-tool` dispatch is spawn-free.** `ToolRegistry::run_stream`
  resolves the whole batch under one read lock, then drives the
  per-tool drains from a single `flatten_unordered` stream — no
  per-call task, channel, or spawner. Dropping the stream still
  cancels every in-flight call; the panic / missing-`Result` /
  progress contracts are unchanged (tests pin them). The registry
  no longer has `with_spawner`; `uva-tool`'s `[dependencies]`
  names no runtime crate at all (tokio moved to
  `[dev-dependencies]`; `make check-no-runtime` asserts the
  direct-deps discipline).
- **`ToolRegistry::resolve_dispatch`** is the single source of the
  hidden-refusal + argument-validation decision. The harness loop's
  tool routing (`route.rs`), `run_stream`, and `dispatch_one` all
  dispatch on it; refusal rendering lives on
  `DispatchRefusal::tool_output`.
- **Removed `hpc-coalesce`.** The `coalesce` feature
  (`CoalescingRegistry`, `ToolDescriptor::non_coalescible`) had no
  consumers beyond its own example; the paradigm crate is
  zero-feature now and the facade drops `hpc-coalesce`.
- **Dead harness APIs removed**: `from_name` (use
  `StrategyRegistry::register` + `resolve`), `ReActAgent`'s
  inherent `descriptor()` / `clock()` getters and
  `StrategyRegistry::get` (the `Agent` trait and `resolve` cover
  them), and the output-spill cleanup task (`CleanupTask` /
  `start_cleanup_task` / `OutputBound.retention` /
  `cleanup_interval`); the spill writer uses blocking `std::fs`.
- **Sampling and projection share one build.** Both request
  constructors go through `build_completion_request`; the loop
  collects every synthetic definition once per run (fingerprint +
  projection share the vec).
- **`uva-app-server` assembles agents in one place.**
  `AgentAssembly` (private module) is the only `with_*` chain and
  the only `TaskDelegator` install site; boot's default member, the
  session run factory, dynamic agent registration, and the eval
  agent all build through it. `encode_event` serializes once
  (`to_value`) instead of `to_vec` + `from_slice`.

### Added (durable workflows / evals, real eval runner)

- **`WorkflowStore`** (`uva-workflow`, re-exported): atomic JSON
  persistence for `WorkflowSpec`s under `<workspace>/workflows/` —
  `temp + sync_all + rename` writes, lazy directory creation, and a
  `load` that skips unreadable or malformed files with a `warn!`
  (the `ScheduleStore` honest-recovery posture). Ids that could not
  name a plain file (`../`, separators, empty) are refused.
- **`EvalStore`** (`uva-eval`, re-exported): the same contract for
  `EvalSuite`s under `<workspace>/evals/`, keyed by suite name.
- **`uva-app-server` workflows / evals are durable now.**
  `WorkflowsState::build(&workspace_root)` / `EvalsState::build(..)`
  load from the stores at boot (a load failure degrades to empty with
  a `warn!`), and `create` / `replace` / `remove` write through under
  the state lock — `remove` now returns `Result<bool, String>` and the
  route / self-MCP callers map the error. A restart (or a second node
  over a shared filesystem) sees the same catalogue.
- **Eval runs drive the real harness.** The `StubAgent` echo path is
  gone; `HarnessEvalAgent` (server `state` module) assembles a
  one-shot `ReActAgent` per case from the live provider + a snapshot
  of the tool registry, drains the run to its final assistant text,
  and `EvalRunner::run` scores it. `POST /api/v1/evals/{name}/run`
  returns a real agent-driven report.
- **Server agents detach on the tokio runtime.** Every server-side
  `ReActAgent` assembly (the run factory, the registry default, the
  eval agent) installs a local tokio-backed `Spawner`
  (`uva_server::spawner::ServerSpawner`). The harness default
  (`InlineSpawner`) runs the loop on a bare std thread with no
  reactor, so any tool awaiting `tokio::fs` (`read` / `write`)
  panicked mid-run; that failure was latent because most tests never
  executed a file tool.

### Added (multi-node cluster bus)

- **`[cluster]` config + `SessionBus` seam.** `mode = "local"` (default)
  is the in-process `LocalBus`; `mode = "redis"` (server feature
  `cluster-redis`) is `RedisBus`: `SET NX EX` session leases with Lua
  compare-and-renew/release, and pub/sub for session frames +
  control frames. Delivery is best-effort by design — the durable
  truth stays the shared workspace JSONL, and an SSE client recovers
  with its `from` cursor.
- **Session ownership is leased.** The node holding
  `"{ns}:lease:session:{user}:{session}"` runs the controller and
  renews at TTL/3. A node that loses the race gets a remote
  `SessionController` handle (`is_remote()`): submit → control frame,
  event tail → bus → local broadcaster (so the SSE handler is
  unchanged). A crashed owner's lease expires and any node's next
  request takes over — including upgrading a cached remote handle —
  and rebuilds the session from disk.
- **Cluster-wide tool-registry mutations.** `POST/DELETE /api/v1/tools`
  publish an `InvalidateTools` control frame; one process-wide
  dispatcher applies peer mutations to the local registry
  (`ToolEntry::dynamic` is complete, so a peer replays it verbatim).
  `ForwardOp`/`CancelSession` frames route to the local owner.
- **Docs**: `crates/uva-app-server/README.md` gains the multi-node
  deployment contract (shared filesystem, lease/TTL, per-node
  `/metrics` and usage, no background scheduler tick today).

### Added (keyless demo provider)

- **`[providers.<name>] type = "demo"`** (alias `"fake"`, requires the
  server's `test-utils` feature): a keyless echo provider for demos
  and CI — no network, no API key. It echoes the user's turn and skips
  the harness's injected runtime-context snapshot. The `config.yaml`
  bridge now honours the typed provider fields (`type`, `api_key_env`,
  `default_model`, capability flags) instead of silently inferring
  `openai` and demanding a key.

### Fixed

- **Server-side agents detach on the tokio runtime.** The harness
  default (`InlineSpawner`, a bare std thread) gave the loop no tokio
  reactor, so a tool awaiting `tokio::fs` (`read` / `write`) panicked
  mid-run. All server assembly sites (run factory, registry default,
  eval agent) install the local `ServerSpawner`. Latent before because
  no test had executed a file tool through the real run factory.
- **Chat submissions no longer pin the default provider.** A provider
  pin holds a live handle and cannot cross nodes; only an explicit
  `model` selection pins now, so a submission to a remotely owned
  session runs the owner's configured default instead of failing.

### Changed (harness refinement: touched projection home, cheap checkpoints, `after_step` hook)

- **`TouchedPair` / `touched_target` moved to `uva-context`** and are
  re-exported from it: the `read` / `write` projection feeds
  `CompactionDetails`, so the compaction layer owns it and the loop no
  longer knows tool names.
- **`CheckpointSnapshot::messages` is `Arc<Vec<Message>>` now** — persisting
  a snapshot of a live transcript is a refcount bump instead of a full
  deep copy per persist. `CheckpointSnapshot::at` takes
  `impl Into<Arc<Vec<Message>>>`, so existing `Vec` callers compile
  unchanged.
- **New hook event `after_step`** (pi `finishTurn` parity):
  `HookEvent::AfterStep { agent_name, run_idx, iteration }` fires at the
  end of each loop iteration, after the iteration's tool results are
  committed and before the next iteration is sampled. `Terminate` ends
  the run with `SessionEndReason::Error(reason)`; `Block` is meaningless
  after a step already ran and is treated as `Allow`. Zero-hook runs keep
  the `is_empty` fast path.

### Changed (harness refinement: module convergence, one assembly path, sync tool lookup)

- **AfterTool now carries the real `call_id`** — the success path used
  to hard-code an empty string, so a hook observer could not correlate
  the observation with the `ToolUse` it belongs to (the veto path
  already passed it). Regression-tested in `steering_hooks.rs`.
- **Single-shot helpers share the ReAct transcript assembly.**
  `prepare_messages_for_shot` / `single_shot_request` now build their
  conversation through the same `base_transcript` the loop sends: an
  empty system prompt no longer injects a placeholder system message,
  and single-part user input is `Content::Single` instead of a
  one-element `Content::Multi`.
- **`ToolRegistry::get` is an inherent sync method** (one read lock +
  one clone, LIFO top). The `uva_core::registry::Registry` impl
  delegates to it, and the ReAct dispatch path no longer boxes a
  future per tool call. In-tree callers migrated off the async form.
- **Module layout converged** (no public paths changed): `agent/run/`
  flattened to `agent/run.rs` (tests inlined), the projection module
  moved from `loop/` up to `re_act/` and `tool_definitions` no longer
  takes the loop struct, per-tool-call file-activity tracking moved to
  `loop/touched.rs`, and the `text_parts` helper merged into
  `loop/tools/mod.rs`.
- **Dead code removed**: `RunState::started_at` (written, never read),
  `ReActAgent::strategy()` / `ReActAgent::checkpointer()` (zero-
  reference getters), the `Arc<T>: ReasoningStrategy` blanket impl
  (every consumer coerces explicitly), the per-tool-call wall-clock
  read the after-hook ignored, the per-sample provider timing nothing
  consumed, and the double "no dispatch path claimed it" fallback in
  `commit_all_results` (counter renamed `unclaimed` to say what it
  counts).

### Removed

- **`Agent::run` no longer takes a `scope` argument** (breaking).
  The signature is now
  `run(&self, input: AgentInput, cancel: Arc<dyn CancelToken>) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send>>`.
  Migration: drop the third argument — `agent.run(input, cancel, AgentScope::new())`
  becomes `agent.run(input, cancel)`. Every in-tree call site
  (server, `task`, the three strategies, the facade, both examples,
  both consumer crates, `MINIMAL.md`) is migrated in the same change.
- **`AgentScope` deleted.** It had zero non-test callers, and its
  three setters (`with_synthetic_tool` / `with_tool_restriction` /
  `with_steering`) were literal duplicates of `ReActAgent`'s, which
  also ran `validate_synthetic` twice for the same install path.
  Per-run variation is unchanged in practice — `uva-app-server`
  rebuilt the whole `ReActAgent` per request anyway.
- **`AgentHandle` / `DetachedAgent` deleted.** The deferred-run pair
  had zero consumers in the workspace and no equivalent in either
  reference design (pi spawns a separate process for a subagent;
  dsh has no detached handle). `Agent::run`'s event stream is the
  single run entry point; `RunInbox` / `MpscInbox` stay for
  steering a live run.
- **The background prefetch seam deleted** (`ReActLoop::maybe_prefetch`).
  It called `ContextManager::frozen_segments`, whose only
  implementation returns `&[]`, so every iteration allocated a
  `Vec` copy check to reach a guaranteed-empty result. The
  `ContextManager` trait methods stay in `uva-context` (removing
  them would break every impl); the harness simply stops calling them.
- **Duplicate accessors collapsed.** `AgentCaps::{model_config,
  system_prompt}` and `ReActAgent::{model_config, system_prompt}`
  were one-line delegation with no state of their own; callers read
  `caps.model.provider.model_config()` and
  `caps.model.prompt_context.assemble(&caps.model.descriptor)`
  directly. `AgentInput::new_with_history` is gone — `multi_with_history`
  is the constructor and `history` delegates to it.
- **EventSink gains `end(reason)` and enforces one terminal event
  per run** — see the Changed entry below.

### Changed

- **One spelling per lifecycle event on the `EventSink`.** The ReAct
  loop emitted `SessionStarted` / `SessionEnded` itself while the
  three strategy crates went through `EventSink::begin` / `finish` /
  `fail`, so a consumer's pairing depended on which path produced it.
  The loop now uses `sink.begin(&cancel)` in `drive_setup` and
  `sink.end(reason)` in `finalize`, and `EventSink` carries an
  `ended` flag so a run publishes exactly one `SessionEnded` no
  matter how many code paths try.
  **Behaviour change:** a token that already fired when `Agent::run`
  is called now ends the session at setup — `SessionStarted` then
  `SessionEnded { Cancelled }` — instead of running setup and failing
  at the first in-loop cancel check. No iteration starts, so no
  `request_header`, no `SessionInterrupted`, no provider call. A
  *mid-run* cancel is unchanged: the loop reports
  `SessionInterrupted` and then the terminal event.

### Fixed

- **`with_output_schema` installs the `structured_output` tool
  again** (behaviour regression): the setter only stamped
  `descriptor.output_schema` and left the model with nothing to
  call, so every run that advertised structured output silently got
  no tool. The two-step form
  (`register_output_schema_tool(schema)` +
  `with_output_schema(schema)`) had become the only working recipe,
  which is what `README.md`, `MINIMAL.md`, `AGENTS.md` and the two
  assembly examples all documented while no production caller used
  the second step. `with_output_schema` now builds the tool, stamps
  the descriptor from the same value, and registers it; a registry
  that already holds `structured_output` (two agents sharing one
  `ToolRegistry`) keeps the first schema and logs the conflict, so
  the setter stays infallible and chainable. `register_output_schema_tool`
  is removed — the two-step form *was* the bug.

### Fixed (harness refinement: dispatch race, dead code, loop cohesion)

- **Synthetic dispatch no longer hangs on a leaked `emit` sender.**
  `try_synthetic` awaited the executor *and* the close of the
  plugin's event channel (`join`); a plugin that leaks its
  `SyntheticCall::emit` sender into a task outliving `execute` —
  exactly what a delegator spawning subagents does — kept the
  channel open forever and hung the dispatch. The executor now
  races the channel and, once resolved, drains what it buffered
  with a non-blocking poll loop. Regression-tested in
  `tests/synthetic_emit_leak.rs` (fails by hang before the fix).
- **`ToolSurfacePolicy::max_visible` no longer counts MCP
  self-management tools.** The cap ran before the self-tool filter,
  so deferred `mcp__self__*` entries consumed the budget and a
  server with MCP self servers plus a capped surface advertised
  *zero* tools to the model (`boot_wiring_applies_the_tools_section`
  was already failing on main with an empty projection).
  `visible_tool_names` now skips self tools before spending budget,
  matching the documented "unadvertisable tools take no cap slot".

### Changed (harness refinement, continued)

- **One hook-dispatch helper** (`loop/hooks::dispatch_hook`) replaces
  the eight inline `if hooks.is_empty() … dispatch` sites in
  prepare / sample / guard / finalize. The event closure stays lazy,
  so a zero-hook run still allocates nothing; `Allow` is the
  no-hook outcome every site already assumed.
- **`checkpoint` takes `context_applied: bool`** — the loop no longer
  reconstructs a `Step` just so the checkpoint can read one flag off
  it. Stream-cancel now goes through `ReActLoop::cancelled` (same
  `SessionInterrupted` wire event; one extra `warn!` line).
  `provider_failed` builds its log and wire strings from
  `(context, detail)` parts. The two parallel-bucket closures share
  `run_one_parallel`; three `async fn`s with no `await`
  (`bucket_by_execution_mode`, `apply_guard_pipeline`,
  `commit_assistant`) are plain `fn`s. The denial-path tracker call
  is panic-isolated like the success path. `commit.rs` imports its
  siblings directly instead of through `tools/mod.rs` aliases.
  `drive()` records the `agent.id` / `session.id` span fields it
  declared. `Agent::run` drops one redundant `Arc` clone.
- **Prompt-context fallback has one spelling**:
  `prompt::resolve_prompt_context(input, caps)` — used by both the
  loop's `assemble_system_prompt` and the single-shot strategies.
- **`uva-harness-tokio` demotes `uva-harness` to a
  dev-dependency.** The adapter's lib only implements
  `uva_core::spawn::Spawner`; the harness types appear in the
  example and doc prose, which now use plain code spans instead of
  cross-crate intra-doc links.
- **`crates/uva-harness/README.md`** replaces the seam table
  with a "Where new behavior goes" extension map (one row per need →
  the seam that answers it) and drops the hook example duplicated
  from "Extending the loop" (283 → 247 lines).

### Removed (harness refinement, continued)

- **`Agent::effective_max_iterations`** (defaulted trait method +
  override): the loop reads `caps.control.max_iterations` directly;
  the one example caller prints `ReActAgent::max_iterations()`.
- **`ReActAgent::projected_tool_definitions`**: the in-crate test
  compares `compose_tool_definitions` output directly; the server's
  `boot_wiring_applies_the_tools_section` now drives a real one-turn
  run with a capturing provider and asserts on the wire request —
  which is how the `max_visible` bug above surfaced.
- **`EventSink::is_closed` / `EventSink::text_delta`** (test-only
  shorthands; `EventSink::emit(AgentEvent::text_delta(..))` is the
  single spelling), a `pub(super)` re-export of
  `commit_all_results`, the `pub(super)` on `guard::Verdict`, and a
  duplicate `projection_cache_record_call_is_idempotent` test.

### Changed (harness core refactor: the loop owns its bundle, the cost of a run is measured)

- **`ReActLoop` owns `Arc<AgentCaps>`** — the `'a` lifetime is gone.
  `AgentRuntime.caps` is already an `Arc` (so a strategy can move the
  runtime into a spawned task), and `drive_from_runtime` was doing
  `Arc::try_unwrap` plus a full `AgentCaps::clone()` on the shared
  arm: every `Arc` plane, the `Vec<Arc<dyn SyntheticTool>>` and the
  `PathBuf`, on every `Agent::run`. The loop now shares the handle
  it was handed.
- **One `ModelConfig` per run.** `AgentCaps::model_config` asked the
  provider and returned an owned `ModelConfig` (two `String`s + five
  scalars); the loop called it once per iteration for two `&str`
  labels and twice more in `prepare` — six reads for a
  three-iteration run. `ReActLoop` reads it once when it is built and
  borrows from that copy. `AgentCaps::model_config`'s doc claimed it
  was "snapshotted at construction so the hot path does not re-ask
  the provider on every iteration"; it never was, and now it is.
- **Allocations that outlived their guards are gone**: the prefetch
  seam tested `frozen_segments(..).is_empty()` *after* copying, so
  every iteration of every default deployment allocated a `Vec` to
  discover that the default truncating manager freezes nothing (and
  its `prefetch: bool` knob had exactly one call site, passing
  `true`); the tool-result preview joined the entire output before
  truncating to 160 chars; the hook fan-out cloned the whole
  `CompletionResponse` once per installed hook instead of sharing one
  `Arc`; `json_len` allocated a `String` per JSON number on the
  per-event `size_hint` path; the projection rebuilt its visible-name
  set from two copies of the same predicate and fetched the
  descriptor list twice per rebuild.
- **Forwarding layers collapsed**: five `phase_*` wrappers plus
  `commit_assistant_and_step_end` and the `ReActLoop::drive` method
  are gone; `iteration_body` is the one place the five-phase order is
  written. The two provider-failure arms and the two tool-slot-fill
  bodies are one function each.
- **`AgentCaps::minimal` → `AgentCaps::for_provider`**, with
  `max_iterations` from `DEFAULT_MAX_ITERATIONS` instead of a
  hard-coded `4`, and a descriptor from `default_descriptor` instead
  of a literal whose name and kind were both `"test"` — a test
  fixture shipped in production code. `AgentInput::multi` is
  removed; it was a strict special case of
  `multi_with_history(Vec::new(), parts)`.
- **The seam rule is written down** (`seam.rs` module doc + the crate
  README): a type is `dyn` iff a downstream crate must install it at
  runtime. It already held; it had never been written, which is why
  `AgentRuntime`'s three fields and `ReActLoop`'s ownership read as
  accidents. Alongside it: an accessor's callers are the library's
  consumers, not this repository — do not delete a read accessor on a
  zero-reference count.
- **Comment budget ratcheted** (`make check-harness-docs`): 717 → 523
  doc-comment lines on non-`pub` items under
  `crates/uva-harness/src`. The public surface is unchanged.
- **Docs that contradicted the code are corrected**: the facade no
  longer promises `from_name("chain-of-thought")` (it resolves
  `"react"` / `"re-act"` / `"re_act"` and errors otherwise — CoT and
  BestOfN ship as `uva-strategy-cot` / `-best-of-n` behind the
  facade features); `SEAMS.md` §1 moved them to their real crates;
  the ~50 `Report:` links to `docs/optimization-report-R*.md` and
  `docs/reference-parity.md` are gone (those files are not in this
  repository) with each sentence's text kept; `AGENTS.md` points at
  `re_act/synthetic.rs` and at 5 shellchecked gate scripts; the
  `events` module doc now states which four variants are reserved
  for a consumer's own `AgentHook` / policy layer.
- **Bench**: the `"caps borrow vs per-iteration Arc clone"` invariant
  stopped describing the loop once it started sharing the bundle. It
  is now `"per-run Arc clone vs AgentCaps::clone"` — the fast side is
  an `Arc::clone` plus two field reads, which is what the loop now
  does. The other six ratios are untouched.

### Changed (harness kernel refinement: capability planes + loop consolidation)

- **`AgentCaps` regrouped into capability planes** (breaking,
  0.x): the 16-field flat bundle is now `strategy` +
  `model: ModelCaps` (provider / descriptor / prompt context) +
  `tools: ToolCaps` (registry / surface / restriction /
  synthetic) + `control: ControlCaps` (steering / iteration
  cap / context manager) + `env: EnvCaps` (workspace / clock /
  spawner / inbox / checkpointer / typed sink). No field added
  or removed; every access moves one segment down
  (`caps.provider` → `caps.model.provider`). `AgentScope`
  overlays patch only the `tools` and `control` planes.
- **Loop consolidation**: the 21 files under
  `re_act/orchestrator/` + `re_act/runtime/` + `re_act/state/`
  are now 8 phase-aligned files under `re_act/loop/`
  (`mod` = drive/iteration/prefetch, `prepare`, `sample`,
  `tools`, `guard`, `hooks`, `finalize`, `projection`), plus
  `re_act/state.rs` and `re_act/synthetic.rs` as direct
  siblings. Pure moves; item names and the wire contract are
  unchanged.
- **Hot-path allocations**: the tool `Context` is built once per
  run on `ReActLoop` instead of cloning `workspace_root` per
  dispatch; hook fan-outs share one `Arc<str>` payload per
  event instead of a `String` per hook; `AgentEvent::size_hint`
  is a shape-aware estimate (no `serde_json` serialization on
  the metrics path).
- **Reason-tagged recovery for unanswered tool calls**: a
  sequential-bucket call skipped by cancellation answers
  `"... did not run: tool call cancelled before dispatch; safe
  to retry."`, one skipped by a sibling error answers `"... did
  not run: skipped after sibling sequential tool error; outcome
  unknown — verify external state before retrying."` — instead
  of the old generic "tool did not produce a result".
- **Dead surface removed**: `AgentRuntime::caps_ref` (zero
  callers) and the `is_length_stop` alias are gone; panic
  isolation consolidates to `panic::caught` / `panic::isolate`
  (one `catch_unwind` per calling convention instead of five
  near-identical wrappers); the empty-hook fast path is the
  single `caps::has_hooks` predicate.

### Changed (harness refactor: runtime-free + plugin strategies)

- **`AgentRuntime` → `AgentCaps` + `AgentRuntime<'_>` (borrowed)**:
  the god-object (18 owned fields, 19 accessors) is split into
  `AgentCaps` (the immutable per-agent capability bundle,
  `ReActAgent` owns it) and a 3-field per-run `AgentRuntime`
  view (`caps: Arc<AgentCaps>`, `cancel`, `subagent_depth`)
  handed to `ReasoningStrategy::run`. The 13 accessors with
  zero workspace call sites are deleted; the 6 live ones are
  field reads on `AgentCaps` now.
- **`ToolInterceptor` → `SyntheticTool`** (and `InterceptorCall` →
  `SyntheticCall`): the trait is, after all, a tool the model sees
  but the registry does not own — the new name says so.
  `ReActAgent::with_interceptor` → `with_synthetic_tool`
  (`try_with_interceptor` → `try_with_synthetic_tool`). The file
  path stays `runtime/interceptors.rs`. Consumer update:
  `uva-tool-task`'s `DelegatorInterceptor` implements the
  renamed trait; the server's three call sites renamed.

- **`uva-harness` is runtime-free**: its `[dependencies]` carries
  zero tokio. `TokioSpawner` moved to the new sibling crate
  `uva-harness-tokio` (`uva::TokioSpawner` does not exist —
  the facade would fail `make check-public-api-runtime`; depend on
  the sibling crate directly). `cargo tree -p uva-harness -e
  normal` names no tokio / async-std / smol.
- **Alternative reasoning strategies moved to plugin crates**:
  `ChainOfThoughtStrategy` → `uva-strategy-cot` (facade feature
  `strategy-cot`), `BestOfNStrategy` / `CandidateScorer` /
  `LongestAnswer` / `LlmJudgeScorer` / `DEFAULT_JUDGE_PROMPT` →
  `uva-strategy-best-of-n` (facade feature
  `strategy-best-of-n`). The harness ships only the canonical
  `ReActStrategy`; `from_name("react")` still works, and plugin
  strategies resolve through `StrategyRegistry::register` or the
  server-side resolver. `ReActAgent::with_chain_of_thought` /
  `with_best_of_n` / `AgentBuilder::chain_of_thought` /
  `best_of_n` sugar setters deleted — install the plugin strategies
  via `with_strategy(Arc::new(...))`.
- **`AgentRunConfig` / `RunOverrides` moved to `uva-app-server`**
  (`crate::agent_run_config`): server-only wire types, not part of
  the library's public surface.
- **`CompactionEmitters` /
  `context_manager_for_compaction_with_emitters` moved to
  `uva-app-server`**: the harness's `runtime_for_run` inlines the
  same decision core; hand-built factories call the server-side
  entry.
- **`AgentRuntime` fields are now `pub(crate)`** — the 19
  accessors are the sole public read surface, so a strategy plugin
  reads `runtime.provider()` / `runtime.cancel()` instead of the
  field. Two accessors return an owned handle (`provider()`,
  `cancel()`) because `EventSink::begin` / `finish` and
  `ModelProvider::complete_with_stream` take `&Arc<dyn …>`;
  mutating one now goes through the existing chainable
  `with_descriptor` / `with_spawner` / `with_context_manager` /
  `with_cancel`. Migrated: `uva-strategy-cot`,
  `uva-strategy-best-of-n`, `uva-strategy-plan-execute`.
- **`uva_harness::extension` deleted** (breaking): the
  `Extensions` registry / `Extension<T>` trait were fully
  implemented, exported, documented — and never called by the
  orchestrator, so every consumer wiring (the `lib_grade` example
  among them) exercised code that could not run end-to-end. The
  already-wired seam is `uva_steering::AgentHook`, and the
  orchestrator calls that one. `ReActAgent::with_extensions` /
  `extensions()` and `AgentRuntime::extensions` go with it. No
  deprecated alias — the type was unreachable in practice, so
  nothing outside the crate could have depended on it.
- **`uva_harness::agent::re_act::runtime::registry` deleted**:
  `AgentEntry` moved next to the `AgentRegistry` that stores it
  (`uva_harness::agent::AgentEntry`, unchanged path). The old
  module only existed because the registry imported its own entry
  type through a cross-module hop, and it re-exported
  `AgentDescriptor` as a side effect.
- **`ReActAgent::try_with_synthetic_tool` deleted** (breaking): it
  was an infallible alias of `with_synthetic_tool`, which already
  returns the conflicting-name `Err`. `strategy::minimal` deleted
  for the same reason — `AgentRuntime::minimal` is the same
  function under a shorter path.
- **`ReActAgent` no longer has `descriptor_mut`**: the
  `#[cfg(test)]` accessor existed for two tests that now pass their
  descriptor to `with_descriptor` at construction.

### Added

- **`hpc-pool`** — HTTP connection pooling for the provider adapters.
  `PoolConfig` tunes the per-host idle cap, idle window, connect
  timeout, and warm-up; `PooledHttpClient` is the wrapper the
  adapters store; `PoolStats` reports `idle` / `in_flight` / `reused`
  / `established`. `AnthropicProvider::warm_up_pool` and
  `OpenAICompatibleProvider::warm_up_pool` seed the pool at boot.
  Off by default; with the feature disabled the adapters keep
  building `reqwest::Client::new()`.
- **`hpc-simd`** — arch-specific dot products for vector search
  (`uva-search` feature `simd`): AVX2 / SSE2 on x86_64, NEON on
  aarch64, scalar elsewhere, selected at runtime and reported by
  `DotRoute`; `SimdVectorStore` dispatches to the selected kernel.
  Off by default.
- **`hpc-batch`** — new `uva-hpc-batch` crate. `BatchEmbedder`
  wraps any `Arc<dyn Embedder>`, `BatchCompleter` wraps any
  `Arc<dyn ModelProvider>`; both are governed by
  `BatchPolicy { chunk_size, max_inflight }` and report `BatchStats`.
  No HTTP, telemetry, or database dependencies. Off by default.
- **`hpc-pipeline`** — out-of-band context compaction
  (`uva-harness` feature `pipeline`, forwarded from
  `uva-context/prefetch`). `ReActAgent::with_pipeline` overlaps
  compaction of frozen message segments with the next LLM request,
  detaching through the consumer's `Spawner`; `ContextManager` gains
  default no-op `frozen_segments` / `prefetch_compaction` methods, so
  existing implementations compile unchanged. Off by default.
- **`safety`** — new `uva-safety` crate. A `SafetyScanner`
  checks four stages — Input, ToolCall, ToolOutput, Output — for
  injection / jailbreak / PII-exfiltration patterns. The crate ships
  scanners, not harness wiring: a consumer installs one through its
  own `uva_steering::AgentHook` (`on_provider_start` for input,
  `before_tool_execute` / `after_tool_execute` for the tool stages);
  the harness loop itself calls no scanner. Off by default.
- **`resilience`** — new `uva-resilience` crate:
  `ResilientProvider` / `ResilientTool` wrap any provider or tool
  with a circuit breaker, adaptive backoff, and jitter, on top of
  the new runtime-neutral `uva_core::BreakerState`. Off by
  default.
- **`semantic-cache`** — new `uva-semantic-cache` crate:
  `SemanticCacheProvider` wraps a `ModelProvider` and short-circuits
  near-duplicate completions via embedding cosine similarity over a
  pluggable store. Off by default.
- **`tool-memo`** — new `uva-tool-memo` crate: `MemoizedTool`
  wraps any tool and short-circuits identical invocations through an
  LRU + TTL store. Off by default.
- **`strategy-plan-execute`** — new `uva-strategy-plan-execute`
  crate: a `ReasoningStrategy` that decomposes the prompt into
  numbered steps and runs each step through the model's tool loop.
  Registers through the new `StrategyRegistry`. Off by default.
- **`reflection`** — new `uva-reflection` crate: a
  `ReflectionHook` that every `every_n_steps` tool calls asks a
  cheap critic model to critique the trajectory and steer the next
  step. Off by default.
- **`budget`** — new `uva-budget` crate: `BudgetProvider` /
  `BudgetTool` enforce token / latency / cost ceilings per provider
  and tool, with per-model pricing. Off by default.
- **OpenTelemetry metrics, span taxonomy, and redaction**
  (`uva-telemetry`): an OTLP meter path alongside the existing
  Prometheus one, a typed `SpanKind` annotating every lifecycle
  span, and exporter-side redaction of `sensitive_keys` span
  attributes.
- **`ToolRegistry::dispatch_one`** — per-call sibling of
  `run_stream` with the same hidden-refusal, schema-validation, and
  panic-recovery contract. The stream yields zero or more `Progress`
  items plus exactly one `Result`; dropping it aborts the in-flight
  task.
- **`Visibility`** — `ToolDescriptor::visibility()` derives a single
  visibility state from `is_hidden` and `exposure`, so the
  projection consumes one source of truth instead of two independent
  fields.
- **`AgentRuntimeFactory`** — materialise an agent runtime from a
  bare descriptor, letting hosts run agents without naming a
  concrete agent type.
- **`JobAction::Agent`** — typed wire payload for "run one agent
  session" in the scheduler, plus an `AgentSpawner` seam that keeps
  the scheduler agent-agnostic.
- **`StrategyRegistry`** — register custom `ReasoningStrategy`
  implementations under a name and resolve them through the same
  lookup as `react` / `chain-of-thought` / `best-of-n`.
- **`LifecycleHookRegistry`** — fan telemetry out at every `SpanKind`
  boundary the orchestrator crosses; installed via
  `ReActAgent::with_lifecycle_hooks`. Hook panics are swallowed so
  telemetry cannot take down the loop.
- **`ServerWorkflowHost`** — the server's `WorkflowHost` wiring now
  spawns registry agents for real instead of stubbing the effect.
- **Repo gates** — `check-plugin-shape` pins a function-length and
  nesting budget for plugin crates; `check-pub-surface-strict`
  ratchets per-crate pub-use re-exports.
- **`ReActAgent::builder(...)`** — canonical constructor: returns an
  `AgentBuilder` pre-populated with the harness's defaults
  (`TruncatingContextManager`, `Steering::noop()`, `InlineSpawner`,
  `SharedClock::system()`); the builder carries chainable setters
  for `provider`, `tools`, `workspace`, `context_manager`,
  `interceptor`/`try_interceptor`, `spawner`, `steering`,
  `lifecycle_hooks`, `checkpointer`, `strategy`, `inbox`, `prompt`,
  `clock`, `max_iterations`, `safety_scanner`, `descriptor`,
  `typed_sink`, plus sugar methods `chain_of_thought` /
  `best_of_n`. The legacy `ReActAgent::new(...).with_*(...)` chain
  remains as a post-construction mutator surface.
- **`crates/uva-harness/docs/extension-points.md`** — public
  extension guide: one section per extension trait (`Provider`,
  `Tool` / `ToolRegistry`, `ContextManager`, `ReasoningStrategy`,
  `ToolInterceptor`, `LifecycleHook`, `Checkpointer`, `RunInbox`,
  `Spawner`, `Steering::{Guard, Hint, OutputTransformer}`,
  `SafetyScanner`) with the trait signature, default impl, when to
  override, and a worked example.
- **`examples/zero_deps_agent.rs`** — smoke test that exercises
  `ReActAgent::builder(...).build()` against a `FakeProvider`.

### Removed (the facade's two forwarding feature paths)

- **`uva::checkpoint` and `uva::transcript` are gone**, together
  with the `checkpoint` and `transcript` facade features that gated
  them. Both modules were pure forwarding shims: each re-exported one
  crate's types under a second path, so the same name lived at two
  places and the facade had a module per *concept* instead of per
  crate. The types keep their reachability at the one path each crate
  owns:
  - `uva::harness::{Checkpointer, InMemoryCheckpointer,
    CheckpointSnapshot, CheckpointError}` — the durability seam, from
    `uva-harness`.
  - `uva::eval::{Transcript, TranscriptSink, JsonlTranscriptSink,
    TranscriptOutcome, ToolCallRecord, SpanRecord, Usage}` — the
    evaluation trajectory, from `uva-eval`.
  A consumer that wrote `features = ["checkpoint"]` (or
  `["transcript"]`) in its `uva` dependency drops the feature and
  reaches the types under `uva::harness::*` / `uva::eval::*`;
  no other code changes.

### Changed
- **Breaking:** the six harness cargo features
  (`tokio-spawner`, `otel`, `safety`, `chain-of-thought`,
  `best-of-n`, `llm-judge`) are removed. `uva-harness`
  ships a single always-on build; the same symbol table (byte-
  identical `libuva_harness.rlib` across default /
  `--all-features` / `--no-default-features`) is the verification
  artifact. What used to be feature-gated is opt-in via types and
  traits: `TokioSpawner` is unconditional, the four safety scan
  helpers (`scan_input` / `scan_tool_call` / `scan_tool_output` /
  `scan_assistant_output`) compile unconditionally and return
  `Continue` when the scanner is `None`, `ChainOfThoughtStrategy` /
  `BestOfNStrategy` / `LlmJudgeScorer` are unconditional, and OTel
  emission is enabled through `uva-telemetry/otlp` (not via the
  harness).
- **Breaking:** the `LoopInputs` per-run mutable shim
  (`src/agent/re_act/runtime/inputs.rs`) is removed; the three
  values it carried (`cancel`, `depth`, `inbox_at_run_start`) live
  on `AgentRuntime` directly, so `ReActLoop::from_runtime` takes
  the runtime by value with no shim.
- **Breaking:** the `harness-tokio-spawner` facade feature is
  removed (the harness's default spawner is now `InlineSpawner` in
  the always-on build; consumers that want `TokioSpawner` install
  it via `ReActAgent::builder(...).spawner(Arc::new(TokioSpawner))`).
- **Breaking:** the `uva-harness/safety` feature is removed;
  the facade's `safety` feature still forwards `uva-safety`
  (the small scanner crate), but the harness's scan points compile
  unconditionally.

- **Breaking:** `ReActAgentBuilder` (the typed builder) is removed.
  Construct agents with `ReActAgent::new` / `with_descriptor` /
  `with_prompt_context` plus the `with_*` setters — the only
  construction story the guides document. The `typed_builder_smoke`
  example is removed with it.
- **Breaking:** zero-caller convenience items are removed:
  `PipelineConfig::disabled`, `PrefetchOutcome` (`maybe_prefetch`
  now returns `()`), `StrategyRegistry::unregister` and
  `StrategyRegistry::resolve_builtin`, `AgentEvent::progress` /
  `warning_kind` / `recovery` (construct `AgentEvent::System(
  SystemEvent::…)` directly), `BestOfNStrategy::candidates` /
  `scorer`, `ChainOfThoughtStrategy::max_steps`, and the `pub
  SNAPSHOT_FRAME` re-export (now crate-private).
- **Breaking:** `AgentEvent::warning` is removed (zero callers,
  hard-coded `WarningKind::Hook`). Construct
  `AgentEvent::System(SystemEvent::Warning { .. })` directly with
  the intended kind.
- `WarningKind` gains an `Inbox` variant (serde `"inbox"`): a
  consumer-supplied run-inbox seam panicked; the run continues
  from what was already drained. Additive for consumers that
  treat the tag as a string.
- `act_loop/dispatch.rs` is renamed `act_loop/iteration.rs`
  (iteration outcome routing), ending the name collision with
  `act_loop/seams/dispatch.rs` (single tool-call routing).
- `uva-harness` internal layout: the 688-line
  `act_loop/seams.rs` is split into `seams/{veto,dispatch}`;
  `builder/agent.rs` moves to `re_act/agent.rs`; the default
  dependency bundles get single-source constructors
  (`deps::default_loop_deps` / `default_dispatch_deps`); the
  `touched_target` projection and the compaction-emitter helpers
  live in `compaction.rs`.
- Hot-path allocations: the default (no-`safety`) assistant
  commit no longer clones the full text for its empty check,
  and the per-LLM-pass `provider_id` string is only built when
  otel metrics or a lifecycle hook actually consumes it.
- **Breaking:** `ToolSurfacePolicy::apply` is split into `validate`
  and `write_to_registry`. Callers that applied a policy in one step
  now check the verdict first, then write it to the registry.
- **ReAct loop restructured** into a shared resource bundle and
  per-phase functions with no public API change: `ReActAgent`,
  `AgentRuntime`, and every `with_*` setter keep their signatures.

### Fixed
- **Cancelled-before-tool runs now persist their checkpoint.**
  The path returned a pre-built `AgentOutput` and skipped the
  shared finalize tail, so a cancelled run with a checkpointer
  armed left no snapshot — it could not be resumed. Finalization
  is unified in `drive_finalize` (`IterationOutcome::EarlyReturn`
  is gone), which also restores the missing-output warning for
  schema-declaring runs cancelled at that seam.

- Broken intra-doc link in the `dispatch_one` rustdoc.

### Performance

- **`hpc-pool`:** after boot warm-up the first user request no
  longer pays the TCP/TLS handshake; the pool workload reuses
  64/64 connections.
- **`hpc-simd`:** 768-dim kNN dot runs ~1.65× faster on the AVX2
  lane; the scalar path remains as the fallback.
- **`hpc-batch`:** batched embeds and completions hold in-flight
  concurrency at `max_inflight` while preserving input order.
- **Tool projection:** the descriptor-to-projection walk is
  single-pass, the projection memo is keyed by one `u64`
  fingerprint, and closing an iteration drains steering once
  instead of twice.
- **Event sizing:** `AgentEvent::serialized_size` counts JSON
  bytes through a null `io::Write` sink instead of materializing
  a `Vec` per event (the server calls it on every event).
- **Veto seam:** the pre-execution hook loop clones the tool
  name and JSON input once per tool call (shared `Arc`) instead
  of deep-cloning both once and again per hook.
- **Best-of-n:** candidate scoring is concurrent — an
  `llm-judge` scorer's N provider calls overlap instead of
  queueing behind the arrival loop.

### Fixed

- **`GET /api/v1/tasks` cursor resumes, and one delegation is listed
  once.** A delegation's id is now `<parentSessionId>:<childSessionId>`
  — the pair that names it, fixed at delegation time. The landed
  pagination had keyed the cursor on the row's position inside the
  scan's 256-row tail window, which shifts as the child's log grows,
  so the second page came back empty; and a delegation — recorded both
  in the child's own log and in the root parent's mirror — was listed
  twice. The list is now sorted (`started_at`, newest first by
  default, with the id as the tiebreaker), so the cursor resumes in
  the order the first page was returned in.
- **The delegation record is read from the child's own session log.**
  That log holds the whole story — the `subagent_enter` header, the
  dispatch prompt, the closing `subagent_exit` footer; the root
  parent's mirror is the fallback when the child's log cannot be read.
  The detail route reads the same source: a request naming a parent
  that did not delegate is a 404, and an id shaped to escape the
  sessions root is a 422.
- **The Tasks page follows the cursor.** `GET /api/v1/tasks` is
  keyset-paginated and the page had been fetching it once — silently
  showing only the first 20 delegations. It now uses the same
  cursor-list pattern as the other list pages (a "Load more" button
  follows `next_cursor`), and the Tools page's "Recent task
  delegations" panel fetches `?limit=5` — the server's default order
  is newest first, so those five are the five most recent.

## [0.1.0] - 2026-09-30

The first open-source cut of Uva. A modular, runtime-neutral AI
agent framework in Rust: a ReAct loop driven by a pluggable provider
surface, a one-crate facade (`uva`) that re-exports every component
behind feature flags, eight plugin-brick tools (`read` / `write` /
`shell` / `todo` / `web_fetch` / `schedule` / `search` / `task`), an
HTTP / SSE / WebSocket server (`uva-app-server`), a React/Vite web UI
(`uva-web`), and a stdio MCP binary (`uva-mcp-server`) that
exposes the same tools to any external MCP client. Tag-driven
`release.yml` emits cross-platform container images
(`docker.io/crochee/uva-{server,web}` linux/amd64 + linux/arm64)
with OCI labels for runtime identity verification.

### Highlights

- **One facade, 28 crates.** `cargo add uva` is the consumer entry
  point; `uva` re-exports the SDK pieces behind feature flags and
  ships a curated `prelude`. Every crate except `uva-app-server` and
  `uva-mcp-server` (the two application / binary crates) is
  runtime-neutral: public APIs name `futures::Stream` /
  `uva_core::CancelToken` / `uva_core::spawn::Spawner`, not
  `tokio::*`, so a consumer can run the loop on
  `std::thread + futures::executor::block_on`.
- **ReAct loop with full event streaming.** The harness is a
  `ReActAgent` that emits typed events over `futures::channel::mpsc`,
  honours a cancellation token, and exposes hooks (pre-`/post-step`,
  tool call interception, output transformers) at every boundary.
- **Pluggable LLM providers.** `OpenAI` + `Anthropic` adapters ship in
  tree behind feature flags; `ProviderProfile` /
  `ProviderRegistry` build any provider from a typed config.
- **Eight plugin-brick tools.** `read` / `write` / `shell` (with OS
  sandbox policy) / `todo` / `web_fetch` / `schedule` / `search` /
  `task` — each its own crate, each gated on a facade feature, each
  registering into `ToolRegistry` with workspace path confinement.
- **Subagent delegation as first-class.** The `task` tool creates child
  sessions, persists them through the same `SessionSink`, attributes
  them to the parent's transcript via `parent_session_id`, and
  surfaces them in `/api/v1/sessions` next to user conversations.
- **Session lifecycle.** Create / list / read / branch / export /
  delete sessions. Durable JSONL store with schema-version header;
  idempotency keys for retries; recoverable tail.
- **Multimodal attachments.** Image / file / PDF; `ResourceLink`
  bounded inlining (text up to 16 KiB, otherwise a "contents not
  inlined" note); an Anthropic `document` block on the `/v1/messages`
  protocol endpoint.
- **MCP dual surface.** `uva-mcp` drives external MCP servers and
  publishes their tools locally; `uva-mcp-server` is the
  outbound stdio binary any MCP client (Claude Code, `pi`, the
  Anthropic SDK, …) spawns to get the eight plugin tools.
- **Multi-agent workflows.** Declarative `WorkflowSpec` (agent /
  fan-out / pipeline) plus a `WorkflowHost` effect seam; JSONL journal
  with prefix replay for resume.
- **Cron + interval + once scheduling.** Hierarchical `TimingWheel`
  driven by the host's `tick(now)`; opt-in `cron` feature on the
  facade so a consumer can also create cron jobs.
- **Cross-domain search.** `uva-search` Registry over BM25 +
  embedder + reranker; `uva-tool-search` exposes it as the
  model-facing `search` tool (cold-start `Deferred` schema, promotes
  on first call).
- **HTTP server with full protocol coverage.** `/api/v1/chat/*` REST
  + SSE; `/v1/messages` Anthropic-protocol endpoint (Claude Code
  plug-and-play); `/livez` + `/readyz` + `/metrics` Prometheus
  endpoint; OpenTelemetry OTLP tracing + Prometheus RED metrics;
  WebSocket approvals.
- **Production-grade CI / CD.** `make ci` runs `fmt-check`,
  `lint-rust`, `doc-check`, `test-guides`, `check-mvp-deps`,
  `check-no-runtime`, `check-public-api-runtime`, `check-pub-surface`,
  `check-claim-language`, `check-clock`, `check-harness-shape`,
  `check-test-layout`, `check-deny`. 11 GitHub Actions workflows cover
  CI, rust-quality, release, docs, security-audit, contract-closure,
  dependabot-auto-merge, markdown-lint, shellcheck, stale, labeler.
  Tag-driven `release.yml` builds and pushes cross-platform container
  images (`docker.io/crochee/uva-{server,web}` linux/amd64 +
  linux/arm64) with two tags each (`<tag>` and `<tag>-<short-sha>`)
  and verifies identity via OCI labels at release time.
- **Container images with baked identity.** Two release images
  (`uva-app-server`, `uva-web`) ship the exact commit SHA + build
  time + version through both the embedded binary (`--version`) and
  OCI image labels
  (`org.opencontainers.image.revision` /
  `org.opencontainers.image.version` /
  `org.opencontainers.image.created`). nginx config in `uva-web`
  is templated: `UVA_BACKEND_HOST` / `UVA_BACKEND_PORT` set
  the upstream at runtime. (`uva-mcp-server` is a stdio binary
  and is not published as a release image.)
- **Kubernetes-ready.** `k8s/server.yaml` + `k8s/web.yaml` —
  Deployments + ClusterIP Services + PodDisruptionBudgets +
  securityContext (`runAsNonRoot: true`, `runAsUser: 65532`,
  `readOnlyRootFilesystem: true`, `seccompProfile: RuntimeDefault`),
  `emptyDir` mounts for `/data` and `/tmp`, liveness/readiness probes
  against `/livez` and `/readyz`.

### Breaking changes since the prior `[0.1.0]` snapshot (2026-05-09)

The pre-existing internal `[0.1.0]` and `[0.2.0]` entries below
describe a different internal tree; they are kept under **Prior
releases** for historical reference only. The 0.1.0 cut listed here
is the canonical open-source release, and downstream consumers should
pin to it.

### Known limitations

- **sandbox policy wiring**: the `sandbox` config key on
  `[agents.<name>]` is parsed but not yet bound to a per-run shell
  policy; the boot-time default policy is in effect. The follow-up
  wires the parsed policy into `ShellTool::with_policy` at run
  factory. (`docs/research/uva-r124-holistic-design.md` §2.)
- **Phase 3 trait abstractions**: `LoopDetector`, `PermissionPolicy`
  sub-traits, `OsSandbox`, and `Message::cache_control` field
  abstractions deferred 6 months (re-evaluate 2026-12-10) per the
  R29 / R30 6-expert adversarial review.
- **Cross-platform release**: tag-driven release builds and pushes
  container images for `linux/amd64` + `linux/arm64` (server and web).
  Bare-metal binaries are no longer published as release artefacts;
  consumers run the published images instead. Image signing and SBOM
  are tracked in `RELEASE.md` §后续路线.

### Install / verify

```sh
# Latest container images (linux/amd64 + linux/arm64):
docker pull docker.io/crochee/uva-app-server:0.1.0
docker pull docker.io/crochee/uva-web:0.1.0

# Verify the server image's baked identity:
docker run --rm docker.io/crochee/uva-app-server:0.1.0 --version
# → uva-app-server 0.1.0 (<7-char sha> <build-time-utc>) linux release

# Verify image labels (matches the running binary's --version line):
docker buildx imagetools inspect docker.io/crochee/uva-app-server:0.1.0 \
  --format '{{json .Image.Labels}}' | jq -r '."org.opencontainers.image.revision"'
```

### Acknowledgements

Uva's plugin split, MCP surface, multi-agent traces, and
search domain are adopted from a community reading of `pi-mono`,
`traitclaw`, `dsh`, Claude Code, Codex, Cline, Continue, Aider,
Goose, OpenCode, and the broader agent ecosystem. The R124 holistic
design pass tracks 95% community convergence and the rationale for
each divergence. See `docs/research/uva-r124-holistic-design.md`
for the audit trail and `docs/optimization-report-*.md` for the
per-round record.

## [Historical round log — R0–R125, May–Sep 2026]

The per-round development log before the open-source `0.1.0` cut.
Frozen per the precedent set in `optimization-report-R46`: every
change below shipped across the R0–R125 development cycle that
preceded the first public release. Per-round *intent* and the
*verification* they carried is preserved verbatim; the *user-visible
capability set* of the released `0.1.0` is summarised above and
in `docs/INDEX.md` / `docs/ARCHITECTURE.md` / `README.md`. The
following section header reads `[Unreleased]` because that was
its tag at the time; the entries themselves are immutable.

### Fixed

- **The chat page's resume stream survives hydration, yields to a turn,
  then re-arms.** `normalizeSessionState` only recognised the
  protobuf-JSON enum names (`SESSION_STATE_WORKING`) while the
  session-detail endpoint reports the canonical lowercase form
  (`working`), so **every** hydration normalised to `unknown` — the one
  value the resume effect treats as "nothing to tail". Two failures
  followed: the SSE was torn down milliseconds after mounting and never
  re-attached, and `TURN_TERMINAL` never matched, so a completed turn's
  typing indicator could only clear on the next live frame. The helper
  now folds both wire shapes onto one suffix
  (`uva-web/src/pages/ChatPage.tsx`).
- **A locally-owned turn stream takes the resume stream's place.**
  `handleSubmit` / `handleRetry` / `handleRegenerate` now set
  `turnStreamRef` and abort the resume controller; the resume effect's
  `finally` skips reconciliation for that hand-off instead of
  downgrading a working bubble to `failed`, and the turn's own `finally`
  clears the flag and bumps `resumeNonce` so the resume stream
  re-attaches from the persisted `streamIndex`. Without the hand-off
  both consumers read the same broadcast, and the resume path's
  per-cursor re-hydrate (`GET /api/v1/sessions/{id}` →
  `reconstructMessagesFromSession`) replaced the transcript with
  freshly-minted message ids while `applyStreamEvent` appended to the
  old ones — every later frame a silent no-op, so a turn longer than one
  cursor batch (`RESUME_CURSOR_BATCH = 50` frames) froze mid-stream.
  `handleRegenerate` needed it most: it hides the trailing answer while
  the rerun polls, and an attached resume stream put that stale answer
  straight back.
- **The resume stream's 1.5 s re-hydrate poll runs only while a turn is
  in flight.** It was armed for the whole life of the stream, so an
  idle chat page re-hydrated its entire transcript — `setMessages` with
  freshly minted message ids, remounting every message — every 1.5 s
  forever, and on a long session that is a multi-second JSON parse each
  time (measured: 44 consecutive `GET /api/v1/sessions/{id}` at ~1.5 s
  intervals on a `completed` session). An idle stream still needs to
  stay attached to pick up the next turn; it does not need to poll for
  information that cannot change.
- **A session the server has not persisted yet no longer disables the
  resume stream.** A client-minted id answers `404` on the hydrate GET;
  that is now read as "no status yet" (status stays `null`) instead of
  `unknown`.

Verified against a running `uva-app-server` + `uva-web` (headless
Chromium, network + server logs):

- Session `50d53d32…`: the SSE attaches at mount, re-attaches after the
  hydrate's status flip with the persisted cursor, is aborted 13 ms
  after `POST /v1/messages` takes over, and re-attaches with `?from=6`
  when the turn ends.
- Session `0064bf3d…`: clicking Regenerate aborts the resume stream 8 ms
  after `POST …/regenerate` and re-attaches with `?from=3` once the
  rerun's poll finishes.
- Idle session (status `completed`): one cursor-driven `GET
  /api/v1/sessions/{id}` ~1.7 s after attach, then no further request for
  the rest of the window — before the poll gate that endpoint was hit
  every ~1.5 s indefinitely (44 consecutive requests measured on the
  same session).

### Added

- `uva-app-server`: end-to-end `crates/uva-app-server/examples/observability.rs`
  — an executable proof that boots `uva::telemetry::init_tracing`
  against either the OTLP pipeline (when `UVA_OTLP_ENDPOINT` is
  set) or the console fallback (when it isn't), round-trips a W3C
  `traceparent` through extract + inject, and confirms the
  Prometheus text exposition encoder returns within microseconds. Prints
  the proof line `OBSERVABILITY-EXAMPLE: OK` so `make examples`
  picks it up automatically — same `Makefile` walker every other
  example in the workspace already conforms to. No new dep; reuses
  the existing `opentelemetry` dev-dep in `crates/uva-app-server`.
  Spec: `docs/research/uva-r124-holistic-design.md` §3 (Adopt C,
  observability-example half).

- `uva-app-server`: `state::unapplied_agent_keys` now classifies
  `sandbox` (R62) as inert so the boot log names the gap. R62
  adopted the parser (`AgentConfig::sandbox_policy_name`) but never
  wired a consumer — every run still uses the boot-time
  `ShellTool::new()` (`state/app_state/tools.rs:13-32`), so an
  operator who wrote `sandbox = "read-only"` has restricted nothing
  and now sees a one-line warning per configured agent. The actual
  consumer (per-run `ShellTool::with_policy` re-bind in the run
  factory) is tracked in `docs/research/uva-r124-holistic-design.md`
  §2 as a follow-up; this round surfaces the gap rather than fixing
  it because the production-grade fix touches `AgentRunConfig` +
  `RunStreamFactory` + `RunDependencies` and deserves its own
  scoped round.
  Spec: `docs/research/uva-r124-holistic-design.md` §2 (Adopt B,
  audit-half).

- `uva-tool`: `ToolAnnotations` re-exported at the crate root (was
  already public as `uva_tool::registry::ToolAnnotations`, now
  reachable as `uva_tool::ToolAnnotations`) and the seven plugin
  tools that implement `Tool` (read / write / shell / todo / web_fetch
  / schedule / search) now override `annotations()` to publish the
  MCP-native `readOnlyHint` / `destructiveHint` / `idempotentHint` /
  `openWorldHint` hints. The `shell` tool's hints follow the
  configured `ExecutionPolicy::is_confined()` so a confined policy
  (`WorkspaceReadOnly`, `DangerFullAccess`, …) advertises the same
  boundary the call enforces; `web_fetch`'s `openWorldHint: true`
  surfaces the public-internet dependency that was previously
  invisible at the descriptor layer. All hints ride on the existing
  `From<ToolAnnotations> for uva_provider::ToolAnnotations`
  projection (R62), so the wire shape stays additive and old
  consumers ignore the new field. `task` is a `ToolInterceptor`, not
  a `Tool`, so it correctly stays out of this round.
  Spec: `docs/research/uva-r124-holistic-design.md` §1.
- `uva-tool-search`: the `search` tool's `parameters()` schema
  now declares top-level `"additionalProperties": false` so a model
  that emits a typo (`lmit` instead of `limit`, `qury` instead of
  `query`) gets a model-facing error instead of a silent default;
  the existing `name_and_parameters_shape` test pins the new field
  so the regression is caught in CI. Closes the only schema drift
  among the eight plugin tool schemas; the other seven already
  pinned it through `schemars`'s `#[schemars(extend(...))]` derive.
  Spec: `docs/research/uva-r124-holistic-design.md` §1.3.

### Design

- R124 — multi-expert holistic design pass informed by pi-mono +
  TraitClaw + the broader community (Claude Code / Codex / Cline /
  Continue / Aider / Goose / OpenCode / dsh). Five parallel expert
  audits covered TraitClaw 8-trait parity, pi-mono adoption delta,
  plugin-tool UX unification, seams + file-size, and additive
  candidates. Uva is at **95% community convergence**; the design
  selects three concrete adopts for this round and tracks the rest
  with deferral reasoning.
  Spec: `docs/research/uva-r124-holistic-design.md`.

- `uva-session`: `SessionSink` gains two **default-implemented**
  methods, so a backend that needs neither stays at the original
  five-method surface. `append_with_key(key, event)` is the idempotency
  seam: `Some(key)` callers that re-submit the same key observe the
  `seq` originally assigned and the bytes are not written twice —
  `JsonlSessionSink` keeps a 1024-entry move-to-front LRU of recent
  keys, `InMemorySessionSink` a `HashSet` + reverse `HashMap`. This
  closes the "client retried / SSE reconnected mid-flush" hole that
  could land one user prompt twice in the durable log. `append_many
  (events)` is the batched-durability seam: the default loops over
  `append` (N fsyncs); `JsonlSessionSink` overrides it with one
  `write_all` + one `fsync`, and rolls `next_seq` back on failure so a
  retry cannot double-write. `fork_session` now uses it — a 27 086-row
  fork that took 63.8 s completes in well under a second.
- `uva-session`: `JsonlSessionSink` writes a **schema-version
  metadata header** as the first line of a fresh `events.jsonl`
  (`{"_meta":{"kind":"uva_session_meta","schema_version":1,"id":
  …}}`). Readers detect it off the first non-empty line and treat a
  file without one as `schema_version = 0` (every line an event row) —
  so pre-existing logs keep reading byte-identically, and `count_lines`
  / `read` / `read_from` all skip a header when present. `CURRENT_
  SCHEMA_VERSION` and `SessionMetadataHeader` are public.
- `uva-app-server`: `POST /api/v1/sessions/{id}/fork` — **branch a
  session**. The child is a copy of the parent's durable transcript up
  to `from_stream_index` (the whole log when omitted) and is registered
  through the same path as `POST /api/v1/chat/sessions`, so it is an
  ordinary conversation from there on: listed, addressable, streamable,
  deletable and independently appendable, with the parent untouched.
  Rows are copied **through the child's sink**, not by writing the
  child's file behind it: the sink is the trait's only durable write
  path, and writing the file directly would couple the route to one
  backend's layout. The cost was one fsync per row — measured 63.8 s for
  a 27 086-row parent — until `SessionSink::append_many` landed with a
  single-`fsync` batched write path (`JsonlSessionSink`), which the
  route now uses. This is the *branch* half
  of pi's `Navigation` operation — *rewind* (rewriting the parent's log
  in place) is a different, destructive operation and is deliberately
  not part of this route. `SessionsPage` gains a Fork action that opens
  the branch. 5 route tests: the copy is addressable and the parent is
  untouched, the cutoff truncates, a registered-but-empty parent forks
  empty, the child's next append lands at the ordinal after the prefix,
  and an unknown parent is a 404.
- `uva-app-server`: the session lifecycle gains **delete** and **export**
  — `DELETE /api/v1/sessions/{id}` (`204`; `404` for an id neither
  source knows; `409` `session_busy` while a turn is running, because
  the run still owns its sink and is mid-append) and
  `GET /api/v1/sessions/{id}/export` (the durable transcript as
  `application/x-ndjson`: byte-faithful rows in append order, torn tail
  dropped, `Content-Disposition: attachment`). Deletion closes the
  controller and drops it from the active map *before* removing the
  directory, so nothing recreates the path by appending to it; the
  session search index needs no surgery because it rebuilds per query
  from the durable tree. `SessionSink` stays inert:
  `SessionRegistry::remove` is the registry's own bookkeeping and the
  *policy* — when a session may vanish, where its bytes live — is the
  route's. `SessionsPage` gains Export / Delete per card (confirm modal,
  optimistic removal) and `api.getRaw` is the client's entry point for a
  non-JSON body, so a download carries the same `Authorization` header
  as every other call. 6 route tests + the `turn_in_flight` contract
  test.
- `uva-search`: `SearchEngine::save`/`load` — snapshot + op-log persistence
  (`<root>/snapshot.json` full state, `<root>/ops.jsonl` incremental append,
  serde boundary at the file edge; vectors persist with the snapshot so
  provider-class non-deterministic embedders are not re-embedded on load);
  `VectorStore::vector_at` snapshot accessor; `SearchError::Persist`.
  Spec: `docs/superpowers/specs/2026-09-19-wheel-cron-search-persistence-design.md` §1.
- `uva-scheduler`: hierarchical `TimingWheel` (host-tick driven; O(1)
  empty tick and next-deadline; overflow sweep) + the `JobTrigger` seam
  (`OnceTrigger` / `IntervalTrigger`) + the opt-in `cron` feature's
  `CronTrigger` (5-field POSIX; the dom/dow conjunction divergence is
  documented); a store write made around the scheduler — the `schedule`
  tool's actions, an operator's own store handle, a `ScheduleStore::load`
  — is visible to the next `tick` / `next_deadline`, because each pass
  compares the store's revision with the one its wheel was built from
  (`ScheduleStore::version`), so `Scheduler::resync` is an explicit
  rebuild rather than something the host owes; `uva-tool-scheduler`:
  with the `cron` feature forwarded, `create kind=cron` is accepted —
  feature off is byte-identical to before.
  Spec: `docs/superpowers/specs/2026-09-19-wheel-cron-search-persistence-design.md` §2.
- `uva` (facade): `cron` feature — the whole cron path behind one
  switch (`scheduler` + `uva-scheduler/cron`, `tool-scheduler` +
  `uva-tool-scheduler/cron`), so a consumer who enables cron can also
  create cron jobs. A parser-only feature would leave the `schedule` tool
  refusing `kind: "cron"`, i.e. half of the round's user-visible win
  reachable and half not. Stays out of the default feature set.
- `uva-tool-search` (facade feature `tool-search`): the cross-domain
  `search` tool — one `Tool` over a host-built `uva-search::Registry`,
  so a query fans out across every registered engine (tool / mcp /
  skill / memory / agent / session catalogs) and returns the
  agent-facing projection (`domain` / `id` / `title` / `why` / `score`,
  plus a `preview` when the domain carries content, best match first);
  an optional `domain` argument narrows the fan-out to one catalog.
  Registered with `ToolExposure::Deferred`: the cold-start tool list
  carries name + description with a placeholder schema, and the real
  `query` / `limit` / `domain` schema is promoted once a `search` call
  is in the transcript. Opt-in rather than default, like `mcp` /
  `tool-scheduler`, because the host constructs the Registry it
  searches; retrieval is read-only and CPU-bound, so the tool keeps
  parallel execution. Offline (no HTTP client) and asserted by
  `check-mvp-deps` via `OFFLINE_TOOL_CRATES`.
  Spec: `docs/superpowers/specs/2026-09-19-wheel-cron-search-persistence-design.md` §3.
- `uva-search`: the `Registry` is keyed by **domain label** —
  `register` keeps `std::any::type_name::<T>()`, `register_domain` /
  `register_erased` take a host-chosen label (`"tool"`, `"skill"`, …), so
  several engines of one `T` can coexist and every `Hit` is stamped with
  its engine's label (`Hit::domain`); `QueryContext::domains` filters
  which engines a query fans out to. `ErasedEngine::search_erased` and
  `Registry::search` are now async (`async-trait`, runtime-neutral), so
  adapter engines over async retrievers — a session full-text index that
  scans on a blocking pool, a memory tier whose `recall` is async — join
  the same cross-domain merge without a blocking bridge; CPU-only
  `SearchEngine<T>` values complete without yielding. `Hit` /
  `AgentCandidate` gain `domain` and an optional `preview`
  (`Searchable::preview`), and `search_tool_ctx` accepts a caller-built
  `QueryContext` (domain filter, host routing facts).
- `uva-app-server`: `crates/uva-app-server/src/search/` — the six-domain
  wrap. Tool / MCP / skill / agent catalogs become `SearchDoc` rows in
  self-refreshing `CatalogEngine`s (tool + mcp re-probe
  `ToolRegistry::version`, agents `AgentRegistry::version`, skills on a
  clock bucket since skill files carry no counter); memory and sessions
  keep their own retrieval behind `ErasedEngine` adapters
  (`Memory::recall`, `SessionSearch`). One `SearchService` offers two
  views: the **agent view** (five catalog domains — no session corpus)
  is registered as the model-facing `search` tool via
  `register_search_tool`, so an agent discovers tools / skills / MCP
  tools / memory / peer agents instead of the host enumerating them
  into context; the **HTTP view** (all six) is projected by
  `GET /api/v1/search?q=&limit=&domains=`, which the web UI's
  per-resource search now rides. Tenancy stays server-side:
  the handler passes `RequestUserId` through `QueryContext::extra`, so
  session hits are scoped to the requesting user and the agent view
  never exposes transcripts at all. AppState gains
  `memory: Arc<dyn Memory>` (file-backed tier under the workspace's
  `.uva/memory*` scopes) and `search: Arc<SearchService>`. With the
  wrap live, MCP tools register as `ToolExposure::Deferred` (name +
  description per request; schema promotes on first call) and the
  `[tools]` `max_visible` cap now counts the `search` tool — both are
  prompt-economy wins, not behaviour changes to dispatch. The
  `/api/v1/memory/search` skills-directory proxy is deleted: the unified
  endpoint replaces it (clean cutover, no shim).
- `uva-app-server`: `GET /api/v1/search` — cross-domain search over the
  deployment's tool / mcp / skill / memory / agent / session catalogs.
  The v1 `List<T>` envelope with search-owned ordering (best-first, no
  cursor), `domains` as a comma-separated filter, `limit` clamped to
  50, `q` required; session hits scoped to the requesting user with the
  same auth-layer fallback as `/api/v1/sessions/search`.
- `uva-web`: the Sessions page's two search cards (the
  skills-directory memory proxy and the session-history search) collapse
  into one **unified search card** over `GET /api/v1/search` — results
  carry their domain badge and preview, session hits link to the
  transcript, and the command palette queries the same endpoint once the
  user types (server-ranked hits bypass the local fuzzy filter, which
  would otherwise drop semantic matches).
- `uva-app-server`: the model selector is real — a turn's `model`
  selection now picks the provider that samples it, on every surface
  that carries one. `AppState::provider_catalogue` is built at boot from
  `[providers.<name>]` (each entry built through
  `ProviderProfile::build_provider`, keyed by its config name), and
  `AppState::resolve_provider` accepts both spellings a client sends:
  `"<provider>/<model>"` (what `/api/v1/models` advertises and the chat
  UI posts) and a bare `"<model>"` that exactly one configured provider
  advertises (the Anthropic protocol's `model` field). The selection
  travels with the operation —
  `SessionController::submit_with_provider(op, PinnedProvider::new(p))`,
  consumed by the run that operation starts — so it is per turn rather
  than sticky, and two turns queued behind a running one cannot swap
  each other's provider out. `SessionController::submit` is unchanged
  and means "the session's configured default".
  A selection that resolves to nothing answers `400` (or `503` for a
  provider the deployment configures but could not build) with a message
  naming the value, instead of silently running the default. The durable
  `request_header` row now names the provider that actually ran rather
  than the deployment default a selected turn overrode.
  Spec: the model-selector defect (a parsed-and-ignored `model` field).
- `uva-app-server`: the Messages endpoint's `document` block is real, so a
  PDF / text / Markdown attachment sent over `POST /v1/messages` reaches
  the agent again. The block was a field-less stub that converted to a
  `400` naming it, which the web composer's `accept` list never reflected:
  an attachment could be queued, shown, and then silently refused at send
  time. Both of the protocol's spellings convert —
  `{"type":"base64","media_type":…,"data":…}` and `{"type":"url","url":…}` —
  onto the same `ContentPart::Resource` the retired `/api/v1/chat/*`
  `kind: "file"` wire produced (an inline source becomes the historical
  `data:<media_type>;base64,<payload>` URI), so a transcript replayed
  through either endpoint projects identically. An unknown `source.type`
  or a source missing a field it requires is still a `400`, and now names
  the value or field: `WireContent` deserializes the block list directly
  (the untagged wrapper collapsed every inner failure into "data did not
  match any variant", which is what left a malformed document
  unexplained), and `tool_result.content` — the same two spellings — now
  uses that one type rather than a second. Audio remains unrepresentable
  on this wire — the protocol has no audio block — so the composer no
  longer offers it: `audio/*` left the file input's `accept` list, the
  audio attachment kind and its player were removed, and an audio file
  is now refused at selection time with a visible message rather than
  being queued and dropped at send.
- `uva-app-server`: **the Anthropic Messages protocol endpoint** —
  `POST /v1/messages` (non-streaming and `stream:true` SSE), so an
  Anthropic-protocol client (the Anthropic SDK, Claude Code, Cursor-style
  tools) can point its base URL at this server and drive the same agent
  the web UI does. The request projects onto the canonical
  `Message` / `ContentPart` model (`system`, multi-turn `messages`, the
  `text` / `image` / `document` / `tool_use` / `tool_result` / `thinking`
  blocks, and `tools` as `ToolDefinition`s); the response is the
  protocol's own shape, and a streamed turn emits the protocol's event
  sequence (`message_start` → `content_block_start` →
  `content_block_delta`* → `content_block_stop` → `message_delta` →
  `message_stop`).
  Auth accepts `x-api-key` alongside `Authorization` (Authorization wins
  when both are present), and a `401` on this path renders the protocol's
  `authentication_error` envelope so an SDK raises a parseable error;
  every other path keeps the application envelope unchanged.
  A malformed body is a `400` in the protocol envelope, naming the field
  or value at fault.
  Two opt-in extensions, both gated on the presence of
  `metadata.uva_session_id`, keep the web client working on this wire
  while external clients see today's bytes: a session binding (the named
  session is adopted, validated with the same resource-name rule as every
  path parameter and scoped to the requesting user, with history seeding
  skipped once the session holds rows and no close/evict at turn end), and
  an in-thread tool surface (the call and its result are observable in one
  turn, which the protocol's own `tool_use` / `tool_result` pairing cannot
  express for a server that executes its own tools). `metadata.uva_agent_name`
  selects the agent for the turn.
- `uva-web`: the chat client now speaks that protocol — turns are
  `POST /v1/messages` (`stream:true`) with the same credential, and the
  Anthropic SSE frames lower into the existing `SessionStreamEvent`
  vocabulary, so the reducer, tool-block pairing, cancel, regenerate,
  feedback and usage surfaces are unchanged. `SessionDetailPage`'s
  transcript projection renders a turn's attachments from the persisted
  refs (below), so a reloaded file or image turn shows a labelled chip
  instead of an empty bubble.
- `uva-app-server`: a multimodal turn's durable `UserInput` row now
  carries **byte-free attachment references** (`kind` / `name` /
  `mime_type` / `byte_len`) under `data.attachments`. The bytes stay out
  of the log deliberately — base64 images would bloat a session file —
  which is why the refs exist: without them a reloaded transcript could
  not show that the turn carried anything. A text-only turn omits the
  field, and an attachment-only turn now writes a row at all.
- `uva-provider`: a `ContentPart::Resource` projects into a prompt as
  a bounded, readable note instead of the resource itself. The OpenAI
  adapter previously inlined the whole `data:` URI — a 1 MB PDF became
  ~1.4 M characters of base64 that blow the context window and teach the
  model nothing — and the Anthropic adapter emitted a constant that lost
  even the filename. Both now share `ResourceLink::prompt_projection`:
  name and media type always, plus the decoded text for a `text/*`
  payload up to 16 KiB, and an explicit "contents not inlined (N bytes)"
  note otherwise.
- `uva-web`: the UI pass bringing the shell closer to the reference
  desktop client, with the existing colour/typography theme untouched —
  a collapsible left sidebar (`Ctrl/Cmd+B`, persisted, keyboard-accessible,
  and inert while typing), a command palette (`Ctrl/Cmd+K`) that
  fuzzy-filters navigation, sessions and agents with arrow/Enter/Escape
  and a focus trap, the composer's control order realigned (attachment
  before the model selector), a "jump to latest" affordance that appears
  only when the thread is scrolled away from the bottom, and layout /
  control-height tokens replacing one-off pixel values.
- `uva-app-server`: the Messages endpoint's unbound sessions
  (`anthropic-<uuid>`, minted for external clients that pin no session)
  are excluded from the management listing, so a community client's
  throwaway turns do not accumulate in the user's own Sessions page. The
  namespace lives in one `pub(crate)` constant; the listing hides the
  rows while `GET /api/v1/sessions/{id}` still serves them, and an id
  that merely shares the prefix letters is not filtered.
- `uva-app-server`: `GET /api/v1/sessions` and the session detail read the
  durable transcript store as well as the in-process registry, so a
  conversation survives a backend restart. Previously the listing showed
  only sessions created since the process started (5 of 1265 on this
  workspace) and "View Detail" 404'd for anything older; a session known
  only from disk is still 404-free and cannot escape the sessions root.
- `uva-app-server`: usage counters are recorded. `/api/v1/chat/usage`
  read three atomics that nothing ever incremented, so the header chip
  was permanently `0 in · 0 out · 0 turns`; the controller's event funnel
  now folds each `SystemEvent::Usage` into the token totals and each
  `SessionEnded` into the turn count.
- `uva-web`: the modal no longer steals focus on every keystroke.
  Its focus effect was keyed on an `onClose` prop that every render
  replaced, so the cleanup re-focused the trigger after the first
  character and a typed name kept only its first letter.
- `uva-app-server`: `?limit=` is accepted on the list endpoints. A plain
  `Option<u64>` cannot deserialize from a query string through
  `#[serde(flatten)]`, so `?limit=20` was a `400` on exactly the
  endpoints that document the parameter.
- `uva-app-server`: `list_models` compares `If-None-Match` instead of
  treating its presence as a match, and derives the `ETag` from the
  response body. Previously any `If-None-Match` returned `304`, so with
  `Cache-Control: no-cache` a browser was pinned to the first model list
  it ever cached — a redeploy never reached it.

### Added (R125 — `uva-mcp-server` stdio binary: outbound MCP, the plugin-brick tools shipped to any MCP client)

The "outbound" counterpart to R124's in-process `self_mcp`:
R124 let the agent reach into its own deployment via MCP; R125 lets
**any foreign MCP client** (the `pi` coding agent, a Claude Code
plugin, the Anthropic SDK, …) spawn `uva-mcp-server` as a
subprocess and immediately have the same plugin-brick surface under
its existing JSON-RPC 2.0 stack. No Rust in the caller's process, no
network, no API key.

- **New crate `crates/uva-mcp-server/`** — single binary
  (`uva-mcp-server`) speaking MCP over stdin / stdout. Two
  `pub` modules: `lib.rs` (`ServerSurface`, `parse_argv`,
  `bridge`, `render_output`, `overview_tool`) and `stdio.rs`
  (`run`). Same protocol core as `uva-mcp::server::serve` —
  inlined here because the stdio transport owns stdin / stdout
  directly rather than going through a loopback pair. A
  successful call against this binary is therefore also a
  successful end-to-end proof of the in-process MCP server seam.
- **Eight tools exposed by default**: `read` (`uva-tool-read`),
  `write` (`uva-tool-write`), `shell` (`uva-tool-shell`,
  default safety policy), `TodoWrite` (`uva-tool-todo`),
  `web_fetch` (`uva-tool-web`, the only one that pulls
  `reqwest`), `schedule` (`uva-tool-scheduler` over an empty
  `ScheduleStore` rooted at `<workspace>/.uva/schedules/`),
  `search` (`uva-tool-search` over an empty `uva-search
  Registry`), and `uva` (the synthetic overview tool
  returning the binary version, the workspace root, and the
  eight-name tool list in either JSON or text format). The bridge
  (`bridge`) adapts `Arc<dyn Tool>` to the `ServerTool` handler
  signature; the overview tool is built inline because it needs
  data the registry itself does not carry.
- **CLI**: `--workspace <PATH>` (default: `$PWD`,
  canonicalised, must be a directory) and `--name <NAME>`
  (default: `uva`). `--help` and unknown flags report to
  stderr and exit code 2. argv parsing is a hand-rolled matcher
  in `parse_argv`; the binary owns no third-party CLI dep.
- **One `Context` per binary** — `surface.context("test-session")`
  builds the single `Context` (`session_id` + `workspace_root` +
  default truncation) every plugin-brick tool receives, so a
  foreign MCP client's subprocess keeps the same workspace
  confinement as a `uva-app-server`-hosted agent. The
  `SharedClock` is also injected here (and threaded into
  `SchedulerTool::with_clock`) so tests pin time without racing
  the real clock; production defaults to `SharedClock::system()`.
- **`crates/uva-mcp-server/tests/stdio_roundtrip.rs`** — 5
  integration tests that spawn the binary and drive the full
  MCP wire (initialize / notifications/initialized / tools/list /
  tools/call read+write / `uva` overview / unknown-tool
  error envelope). All green.
- **`crates/uva-mcp-server/tests/pi-integration.ts`** — a
  cross-language smoke that drives the binary from Node.js the
  way a foreign MCP client does (over stdin / stdout). Prints
  `OK: cross-language smoke passed (Node ↔ uva-mcp-server)`.
  Same wire shape `pi` would speak if a `pi-mcp-adapter`
  extension wired it as a subprocess.

Verification: `cargo test -p uva-mcp-server --lib`
**5/5** (`argv_parsing_round_trip`, `plugin_brick_tools_assemble`,
`overview_tool_round_trips`, `bridge_passes_input_through_to_the_tool`,
`render_output_errors_are_surfaced_as_err`);
`cargo test -p uva-mcp-server --test stdio_roundtrip`
**5/5** (`initialize_advertises_protocol_version_and_tools`,
`read_tool_round_trips_through_stdio`,
`write_tool_creates_files_under_the_workspace`,
`uva_overview_tool_lists_every_exposed_tool`,
`tools_call_with_unknown_tool_returns_error_envelope`);
`cargo clippy --all-targets --all-features --tests --all -- -D warnings`
0; `cargo +nightly fmt --all --check` 0;
`cargo doc --no-deps -p uva-mcp-server` 0;
`make ci` (every gate: `check-mvp-deps`, `check-no-runtime`,
`check-public-api-runtime`, `check-pub-surface`,
`check-claim-language`, `check-clock`, `check-harness-shape`,
`check-test-layout`, `test-guides`) green;
`node tests/pi-integration.ts` → `OK: cross-language smoke passed`.
Reproduction: `cargo build -p uva-mcp-server` →
`./target/debug/uva-mcp-server --workspace /tmp/ws --name pi` →
send `{"jsonrpc":"2.0","id":1,"method":"initialize",…}` /
`{"jsonrpc":"2.0","id":2,"method":"tools/list"}` /
`{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"read",…}}`
over stdin and read the JSON-RPC responses off stdout.

### Added (R124 — subagent sessions become first-class; the agent manages itself via MCP)

Every `task` delegation now runs in its own session: delegated traces no
longer pollute the parent's `events.jsonl` or SSE stream, the child
session appears in `GET /api/v1/sessions` next to the user conversations
(`parent_session_id` + `agent_name` attribution on `SessionSummary` and
`SessionDetail`), and the child session stream is openable like any
other (`GET /api/v1/sessions/{id}/messages/stream`). The parent's transcript
keeps only two lightweight `subagent_enter` / `subagent_exit` marker rows
naming the child; the `task` tool result carries `[subagent session: <id>]`
so the chat can link to the child.

- `uva-harness`: `AgentMeta` gains `agent_name: Option<String>` (the
  registry name of the delegated peer) and `prompt: Option<String>` (the
  dispatch prompt — set on the **first** forwarded event of the child
  only, so a session router can persist it as the child's `UserInput`
  row without cloning it onto every event of the run).
- `uva-tool-task`: `run_subagent` sets `meta.agent_name` and
  `prompt`, and appends `\n\n[subagent session: <child_id>]` to every
  task result (success / failure / cancelled / empty) so the chat
  surface has a stable place to read the child id.
- `uva-app-server`: new `session::subagent_router::SubagentRouter`
  persists wrapped `AgentEvent::Agent` frames into their own sessions
  via `SessionRegistry::sink(user, child)` instead of inlining them
  into the parent's log; durable inner events (the child's own
  `Model(Text|ToolUse|ToolResult)`) land unwrapped, nested
  `Agent(m1, Agent(m2, …))` is handled by recursion, and the child
  session's live broadcaster is fed when a controller exists for it.
  The parent's log keeps only the depth-1 enter/exit markers; deeper
  levels' marker rows are suppressed because the wrapped `Agent` row
  in the immediate parent's log already records the delegation.
  `persist_and_broadcast` short-circuits on `Agent` events so the
  parent's SSE never carries the child's events either. `ControllerInner`
  gains `subagent_router` and `record_usage_tree` walks the
  sub-tree so child token counts still hit `/api/v1/chat/usage`.
- `uva-app-server`: `routes/sessions.rs` — `SessionSummary` /
  `SessionDetail` carry `parent_session_id` + `agent_name`; the
  registry loop reuses its single `sink.read()` for status + timestamps
  + attribution, and `scan_unregistered_transcripts` reads the first
  row of each disk transcript for the same `subagent_enter` marker.
  `ScannedSession` is a small struct that carries both the
  cursor-paginated row and the attribution.
- `uva-app-server`: `routes/tasks.rs` rebased on the new markers
  (previously walked the parent log for `{type:"Agent"}` envelopes,
  which the router no longer writes). `TaskDelegation.prompt` is read
  from the child session's `UserInput` row on detail (kept empty on
  the list to avoid the extra sink reads); status is the exit marker
  when present, else `"running"`. `TaskDetailPage` no longer 404s on a
  valid delegation.
- `uva-mcp`: an in-process **MCP server** + a **loopback transport**
  pair. `serve()` handles `initialize` / `tools/list` / `tools/call`
  with the same JSON-RPC 2.0 wire `uva::mcp::McpClient` already
  speaks (no protocol fork), and the loopback pair gives the in-process
  case the same request / oneshot-reply semantics stdio / HTTP use.
  Every error path falls through to a single `server.respond(framed)`
  so the client never hangs on an unset `id`.
- `uva-app-server`: `state::app_state::self_mcp` — the deployment's
  own management surface, served over MCP. Nine `mcp__self__*` tools
  (`agents` / `tools` / `skills` / `schedules` / `workflows` / `evals`
  / `tasks` / `models` / `attachments`), each action-discriminated
  (`list` / `get` / `create` / `delete` / etc.) and wrapping `Arc<AppState>`
  directly — `register_self_mcp` spawns `serve` over a loopback
  transport, runs the handshake, and publishes through the existing
  `register_mcp_tools(..., "self", ...)` path so the boot-time
  `mcp__*` → `Deferred` flip applies uniformly. The tool registry never
  grows per-tool code: handlers close over `Arc<AppState>`. Session /
  message endpoints are deliberately excluded — the agent managing
  its own transcript is the foot-gun the carve-out exists to prevent.
- `uva-web`: a chat **side panel** (`src/components/chat/SessionSidePanel.tsx`)
  fed by pure derivations in `src/lib/session-panel-data.ts` over the
  same `Message[]` the list renders: the latest `TodoWrite` list
  (checklist + progress bar), every `task` delegation (peer name +
  status + link to `/sessions/<child>`), and every file the agent wrote
  (`write` tool blocks, newest write per path). The panel is
  default-open and its state persists in `localStorage` under
  `uva.chatPanelOpen`; the toggle uses the right-side precedent in
  `useSidebarCollapse` (`Ctrl/Cmd+B`).
- `uva-web`: inline distinction inside `SegmentView`'s
  `tool_block` branch — `TodoWrite` calls render as a checklist +
  `n/m` progress, `task` calls as a sub-agent card with a
  `查看子会话 →` link to `/sessions/<child>`, `write` calls as a
  file chip with the workspace-relative basename. The raw call / result
  JSON body stays one click away under the existing toggle, so
  debugging and reproduction are not sacrificed for the at-a-glance
  rendering.
- `uva-web`: thinking-block visual separation — the
  `.nt-chat__segment--thinking` block gains a tinted surface, a
  dashed left accent rail, italic body, the label `思考过程`, and a
  one-line preview under the collapsed header. Together they read
  the scratchpad apart from the answer at a glance.
- `uva-web`: `SessionSummary.parent_session_id` /
  `agent_name` and the matching `SessionDetail` fields drive a
  `子代理 · <agent>` badge on every child-session card and a
  `PARENT SESSION` / `SUB-AGENT` pair on the detail page. The
  marker replay path uses the existing `notice` segment type (no
  new `SegmentType`) so both the live stream and history replay
  render the same `已委派子代理 **<agent>** · [查看子会话 →]`
  note in context.

### Changed (R124 — subagent traces no longer inlined; `task` result carries the child id; pre-existing §3.8 violations in three adjacent server files fixed)

- **Subagent traces no longer in the parent.** `AgentEvent::Agent`
  frames are now consumed by `ControllerInner::persist_and_broadcast`
  as soon as they arrive and routed into the child session's own
  sink / broadcaster; the parent sees only `subagent_enter` /
  `subagent_exit` markers. Child usage counts still roll up into
  `/api/v1/chat/usage` (a recursive `record_usage_tree` walks the
  wrapped sub-tree).
- **`task` tool result carries the child session id.** Every
  `task` result the model sees — success or failure — ends with a
  `\n\n[subagent session: <ulid>]` line. The chat surface parses this
  with `childSessionIdOf(resultContent)` to render the link.
- **Clock discipline, three adjacent server files.** `make check-clock`
  (`chrono::Utc::now()` calls in library code; baseline 6, all of
  which live in `scheduler.rs` tests) failed at 10 because four
  production sites had been calling `Utc::now()` directly:
  `state/app_state/schedules.rs:83` (`tick`),
  `state/app_state/evals.rs:115` (`generated_at`),
  `routes/schedules.rs:172` / `routes/schedules.rs:249` (create + tick
  handlers). Each now reads "now" through the injected
  `uva::core::SharedClock` — `SchedulesState` and `EvalsState`
  gained a `clock` field + a `build_with_clock` constructor (the
  existing `build()` defaults to `SharedClock::system()`) and a
  `SchedulesState::clock()` accessor so route handlers read from the
  state. No `CLOCK_BASELINE` change was needed.

Verification: `make ci` green; `make check-web` green;
`cargo test -p uva-harness --lib` **241/241**,
`cargo test -p uva-session --lib` **151/151**,
`cargo test -p uva-tool-task --lib` **76/76**,
`cargo test -p uva-mcp --lib` **28/28**,
`cargo test -p uva-app-server --lib` **477/477**;
end-to-end browser verification (uva-app-server + vite running
locally, model invoked through `/v1/messages`) confirmed the chat
panel populated, the inline checklists / sub-agent cards / file
chips rendered, the sub-agent link opened its child session page,
and `GET /api/v1/tools` listed all nine `mcp__self__*` tools with
`Deferred` exposure.

Live MCP proof (uva-app-server `:18780`, /tmp/uva-pi-test workspace):
the agent, prompted via `/api/v1/chat/sessions/.../messages`, issued
`tool_use id=call_190b3a1678c042108a0534d5 name=mcp__self__models
input={"action":"list"}`; the persisted `events.jsonl` records
`tool_result metadata.mcp_server="self"` and the exact JSON returned
came from `state.provider_catalogue` / `state.workspace_config`. A
second turn created a real skill through MCP
(`tool_use mcp__self__skills action=create name=e2e-pi-demo …` →
`tool_result metadata.mcp_server="self" text="created skill
'e2e-pi-demo'"`) — `find /tmp/uva-pi-test/.agents/skills/` shows
`e2e-pi-demo/SKILL.md` with the content the agent sent, and
`GET /api/v1/skills` lists it alongside the three bundled skills.
This proves the in-process MCP server is the same code path the
agent reaches in production (no separate `mcp-server` process, no
hand-rolled shortcut): the `LoopbackTransport` ↔ `serve` ↔
`register_mcp_tools` ↔ `ToolRegistry` chain round-trips a real
provider call and the changes show up everywhere the HTTP API
sees them.

### Changed (R123 — harness surface audit: runtime-neutral event channel, order-independent compaction, hidden-tool refusal)

An audit of `uva-harness` against its own claims (runtime neutrality,
lego assembly, minimal public surface, accuracy of the docs) found four
places where the code and the documented contract disagreed, and one
dispatch-path hole. Each is fixed at the seam the contract names, not at
the symptom.

- **The event channel is `futures`, not tokio.** `EventSink` — the only
  handle an out-of-tree `ReasoningStrategy` gets for publishing, and the
  only public constructor it had — took
  `Arc<tokio::sync::mpsc::UnboundedSender<AgentEvent>>`. That made the
  crate's most-advertised plugin seam unusable without a tokio
  dependency, contradicting `AGENTS.md` §3.7 ("事件通道用
  `futures::channel::mpsc`"). Both the sink and `Agent::run` now use
  `futures::channel::mpsc`, so the constructor costs no runtime
  neutrality and stays public for a strategy that unit-tests itself
  without an agent around it. `tokio` remains a dependency only for the
  default `Spawner` and the parallel-dispatch semaphore — neither is in
  a public signature.
- **Compaction resolution is per run, so setter order cannot lie.**
  `resolve_compaction_now` ran inside the two compaction setters and
  passed a *fresh* `TruncatingContextManager` as the decision core's
  fallback, so `.with_context_manager(MyCm).with_compaction_settings(..)`
  silently discarded `MyCm`, and `.with_compaction_settings(..)
  .with_context_manager(..)` silently discarded the policy. It is now
  `effective_context_manager`, called from `runtime_for_run` with the
  caller's own manager as the fallback: an enabled + valid policy
  replaces it, anything else keeps it, in either order. The crate's own
  `assemble_with_builder` example had been printing "summariser armed"
  while installing no checkpoint because of this; it no longer does.
- **A hidden tool is no longer callable.** `is_hidden` is the registry's
  privacy flag — `ToolRegistry::run_stream` refuses such a call — but the
  loop resolved entries through `Registry::get`, which does not filter, so
  a model that emitted a hidden tool's name got it executed. The lookup
  now refuses it with the same model-facing error the registry produces.
- **`AgentOutput` is crate-internal and no longer carries a dead field.**
  Its `events: Vec<AgentEvent>` was hardcoded empty at its only
  construction site while the crate root advertised it as "events emitted
  by any `Agent::run`"; the field is deleted and the type is
  `pub(crate)` — no public signature returns it, so a run's public
  surface is its event stream plus `SystemEvent::SessionEnded`.
- **Removed: `context_manager_for_compaction`.** A second public entry
  point whose only body called its `_with_emitters` sibling; the sole
  in-repo caller (the server's run factory) uses the sibling. `resolve`
  drops to `pub(crate)`.
- **Demoted: `AgentEntry::descriptor_mut`.** It was reachable as
  `uva_harness::agent::AgentEntry::descriptor_mut` (the type is a
  curated re-export) and self-described "test-only" — it existed so the
  registry's own tests could seed a version / tool list / owner by hand.
  Now `#[cfg(test)] pub(crate)`: a write handle on a descriptor cache
  that is otherwise write-once at registration, absent from production
  builds entirely. The same-named `ReActAgent::descriptor_mut` stays
  `pub` — it replaces a running agent's descriptor wholesale, and
  `uva-tool-task`'s tests call it across crates.
- **Module paths closed:** `config`, `input`, `prompt` and `compaction`
  are private modules; their types were already re-exported at the crate
  root, and no consumer used the module path.
- **Dependency placement:** `uva-skill` moved from `[dependencies]`
  to `[dev-dependencies]` (only `examples/assemble_with_skills.rs` names
  it, and that example exercises no harness type), and `tokio-util` moved
  to `[dev-dependencies]` (every use is inside `#[cfg(test)]`). Both were
  compiled into every harness consumer for no library-side reason.
- **`make check-public-api-runtime` is now a script.** The previous
  one-line recipe scanned for `pub ` plus `tokio::` on the *same* line,
  which misses the ordinary leak: `use tokio::sync::mpsc;` binds `mpsc`,
  and `pub fn new(tx: Arc<mpsc::UnboundedSender<..>>)` names no runtime
  crate at all. The gate resolves each file's runtime `use` binders
  (including `as` aliases) and follows a public item across its signature
  lines. Verified against a synthetic fixture with both leak shapes, and
  against the tree the fix left clean.
- **`DetachedAgent::join` no longer runs the agent twice.** It took the
  event stream out of its slot and dropped it, so a second `join`
  re-entered `Agent::run` and re-executed every tool. The stream is now
  kept and the whole lifecycle (idle / running / done / exhausted) sits
  behind one lock, so a *concurrent* join replays the terminal event
  rather than reporting `EmptyStream` for a run that succeeded — the
  three correlated facts used to be three locks, and a queued caller
  could observe a finished run as an exhausted stream. The module docs also stop
  claiming the run starts at `spawn_detached` (it starts at the first
  `join` — the type holds no executor) and the
  `clippy::await_holding_lock` allowance whose comment described the
  opposite of the code is gone.
- **`DetachedAgent::join` leaves the handle clean when abandoned.** The
  single-lock rewrite installed a placeholder before awaiting
  `Agent::run`, so a caller that dropped its `join` future mid-await left
  that placeholder behind and the next `join` reported `EmptyStream` for
  a run that had never started. `Agent::run` may suspend before yielding
  its stream (`ReActAgent::run` happens not to), so the slot is now
  mutated only *after* the stream exists, with no await between the two
  writes.
- **`DetachedAgent::try_event` can now report closure.** `DetachedClosed`
  was unreachable: nothing ever cleared the receiver, and the handle held
  the sender for its whole life, so the error arm could not fire and its
  catch-all also swallowed the genuine closed signal — `Ok(None)` meant
  both "nothing yet" and "never again". The sender now lives inside the
  run's state and is dropped on the transition to `Done`/`Exhausted`, so
  a finished run yields its buffered tail and then reports
  `DetachedClosed`, giving a polling caller a terminating loop. The
  vestigial `Option` wrapper on the receiver went with it.
- **Docs corrected where they contradicted the code:** the crate root
  claimed prompt sections "identity / tool / skill / agent / rules" (the
  assembler renders identity / skills / agents and deliberately keeps
  tool schemas off the prompt); the harness README listed
  `ToolInterceptor`, steering and compaction as "deliberately not here"
  while the crate ships all three; `Agent::run` was said to detach "one
  task per **turn**" (it detaches one per **run**);
  `spawn_detached` was said to "spawn the agent's event stream into a
  background buffer" and the module layer tables called `AgentHandle`
  "background execution" — the run starts at the first `join` and the
  handle holds no executor, so no task is detached at `spawn_detached`
  at all (0 sites left; `uva-tool-task`'s `GroupJoin` and
  `SubagentPool` use the word for their own lanes, which is accurate);
  `runtime_context.rs` said the production loop "passes
  `chrono::Utc::now()`" (it reads the injected `SharedClock`, which is
  what §3.8 requires); the event enum's doc said "five top-level
  variants" for a four-variant enum; and `AgentRunConfig`'s rustdoc
  opened mid-sentence. Six live docs also named a crate that does not
  exist (`uva_agent::`) in copy-pasteable snippets — now
  `uva_harness::`.
- **`run`'s tests moved to a sibling `tests.rs`.** `handle.rs`'s inline
  block had grown past the 400-line hard limit
  `make check-test-layout` is specified to enforce (the gate currently
  runs a temporarily relaxed limit — see its `RESTORE HERE` marker). Per
  the repo's rule for a module that outgrows it, `handle.rs` +
  `inbox.rs` tests now live in `agent/run/tests.rs` — a sibling file,
  not a same-named subdirectory. No production item had to widen its
  visibility for it: the "nothing runs until the first `join`" contract
  is asserted through the run counter, not the handle's internals.
- **A panicking tool no longer truncates the run silently.** The loop
  drove `Tool::stream` itself, without the `catch_unwind` the registry's
  own dispatcher applies — so a panic in a tool unwound the run's
  detached task, dropped the event sender with it, and the caller
  watched the stream end with **no** `SessionEnded`, unable to tell a
  finished session from a dropped connection. Reproduced first
  (`["System","System","Model","System","ModelDone"]`, no terminal
  event), then fixed. Thirteen call sites now run under a guard, through
  `crate::agent::re_act::{catching_panics, catching_panics_sync}` — the
  async form for awaited seams and the sync form, named distinctly so a
  reader can tell which is in play, for the plain-method ones. Guarded:
  the `Tool`, the `ToolInterceptor` plugin (which returns *before* the
  registry path, so it needed its own guard), the `ModelProvider`, the
  consumer-supplied `ContextManager`, the `RunInbox`, and `steering`'s
  `Hint` (both injection points) + `Tracker` (all four methods). The
  extractor stays shared in `uva_core::panic_message`; the guards
  themselves live in the harness, which is their only user. Adding the
  `Hint` guard surfaced a second defect on the same call site:
  `inject_hints` evaluated every hint's `should_trigger`/`generate`
  *before* checking its `injection_point()`, so an `AppendToToolResult`
  hint ran a consumer's `generate` once per iteration and had the result
  discarded — `hint.rs` documents that injection points are honoured.
  The point is now checked first, and the test pins the routing (that
  hint is evaluated exactly once, at commit time). `agent_impl` additionally wraps the whole `strategy.run(..)` call
  — the only guard that reaches a panic in the loop's *own* code
  (bucketing, commit, hook fan-out), which no inner guard can.
  Three deliberate differences from the tool case: a panicking
  `ContextManager` ends the run **reported** rather than resuming,
  because `prepare` rewrites the message list in place and a
  half-truncated transcript is what the next provider call would send; a
  panicking `RunInbox` or `Hint`/`Tracker` skips its own work, since the
  steering crate already isolates its guards and hooks the same way; and
  a panicking `ModelProvider` necessarily ends the run, there being no
  response to carry on with. Five regression tests pin it, each verified
  to fail without its guard.
- **A finished run reports nothing after `SessionEnded`.** The
  cancelled-before-tool path finalizes *inside* the iteration — it emits
  `SessionEnded` and fans out `OnAgentEnd` inline, and the orchestrator
  returns that output verbatim — but the close phase ran anyway. With
  steering queued mid-sample, that appended user turns to a terminal
  history and emitted `SteeringInjected` *after* the terminal event,
  contradicting the crate's own contract that `SessionEnded` is what
  tells a consumer the run is over (reproduced: `SessionEnded` was not
  the last event). The close phase is now skipped for an already-reported
  run, and the typed `iteration_end` that would have been skipped with it
  is emitted on the cancel path instead — with the accurate action
  `"cancelled"` rather than the `"tools_complete"` the close phase would
  have mislabelled a run in which no tool ran.
- **A failed sample keeps its reason.** `sample_or_fail` logged the
  `SessionEndReason` and returned `None`, so the caller substituted
  `"sample_once returned no outcome"` — every sample failure, including
  an HTTP error and a panicking provider, reached `SessionEnded` as that
  same opaque string. It now returns the reason it was given.
- **`SystemPrompt`-point hints behave as documented: once, at session
  start, per hint.** `SessionState::system_hinted` existed to enforce
  "appended to the system message once at session start" but nothing ever
  set it, so the guard around `append_to_system_prompt` was a constant: a
  hint triggering every iteration re-appended itself each time
  (reproduced: marker count 2 across two iterations), growing the system
  message — the prompt-cache stability this crate keeps volatile facts
  out of the system prompt to protect. Fixing that exposed a second
  defect in the same three lines: one boolean cannot serve *several*
  `SystemPrompt` hints, because the first to fire would permanently
  silence the rest. The state is now a set of injected hint **names**, so
  each injects once, and the append is gated to iteration 0 to match the
  documented "at session start" rather than mutating the cached prefix
  mid-run (no shipped hint uses this point; a late-firing one belongs at
  `BeforeNextLlmCall`). Two tests pin it: one hint injects exactly once
  across two triggering iterations, and two hints both do.
- **A panic's message is no longer thrown away.** `panic_message` was
  called as `panic_message(&payload)` in the registry dispatcher. A
  `Box<dyn Any + Send>` is itself `Any`, so `&payload` unsized to
  `dyn Any` *over the Box* and every downcast missed: the text was
  always the opaque fallback. The canonical test read that as
  "implementation-defined", documented it as such, and asserted only the
  wrapper — while the trait's own docs promised the output carried "the
  panic message". Passing the payload by value restores it, and the
  canonical test now asserts the message survives (fails without the
  fix).
- **One panic extractor, where there were four — and one fallback, where
  there were three.** The payload-to-string conversion existed in
  `uva-tool`'s dispatcher, `uva-steering`'s hook runner, its
  guard runner, and inline in `uva-app-server`'s error middleware, with
  fallbacks `"<non-string panic>"`, `"unknown panic payload"` (twice) and
  `"unknown panic"`. All four now call
  `uva_core::panic_message`, whose fallback is the named
  `NON_STRING_PANIC` = `"(non-string panic payload)"`. **Behaviour
  change:** logs that used to contain `unknown panic payload` or
  `unknown panic` now carry `(non-string panic payload)`, so a grep for
  the old strings will find nothing. The unified signature takes
  `Box<dyn Any + Send>` **by value** on purpose — a reference parameter
  re-admits the `&payload` bug above, which compiles and silently
  degrades; by-value makes it unrepresentable at the call site.

- **A dropped event is now observable.** The detached run's event buffer
  is bounded, and the drive loop discarded every overflow with `let _ =
  try_send(..)` — so a run that outran its poller silently produced a
  truncated transcript that looked complete. Each drop is now counted
  and reported once, with the count and the requested capacity, when the
  run ends. The capacity itself is documented as *requested*: the
  channel's real bound is `buffer + num_senders`, so the warning names
  the number to raise rather than restating the arithmetic.
- **Tests:** new `agent::re_act::tests::compaction_wiring` pins the
  setter-order contract (all three tests fail against the pre-fix code);
  `loop_lifecycle` gained a hidden-tool refusal test, and the detached
  handle `join_twice_runs_the_agent_once` plus a concurrent-join test
  that reproduces the `EmptyStream` report; `loop_lifecycle` gained
  `panicking_tool_still_reports_session_end` and
  `panicking_interceptor_still_reports_session_end`, and
  `uva-tool`'s `tool_panic_isolation` now asserts the panic message
  survives — each verified to fail against the pre-fix code. The four tautological `AgentOutput` tests
  (field construction, `Clone`/`Debug` presence, `Vec` ordering) were
  deleted with the field they pinned.
- **Poisoned-lock panics removed from the loop's hot paths.** The
  streaming accumulator, the detached handle's state, and the run inbox
  now use `parking_lot::Mutex` (already a dependency) instead of
  `std::sync::Mutex`, deleting all nine production
  `expect(.. "poisoned")` sites — several of them on the per-chunk path,
  where the panic would have terminated the run with no `SessionEnded`
  to report it with.

Verification: `cargo test -p uva-harness --lib` **233/233**;
`cargo test -p uva-app-server --lib` **398/398**; `make ci` green;
`make examples` green; `scripts/check-public-api-runtime.sh` green
(plus the harness-shape script and the inline gate targets).

### Changed (R122 — the harness gets a shape: layered modules, a private surface, and the `team` layer deleted)

`uva-agent` had grown a public surface that was really internal
structure. `agent::` exposed ten modules including `re_act`'s own
submodule tree, an out-of-band "team protocol" layer no caller in this
repo used, and a subagent-batching type whose only consumer lives in
another crate. The round answers one question — *what is the harness,
and where does the rest go?* — and each change below is an answer, not
a tidy-up.

**The `team` layer is deleted (1,116 lines: 660 production, 297 of its
own tests, 159 of example).** `agent::team` shipped `BoundAgent` (a name plus one async
`prompt -> text` call), `FnAgent`, `Verifier` / `Verdict` /
`VerificationChain` / `ChainOutcome` (generate, verify, retry with the
rejection reason appended), and `RoundRobinGroupChat` /
`TerminationCondition` / `MarkerTermination` / `MaxRoundsTermination` /
`Turn` / `StopReason`. It was adopted from `traitclaw-team`'s
`{lib,execution,group_chat}.rs` and it was correct — but *correct code
with no caller is weight*, and each of its three pieces has a
first-class home elsewhere in this workspace:

- **Delegation is the `task` tool.** `TaskDelegator` in
  `uva-tool-task` runs a registered peer as a depth-capped child,
  forwards its events, and commits its final text — with gates,
  worktree isolation and admission control the `prompt -> text` seam
  never had. `BoundAgent`/`FnAgent` were a *second*, thinner
  multi-agent vocabulary in a workspace whose §3.7 rule is that new
  synthetic tools become plugin crates implementing `ToolInterceptor`.
- **Verify-then-retry is a scorer.** `CandidateScorer` behind
  `BestOfNStrategy` (or `LlmJudgeScorer`) samples N answers and keeps
  the best; a `uva-workflow` gate re-runs a call until its command
  exits zero. `VerificationChain` was a third mechanism for the same
  job, with a *different* retry policy (`max_attempts` + a
  `with_feedback` flag) that nothing outside its own tests selected.
- **A stop condition is the loop's.** The ReAct loop stops on
  `max_iterations`, a follow-up drain, or cancellation; a round budget
  or a `DONE` marker scanning another transcript is a second stop
  vocabulary nothing in the repo composed.

The outward-facing proof that nothing was lost is that the workspace
still declares every one of those capabilities — in `uva-tool-task`,
`uva-workflow` and the strategy seam — and the whole workspace
compiles with zero references to the deleted names. `docs/reference-parity.md`
now records the two declined halves with their reasons instead of
mapping them onto types that no longer exist.

**`GroupJoin` moved to `uva-tool-task`, where its consumer is.**
Batching a fan-out's completions into one notification is a thing *a
host that runs subagents* wants; a host that does not has nothing to
batch. It was in `uva-agent` next to the loop, and its own example
had to name `AgentHandle`/`DetachedAgent` from a different module to
explain itself. Both moved, and both now say what they are for:
`pool` decides *whether* a child starts, `GroupJoin` decides *when the
host hears about it*. `uva_harness::GroupJoin` is gone; import it
from `uva_tool_task`.

**The three reasoning strategies moved to `agent::strategy`.** They were
`agent::{cot, best_of_n, best_of_n_llm_judge}` — siblings of the *seam*
they implement rather than members of it, so the module that documents
"`ReActAgent` is one way to reason; this module makes that a choice"
sat one directory away from the two other choices. Now `strategy/{mod,
runtime, sink, request, scorer, cot, best_of_n, llm_judge}.rs`, with two
extractions that the move made obvious:

- `CandidateScorer` + `LongestAnswer` are `scorer.rs`: the best-of-N
  extension point is *how candidates are judged*, and it was buried in
  the strategy that consumes it (file was 683 lines, of which 337 were
  tests).
- The one-pass request both single-shot strategies ask is `request.rs`.
  Cot and best-of-n built the same `CompletionRequest` — same prompt
  assembly, same history, same window fit, same `ToolChoice::None`,
  same `max_tokens` — in two copies that differed only in an appended
  instruction. One function, `single_shot_request(runtime, input,
  instruction)`, so the two cannot drift on the three decisions that
  make them what they are.

**Every submodule of `agent` is private now.** Its
`{descriptor, interceptor, re_act, registry, run, spawn, strategy}` are
`mod`, not `pub mod`, and `agent/mod.rs` re-exports the names they are
*for*. (The five crate-root modules — `events`, `input`, `config`,
`prompt`, `compaction` — stay public: they are the pieces a consumer
names directly.) Two consequences
worth the churn: internal structure stops being public API (adding a
`pub` item inside `re_act::loop_` can no longer widen the contract by
accident), and the module root becomes a readable map — the doc comment
is a table of *contract / loop / reasoning / driving a run / catalog /
plugin seam* rather than ten module links. The intra-crate and
cross-crate doc links that named the old deep paths were repointed at
the re-exports in the same pass, because `make ci`'s `doc-check` runs
rustdoc with `-D warnings` and a prose link to a private module is a
hard error.

**Run control got its own layer.** `AgentHandle`/`DetachedAgent`
(`handle.rs`) and `RunInbox`/`MpscInbox`/`RunInboxHandle`
(`run_inbox.rs`) answer one question between them — *what can the
outside world do to a run that is already moving?* (start it in the
background and abandon the wait without cancelling the work; push
messages into it) — and neither knows which strategy is running, nor is
either needed to run an agent. They are `agent::run`.

**`uva-tool::Context` lost three dead fields.** `dispatch_mode`
(with the whole `DispatchMode` enum: `Fork` / `Teammate` / `Worktree`,
whose `Teammate` variant is the last trace of the deleted team layer),
`caller_agent`, and `messages` (+ `with_messages`) were never set by any
runtime and never read outside tests. `messages` was the worst of the
three: its doc comment promised that "context-aware tools (e.g.
`self_reflect`)" could review the session history — a tool that has
never existed anywhere in this repo. A field that documents a capability
nobody implemented is worse than no field. `Context` is now three fields
(session id for output-spill attribution, workspace root for path
confinement, output truncation config), which is exactly what the
paradigm needs and what §3.7's "the context is the same for every tool"
rule permits.

**The harness shape became measurable — because it was not.** The
round turned up that `clippy.toml`'s `cognitive-complexity-threshold =
20` cannot be the reason this loop is split: `clippy::cognitive_complexity`
is allow-by-default, and its own docs now say it no longer measures what
its name suggests — it charges **decisions only** (no nesting, no loops,
no `?`, closures measured as separate items). Measured against that
algorithm, nothing in `agent::{re_act, strategy}` comes near the
threshold: the maximum was `handle_chunk` at **8/20**. `drive.rs`'s claim
that its helpers are "kept thin to satisfy the cognitive-complexity
threshold" was therefore unbacked. What the harness *does* maintain is
**flat nesting and short functions**, and neither was measured. Four
real offenders were fixed, each by naming the thing that was being
repeated inline:

- `handle_chunk` (**121 lines**, the longest fn in the tree) is now a
  7-arm match of one-line arms over named handlers — `absorb_content`,
  `open_tool_buffer`, `absorb_is_done`, `push_tool_use`. The last one
  matters beyond length: recording one tool call took the same three
  steps (push to the history parts, publish on the wire, mark the id
  seen) at **three** sites — the streamed `ToolCallEnd`, the `IsDone`
  re-listing, and the early-termination drain. The first two now share
  `push_tool_use`, so the "a `tool_use_id` is never recorded twice"
  invariant holds in one place for every path that can violate it. The
  drain keeps its own copy **on purpose**: it runs after `seen_tool_ids`
  has been taken out of the state, so its dedup is a filter against the
  already-removed set rather than an insert, and routing it through the
  helper would need the set back.
- `pre_execution_veto` and `apply_guard_pipeline` repeated a denial tail
  inline; the restriction and guard sites were the same two statements,
  and the hook site was three (tracker, the `notify_error_hooks` await,
  then the error). They now share `denied(this, tool_name, call, state,
  message)` in `seams.rs` — "a refused call is always recorded and always
  explained" is one rule, not copies of the same tail. The hook site
  still calls it *before* the await, preserving the original order (the
  tracker recorded the denial before the error fan-out); that ordering is
  easy to lose when consolidating and is now pinned by
  `hook_block_surfaces_as_error_result`, which installs a tracker-counting
  `AgentHook` and asserts the count is non-zero inside `on_error`.
- `sample_once` built its `CompletionRequest` literal (20 lines, 10
  fields) and its 8-field result log inline; those are
  `completion_request(..)` and `log_sample_result(..)`, leaving the pass
  itself as the pipeline it documents (notify → stream → observe).
- `drive.rs` rebuilt the `IterationDispatch { … }` literal at five sites;
  it is now intent-named constructors (`continuing` / `pruned` /
  `truncated_batch` / `early` / `completed`), which also makes the
  *reason* a dispatch differs visible at the call site.

**Two rounds of documentation rot got a compile-time fix.** `MINIMAL.md`'s
Step-3 table tells a consumer which setter to call, and it had been
hand-repaired once before (R47): its cells are inline code spans, invisible
to rustdoc, so nothing ever compiled them. R122 found **five** more stale
names there (`.spawner` / `.typed_event_sink` / `.output_schema` /
`.compaction_settings` / `.run_inbox` — each lost its `with_` prefix in
R110) and **eight** in `SEAMS.md`, the seam index whose whole purpose is to
say which call to write (`.context_manager`, `.steering`,
`.typed_event_sink`, `.tool_restriction`, `.strategy`, `.spawner`,
`.run_inbox`). Every one of those bare names *resolves* — they exist as
getters — so a naive existence check would have passed them while the
recipe could not type-check.

The fix is not another hand-repair: `MINIMAL.md`'s Step-3 recipes are now
transcribed into one **compiled fence** (the file's `rust` fences are
doctests via `include_str!`), so renaming a setter breaks the build.
Verified to have teeth — restoring the old names fails with `E0061: this
method takes 0 arguments but 1 argument was supplied`, exactly the
getter-vs-mutator error. `make test-guides` runs the fences under both the
default features and the seven-feature MVP subset, and `make ci` now
includes it: `doc-check` only *generates* docs, it never *compiled* an
example, so before this the guide's recipes reported green locally no
matter how wrong they were. The workflow's own raw-cargo doctest step in
the `tests` job is deleted rather than kept — it ran the same two
commands, so CI would have compiled the fences twice per run, and it was
the last gate not invoked through a `make` target (AGENTS.md §3.6). `SEAMS.md` has no such mechanism and stays
hand-checked, recorded in `docs/README.md`'s Known gaps with the reason
the obvious gate cannot work.

`make check-harness-shape` keeps it that way: no `uva-agent`
**production** function over 100 lines or 4 levels of nesting
(`too_many_lines` + `excessive_nesting`, the two lints clippy's own doc
recommends in place of the inert one), scoped to `uva-agent` on
purpose — a workspace-wide run at this threshold reports **24** sites,
of which **14 are production code** (the other 10 are benches, examples
and test files, which `--lib` excludes anyway), and this round touched
none of them; the longest is `run_task.rs`'s 415-line handler. Making
those hard errors would be an unrequested refactor.

Scoping is not cosmetic here, and getting it wrong the first time is
worth recording. Writing either threshold into the root `clippy.toml`
**enables the lint for every crate**, so with `make lint-rust`'s `-D
warnings` it turned an untouched `uva-core` block into a hard error.
The thresholds therefore live in `scripts/harness-shape/clippy.toml`,
which the gate injects via `CLIPPY_CONF_DIR`; three facts about this
arrangement were each verified rather than assumed, because each was
wrong on the first attempt:

1. `excessive_nesting` defaults to **0 = disabled**, so a threshold is
   what makes the lint able to fire at all; and a threshold at the root
   enables it globally (proved: `-D warnings` on `uva-core` exits 0
   without the key and 101 with it).
2. `CLIPPY_CONF_DIR` is honoured (proved: the same 7-level function
   reports 0 diagnostics without the variable and 1 with it).
3. cargo does **not** fingerprint `clippy.toml`, so the gate `touch`es
   the crate root before linting; without that it replayed a cached
   verdict.

The gate's three failure modes are each proven: a 125-line function
fails with its location, a 7-level nested function fails with its
location, and a crate that does not compile fails explicitly rather than
passing vacuously (an earlier revision *did* pass on a broken tree — the
`2>&1 | grep` form swallowed cargo's exit status).

Moving the thresholds also meant chasing every pointer to them, since
AGENTS.md §3.4 requires a config change to update its own reference in
the same change: §3.4 gains the gate-scoped `clippy.toml` as a baseline
file in its own right, §3.5 now names it as the shape budget's single
source of truth, §3.6's gate row and its drift rule both list it, and the
Makefile comment says why the thresholds are *not* at the root. A reader
following any of those pointers lands on the file that is actually read.

**Verification** — `cargo +nightly fmt --all` clean; `cargo clippy
--all-targets --all-features --tests --all -- -D warnings` clean;
`cargo doc --no-deps --workspace` under `RUSTDOCFLAGS="-D warnings"`
clean (three broken intra-doc links found and fixed);
`cargo check --workspace --all-features --all-targets` clean; `make ci`
exits 0 over its **12 prerequisites** (10 print an `OK:` line; `fmt-check`,
`lint-rust`, `doc-check` and the new `test-guides` print their tool's own
output), including the new `check-harness-shape`; `make examples` green
(all 39 example targets, plus both consumer crates: `CONSUMER-PROOF: OK`,
`MVP-OK`).

Tests. Both columns are **measured**, not derived: the "before" figures are
`cargo test -p <crate> --all-features` run in a worktree at `5216113e^`,
the "after" figures the same command here. Both totals include doctests.

| Crate | Before | After | Composition |
|---|---|---|---|
| `uva-agent` | 254 | 232 | −10 `agent::team` tests and −2 team doctests (deleted with the module), −10 `agent::group_join` tests and −1 group_join doctest (moved to the plugin), +1 `scorer.rs` test = −22. Re-pathing `best_of_n` / `best_of_n_llm_judge` / `cot` under `strategy`, and `handle` / `run_inbox` under `run`, renames 32 test names without changing the count |
| `uva-tool-task` | 72 | 83 | +11: the 10 tests and 1 doctest that arrived with `group_join` |
| `uva-tool` | 226 | 216 | −10, exactly the `DispatchMode` / `caller_agent` / `with_messages` test fns (`git diff` lists them); doctests unchanged at 4 |
| `uva-app-server` | 455 | 455 | unchanged: `git diff` over the crate shows no test function added or removed, only module paths in doc comments and `use` lines |

The `uva-agent` delta reconciles against the test-name diff, but the
raw counts must be read with the renames factored out: 55 names gone and
33 added, of which **32 are renames** (the same test under a new module
path — `best_of_n` / `best_of_n_llm_judge` / `cot` under `strategy`,
`handle` / `run_inbox` under `run`). Matching on the leaf test name leaves
**23 genuine removals** — 10 `group_join` tests, 10 `team` tests, and 3
doctests — and exactly **1 genuinely new** test (`scorer.rs`). Quoting the
raw 55/33 as removals/additions would overstate both by 32. (After the
round the crate has no runnable doctests; the one entry `--list` still
shows for `prompt/mod.rs` is `rust,ignore`.)

### Added (the two crates nobody could call become tools)

`uva-mcp` and `uva-scheduler` were complete library crates with
no model-facing surface. An MCP server's tools reached the model only
if the *host* had published them at boot, and a scheduled job could not
be created from a conversation at all. Both gaps are the same shape —
*a capability the framework has and the model cannot reach* — and both
are closed by an adapter crate/tool, not by widening the harness.

**`uva-mcp` gains `McpControlTool` — the `mcp` tool.** The existing
path publishes one local `Tool` per remote tool
(`register_mcp_tools`), which is right when the catalog is known at
assembly time and wrong when it is large, volatile, or discovered
later. `McpControlTool` is the dynamic counterpart: one tool with three
actions over a shared `McpSupervisor` — `servers` (each server's state
and registered-tool count), `tools` (one server's live `tools/list`,
schema included, optionally filtered to one tool), and `call` (invoke a
remote tool by its **server-side** name). `register_mcp_control_tool`
publishes it.

The tool cannot exceed the host's authority: it is bound to a
supervisor, so it only ever sees the servers the host already
supervised — no spawn, no socket, no registration. Unknown server and
disconnected server get different, corrective messages, because the
fixes differ. The remote-result projection (`isError`, and the
image/audio `ContentPart` mapping added in R71) moved into one
`output_from_result` that both `McpTool::call` and the control tool
use, so the two paths cannot drift.

An `isError` reply keeps exactly one shape: both paths now name the
failed call through the same helper, so a remote failure reads the same
whether it was addressed as `mcp__srv__echo` or as `<server>/echo`.

**The dynamic path is a second access path, not a way around the
guard** — and closing that turned up a pre-existing defect in the
registered path. `tools`/`call` bypass the registry entirely, so
`[tools] hidden = ["mcp__srv__x"]` would have stopped being enforceable
for remote tools: the flag is read nowhere on that route. The tool now
holds a **weak** handle on the registry the agent loop dispatches from
(weak because the registry owns the tool — a strong handle is a
reference cycle that makes both immortal) and consults exactly the flag
`run_stream` consults: `is_hidden` refuses it. `ToolExposure::Hidden`
deliberately does *not*, because the paradigm crate documents it as
advertisement-only and still executable — a tool that treated the two
the same would have been a second, stricter gate, which is its own bug.
The membership flag is read through the naming policy the supervisor
already applies, via a `pub(crate) McpSupervisor::naming`.

The defect that turned up: **a re-sync silently un-hid the deployment's
tools.** `swap_generation` rebuilt each entry with `ToolEntry::new(..)`
— registration defaults — after unregistering the previous generation,
so every reconnect or `tools/list_changed` reset `hidden` (and any
`exposure`) that an operator had set. `sync_mcp_tools` now captures the
visibility each surviving name carries *before* the swap and re-applies
it, so a policy decision outlives the connection it was made during.
Pinned by `a_resync_keeps_the_deployments_visibility_settings`.

A wrong premise was also retired in the example: `McpSupervisor::tick`
publishes the catalog into **whatever registry it is handed**, so
"the remote catalog is not pre-registered" was false whenever the real
registry was passed. `mcp_control_tool` now ticks a scratch registry,
which is the configuration its claim actually describes (the agent's
surface is the single `mcp` tool; the catalog is read on demand), and
the doc says the contrast is one of configuration, not of reach.

**`uva-tool-scheduler` (new crate) — the `schedule` tool.**
`Scheduler` is deliberately host-driven: `tick(now)` reports due jobs to
whoever drives it. Nothing in `uva-scheduler` is a `Tool`, so the
adapter is a new plugin crate following the `uva-tool-*`
convention: `list` / `create` / `pause` / `resume` / `remove` over a
shared `ScheduleStore`, jobs addressed by id **or** name, `Tool::call`
in, `ToolOutput` out.

Three decisions worth naming:

- **The tool plans; the host delivers.** `create` writes a row and says
  so in its result; no action fires, waits, or spawns. A tool that
  fires its own payload would be a second delivery path beside
  `Scheduler::tick`, and the two would diverge.
- **`cron` is refused, not accepted.** The scheduler crate persists a
  cron expression but cannot compute a recurrence (`Job::advance` uses
  a one-minute placeholder and its own docs say the host parses), so
  `create` offers `interval` + `interval_seconds` and `once`. Accepting
  `cron` would have produced jobs that silently fire every minute — the
  schema and the enum now agree with what the crate can honour.
- **The clock is injected.** `SchedulerTool::with_clock` takes a
  `SharedClock`, so a test pins "now" and gets deterministic
  `first_fire_at` / countdown rendering; `new` uses the system clock.
  This is also why `create` reports both the RFC 3339 instant and a
  human phrase.
- **The payload has a documented shape.** `Job::payload` is free-form
  by design (the crate stores and re-emits it), so a bare `payload`
  argument would leave the model with nothing to send. The tool names
  the convention `{"agent": "<peer>", "prompt": "…"}` — `TaskSpec`'s own
  vocabulary, the reference consumer of `Job::on_fire` being a subagent
  notification — in both the crate docs and the JSON Schema description.
  It is a convention, not validation: the host interprets the payload.

**Where it is not wired, and why that is deliberate.** Neither the
`mcp` tool nor the `schedule` tool is installed by `uva-app-server`.
For `mcp`, `register_configured_mcp_servers` ticks its supervisor
exactly **once** at boot and drops it — only `healthy_clients()` and
`tick.health` escape, and `AppState::mcp_health` is that frozen
snapshot — so there is no driver loop and no `Arc<McpSupervisor>` to
hand the tool; a retained supervisor would report boot-time health
forever. The tool's docs now state that a host installing it must drive
`tick`. For `schedule`, the server has no scheduler at all (by R29's
own note, that wiring is a separate concern) and would additionally need
a store-path config key. What the deployment already has is the
pre-registered path: `tick` publishes every configured server's tools
into the server's registry at boot. Both new tools are
consumer-assembled, and `MINIMAL.md` / `SEAMS.md` carry the
registration lines.

**Naming.** The crate is `uva-tool-scheduler` (not
`-schedule`): the `uva-tool-*` crates name the *domain* they adapt
(`-todo` ships `TodoWrite`, `-web` ships `web_fetch`), so mirroring the
crate it adapts — `uva-scheduler`, whose tool is `schedule` — is
the consistent choice.

**A remotely triggerable host DoS, closed.** `create` accepted any
`u64` for `interval_seconds` (`minimum: 1`, no maximum). The chain then
ran in the **host**, not in the tool call: `create` wrote the job,
`Scheduler::tick` called `Job::advance`, and `advance` did
`interval.as_secs() as i64` into `chrono::Duration::seconds` — which
panics above `i64::MAX / 1000` (~9.2e15). Because the failed advance is
never persisted, the panic repeated on every subsequent tick. A value
above `i64::MAX` was the quieter variant: `as i64` wrapped negative,
leaving a past `next_fire_at` and a job that re-fires forever. Both are
now refused at `create` against a new documented bound,
`uva_scheduler::MAX_INTERVAL_SECS` (10 years — far beyond any
scheduling horizon, ~13 orders of magnitude below the limit), with the
schema carrying the matching `maximum`.

The complementary half is in the scheduler crate, which is where the
panic-ability actually lived: `Job::advance` is now **total** — every
arithmetic step is checked (`chrono::Duration::try_seconds`, and
`checked_add_signed` for the instant, which panics on overflow too), with
`ScheduleError::{IntervalOutOfRange, NextFireOutOfRange}` for the two
failures. `Scheduler::tick` also stopped papering over the error it was
handed: `let _ = j.advance(now)` used to emit a fire event for a job
that had *not* advanced, which is a false claim about the schedule — it
now logs at `warn` and skips that firing. Reproduction before the fix,
via the tool alone: `create interval_seconds: 1e16` returned success and
the next `Scheduler::tick` panicked in `chrono`; both are now regression
tests (`create_refuses_an_interval_that_could_panic_the_host`,
`out_of_range_interval_is_an_error_not_a_panic`), and the boundary value
itself is pinned as schedulable.

**`Completed` is terminal, and the tool now says so.** `pause` on a
completed job made the row un-reapable — `gc_completed` only ever
collects `Completed` rows, so a paused ex-completed job leaked on disk
forever — while `resume` set `Active` on a row that kept its original
past `next_fire_at`, re-delivering a one-shot that had already fired.
Both are refused with a message naming the alternatives (`create` a new
job, or `remove` the row), pinned by
`a_completed_job_cannot_be_paused_or_resumed`, which also re-asserts the
row stays sweepable.

**Parking, not skipping, when a job cannot advance.** Refusing the
absurd cadence at `create` is the first line; the second is what
`tick` does when a job still cannot advance (the library API accepts an
interval the tool refuses). My first version skipped the firing and left
the job `Active` with its stale `next_fire_at` — which is worse than it
sounds, and the reproduction is one line: `next_deadline` then reports
an elapsed deadline (`Some(0ns)`), so a host arming its timer from it
re-ticks immediately, and every pass takes the per-job file lock and
rewrites the row. That is a disk-write hot loop rather than log noise.
`tick` now sets `JobStatus::Paused` in the same `update` closure: not
due, out of `next_deadline`, visible to `list`, addressable by
`remove` / `resume`, and the `warn` fires once instead of per pass.
Deliberately **not** `Completed` — not to dodge the sweep (with the
stamp taken only on success, a `Completed` row would fail
`gc_completed`'s `last_fired_at.is_some_and(..)` filter anyway), but
because `Completed` *means* "this unit of work fired" and is terminal:
`list` must not report a job that ran nothing as finished work, and the
tool layer refuses to pause or resume a completed row. Pinned by
`an_unadvanceable_job_is_parked_not_left_due`, which asserts
`next_deadline` is `None`, the status is `Paused`, no fire time was
recorded, and the row stays unsweepable.

**The model cannot write unbounded state.** Every `create` is a job file
on disk and nothing in-tree reaps them (`gc_completed` collects only
`Completed` rows, and only when the host calls it), so a model looping on
`create` grew `<root>/schedules/` without limit. `MAX_JOBS` (64) caps it
before the write, and the model is told the limit in the tool's
description — the same shape `uva-tool-todo` uses for
`MAX_TODO_ITEMS`. Unlike the interval bound this one has no schema slot
(`create` adds one job per call; no argument carries a count), so the
description is where it lives.

**Two error messages that pointed the wrong way.** A duplicate name was
surfaced through the store's generic `Io` category ("create failed:
schedule store I/O: duplicate schedule name: X"), which reads to the
model as an infrastructure failure; `create` now checks `has_name` first
and says "A job named `X` already exists — including a completed one,
whose name stays taken. Use `remove` on it first, or pick a different
name." That also fixes the remedy in the terminal-status refusal: it had
said "use `create` for a new job", which *cannot* work while the
completed row holds the name, so it now names the order (`remove` first).
Both pinned, the second by a test that actually performs the sequence it
recommends.

**One API change, because the old one contradicted its own docs.**
`Scheduler::new` took `ScheduleStore` **by value** while its doc said
"the store may be shared with other readers (operator dashboards,
manual fire paths)". It could not be: `ScheduleStore` has no `Clone`,
and its `list()` serves from its own cache, so a second store over the
same root answers stale. `Scheduler::new(Arc<ScheduleStore>)` makes the
documented sharing real and gives the tool and the host one view of the
schedule (the per-job file locks still cover other *processes*).
In-crate callers updated; no consumer call sites exist yet.

**Wiring.** New crate in `[workspace.members]` +
`[workspace.dependencies]`; facade feature `tool-scheduler` (opt-in,
like `mcp`: both need a host-constructed runtime object, so neither
belongs in the default set) with the module, table row, and a
`compile_fail` gate proving the module is absent without it;
`make check-mvp-deps` asserts the sixth offline tool crate pulls no
HTTP/OTel/DB dependency. Docs swept: `README.md` (member count 19 → 26,
which had gone stale, plus the plugin rows and example ladder),
`MINIMAL.md`, `SEAMS.md`, `AGENTS.md` §1/§3.6/§3.7,
`docs/examples/README.md`, `uva-tool`'s crate docs, and the facade's
`mcp` / `scheduler` / `tool` module docs.

**Verification** — `cargo test -p uva-mcp` 74/74 (15 in
`tests/control_tool.rs`, including the governance and re-sync ones),
`cargo test -p uva-tool-scheduler` 24/24 (18 integration + 5 unit +
1 doc), `cargo test -p uva-scheduler` 19/19 lib (the crate has no
doctests) after the `Arc` change and the totality work,
`cargo test -p uva` 12/12 doc tests on the default set and 20/20 on
the CI MVP subset, 19/19 `--all-features` (the new `tool-scheduler`
reachability test included), `uva-tool` 196+ and `uva-app-server`
402+ unchanged. Three defects were proven by reproduction before the
fix, each by reverting it: the host-DoS chain (one `create` with
`interval_seconds: 1e16` → `Scheduler::tick` panicked in `chrono` at
`TimeDelta::seconds out of bounds`), the busy-loop variant of the same
job (tick returned `next_deadline: Some(0ns)` with the row still
`Active`, i.e. a re-tick plus a file-lock rewrite per pass, now
`None`/`Paused`), and the failed-advance stamp (`last_fired_at` set on a
job that never fired). All three are regression tests now
(`create_refuses_an_interval_that_could_panic_the_host`,
`an_unadvanceable_job_is_parked_not_left_due`,
`a_failed_advance_does_not_stamp_last_fired`,
`out_of_range_interval_is_an_error_not_a_panic`). Both examples run
against real dependencies and print their proof line: `cargo run
--example mcp_control_tool -p uva-mcp` → `MCP-CONTROL-TOOL: OK` (a
real `sh` MCP child spawned, catalogued at run time, called, and the
agent's surface confirmed as the single `mcp` tool), `cargo run --example
schedule_jobs -p uva-tool-scheduler` → `SCHEDULE-TOOL: OK` (the job
the tool created is the job the host's tick delivers). `make ci` **9/9**,
`make check-mvp-deps` and `make check-no-runtime` green (the sixth
offline tool crate stays HTTP-free, and `uva-scheduler` stays
tokio-free — the `Tool` impl lives in its own crate, never in the
runtime-free one).

### Changed (R121 — the shell owns its execution policy; the `task` tool becomes a `uva-tool-*` crate)

Two pieces of one concern: *a tool's boundary belongs to the tool, and
every agent-facing tool is a `uva-tool-*` plugin crate*.

**The OS execution policy left the paradigm crate.** `ExecutionPolicy`,
`SandboxBackend`, `BwrapBackend`, `NoopBackend`, `ConfinedCommand`,
`SandboxError`, `BackendAvailability`, `bwrap_profile_args`,
`resolve_confined_command`, `denial_dialect`, `classify_denial`,
`denial_marker`, `policy_prompt_block` and `effective_policy` moved
from `uva-tool::sandbox` to `uva-tool-shell::sandbox` (re-owned
by `uva-tool-shell`, which already re-exports them from its root).
The defect this closes was real and silent: the policy lived on the
shared `Context` (`policy` + `sandbox` fields), and **no production
code ever set it** — the loop builds `Context::new(..)` — so
`ShellTool::with_policy(ExecutionPolicy::ReadOnly)` changed only the
description the model was shown while the call spawned unconfined. The
description and the argv now come from one value: `ShellTool` holds its
own `policy` + `sandbox` (`with_policy` / `with_sandbox`) and
`call()` resolves the wrap from them. `Context` loses both fields
(it is the context *every* tool shares, so a per-tool boundary on it
would let one tool's setting change another's behaviour).

`effective_policy` also stopped taking `&[SessionEvent]`: it folds the
`sandbox_mode` wire names (`&[&str]`), which was the sole reason
`uva-tool` depended on `uva-session`. The tool paradigm now
depends on `uva-core`, `uva-provider` and `uva-telemetry`
only, and the tool plugin crates stay free of the session crate as
`AGENTS.md` §3.7 requires. `uva-tool` also dropped its two
now-unused dependencies — `libc` (nothing in the crate mentions it;
the `SIGTERM`/`SIGKILL` escalation lives in `uva-tool-shell`'s own
manifest) and `chrono` (zero `chrono::` uses in `src/`, the old
`sandbox.rs` included) — which is why they disappear from the lock
diff.

**The delegation plugin is now a tool plugin.** `uva-delegation`
→ `uva-tool-task` (facade feature `delegation` → `tool-task`,
module `uva::tool_task`), so the crate that ships the model-facing
`task` tool follows the same convention as `-read` / `-write` /
`-shell` / `-todo` / `-web`. Modules, types and examples are unchanged
(`task`, `gate`, `worktree`, `pool`, `router`, `route_rules`,
`mention_clone`; `TaskDelegator`, `TaskSpec`, `SubagentPool`, …). It is
the one plugin that also depends on `uva-agent`, because it
installs through the `ToolInterceptor` seam — `AGENTS.md` §3.7 now says
so instead of implying every tool plugin depends on the paradigm crate
alone. `make check-mvp-deps` gained it to the offline list (five
crates, none pulling an HTTP client), and the doc/gate sweep covers
`README.md`, `AGENTS.md`, `MINIMAL.md`, `SEAMS.md`, `docs/README.md`,
`docs/examples/README.md`, `crates/uva-agent/README.md` and the
facade module docs.

**A behaviour change, not just a deletion: `write` now runs
sequentially, and the queue it replaces is gone.** `uva-tool`'s
`mutation_queue` (per-canonical-path write serialisation, ~180 lines +
3 tests) had **zero consumers** repo-wide — its own doc named
`WriteTool` as the consumer, and `WriteTool` never used it. That is the
same defect class as the sandbox: a single tool's concern parked in the
shared paradigm crate, `pub` while nothing outside it refers to it.

The race it was written for is real — reproduced before deleting it,
two concurrent `write` calls in `append` mode (`read_to_string` → build
→ `fs::write`) on one path left only the second body on disk — and the
framework already has the mechanism, which the write tool simply was
not using. `WriteTool::mode()` now returns
`ExecutionMode::Sequential`, joining `shell`, `web_fetch` and the MCP
proxy tools, and `uva-tool-write` pins that in a test.

**User-visible consequence**: a batch of `write` calls in one LLM pass
now executes one call at a time (the Parallel bucket completes first),
and the loop's Sequential bucket aborts the remaining calls of the
round after the first failure. Only the serial package is affected —
`read` and `todo` stay `Parallel` (read-only / in-memory). The finer
per-path queue can come back inside `uva-tool-write` if write
throughput ever justifies it; the paradigm crate is no longer where it
would live.

Verification (all counts are `cargo test -p <crate>` totals —
lib + integration + doc — measured at `0c542000`, where the sandbox
move, the rename and the queue deletion are all in tree):
`make ci` **9/9 OK** (including `check-mvp-deps` with the fifth
offline crate, `doc-check`, `check-claim-language`, `check-clock`,
`check-pub-surface`); clippy
`--all-targets --all-features --tests --all -- -D warnings` exit **0**;
`cargo +nightly fmt --all --check` clean; `make examples` runs every
example plus both standalone consumer proofs, all printing their proof
lines (`CONSUMER-PROOF: OK`, `MVP-OK`, `SANDBOX-POLICY: OK`).

The full workspace sweep — every member, one crate at a time per
`AGENTS.md` §3.3 — is **2746 passing / 0 failing** across all 25
crates. The ones this change touches:
`uva-tool` **226** (196 lib — the 16 moved sandbox tests
and the 3 deleted queue tests are gone, nothing else changed),
`uva-tool-shell` **39** (33 lib = the 17 pre-existing shell tests
plus the 16 that moved in from `uva-tool`, plus the 6-case
`sandbox_smoke` integration suite),
`uva-tool-task` **72** (66 lib, test-name set
byte-identical to the pre-rename `uva-delegation`), `uva-tool-write`
**15** (+1: the `Sequential` mode pin), `uva-agent` **254**
(240 lib). `uva-app-server`'s test inventory is byte-identical to
pre-R121 HEAD (its only edit is a one-line import rename), so its count
is unchanged by this change.

Test inventories were diffed **by test-function name** before/after
each move rather than by count: the 16 `sandbox` tests moved
`uva-tool` → `uva-tool-shell` and the 70 delegation tests
carried over unchanged; the only removals anywhere are the 3
`mutation_queue` tests that went with the deleted module.

### Changed (R111 — four deferred ≥120-line inline test blocks moved to sibling tests.rs)

R110c deferred four inline `#[cfg(test)] mod` blocks inside
`uva-agent` production files; this round moves them, pure
code motion in the R106/R109 pattern:

- `re_act/stream.rs` 464 → 343 (+ `stream/tests.rs` 120) —
  the `finalize_buffers` dedup-contract tests.
- `agent/handle.rs` 404 → 266 (+ `handle/tests.rs` 137) —
  `AgentHandle` / `DetachedAgent` spawn-join-buffering tests.
- `agent/group_join.rs` 502 → 330 (+ `group_join/tests.rs` 171) —
  the `GroupJoin` pass/held/partial/timeout state machine.
- `agent/best_of_n_llm_judge.rs` 332 → 172
  (+ `best_of_n_llm_judge/tests.rs` 159) — the judge-scorer
  provider interaction + score-parse tests.

Each production file now ends at `#[cfg(test)] mod tests;` and
carries only its production concern; every moved test runs
under the same name, one path segment deeper
(`agent::re_act::stream::tests::*` etc.). Smaller inline blocks
(< 100 lines, e.g. `interceptor.rs`) remain by design — the
ratchet only tracks the ≥300 band, which is unchanged at 20.

### Added (R115)
- `uva-search` crate: generic multi-domain search engine — BM25 + vector hybrid with pluggable `Tokenizer` / `Embedder` / `VectorStore` / `Filter<T>` / `Reranker<T>`, incremental add / tombstone remove / threshold-triggered compact, thread-safe `Arc<SearchEngine<T>>`, cross-`T` `Registry` keyed by `type_name`, agent-facing JSON projection (`agent_view` / `search_tool`). `HashingEmbedder` ships default; `ModelProviderEmbedder` behind the crate's opt-in `provider` feature (transport-free). Exposed on the facade via the `search` feature (default off). `check-mvp-deps` gains a dedicated uva-search stanza (default + `provider`); `check-no-runtime` covers the crate's default build.

Verification: `uva-agent` lib **240/240** (test-name set
identical modulo the path change); `uva-app-server` **403/403**;
`cargo clippy --all-targets -D warnings` 0; fmt clean; `make ci`
**8/8** OK.

### Changed (R110 — `AgentBuilder` folded into `ReActAgent`; the harness is the only builder)

`AgentBuilder` (R12) and `ReActAgent::with_*` (pre-R12) were two
parallel types with parallel setters wiring the same fields — the
builder stored them, copied them onto a `ReActAgent` in `build()`,
and `ReActAgent` then exposed most of them again as `with_*`
setters the agent crate never used. Two parallel chains, two
places to look up "how do you set the steering?". The two chains
collapse into one:

- `ReActAgent` is **the** builder. Every `AgentBuilder` setter
  has a `ReActAgent::with_*` twin with the same semantics. The
  `agent::builder` module is gone (`mod.rs` 432, `tests.rs` 378,
  `compaction.rs` 212 → deleted); its remaining concerns moved:
- `CompactionEmitters` / `context_manager_for_compaction*` /
  `resolve_context_manager` (now `resolve`) moved to a new
  top-level `uva_agent::compaction` module — they are the
  server-side run factory's helpers, not the agent's plumbing.
  Re-exported from the crate root as before.
- `with_compaction_settings(s)` + `with_typed_event_sink(sink)`
  with no external checkpoint now auto-builds a fresh
  `CompactionCheckpoint` from the sink and wires its emitters
  onto the summarising manager (the previous builder behaviour).
  `with_compaction_checkpoint(c)` installs an externally owned
  checkpoint (it carries its own sink + ledger); it takes
  precedence over the auto-built one.
- `with_output_schema(json)` auto-registers the
  `structured_output` tool AND stamps the descriptor. Same
  LIFO behaviour as before.

Every existing call site updated:

- The 5 agent-side examples (`assemble_with_builder`,
  `best_of_n_judge`, `runtime_agnostic`, `strategy_swap`,
  `hot_paths` bench) now use `ReActAgent::new(..)` +
  chained `with_*` setters — `assemble_with_builder` is
  unchanged in shape (still proves the full one-expression
  assembly), only the type name changes.
- The facade example (`uva/examples/assemble_from_zero.rs`)
  and the two external-consumer examples
  (`docs/examples/external-consumer`, `docs/examples/minimal-consumer`)
  use the prelude's `ReActAgent` directly.
- The server was already `ReActAgent::with_*` (never built
  through `AgentBuilder`); no change.
- `uva::prelude` exposes `ReActAgent` (not `AgentBuilder`).

Verification: `uva-agent` **239/239** lib tests green
(down from 247: the 8 `AgentBuilder`-specific setter/clone tests
are absorbed into `ReActAgent`'s own setter coverage, plus the
2 `compaction::tests`); `uva-app-server` **403/403** byte-identical;
`uva` facade + every example + the two external-consumer
examples print their proof line; `make ci` **8/8** OK; clippy
`--all-targets -D warnings` 0; `make check-mvp-deps` and
`make check-no-runtime` OK; `make check-test-layout` band ratchet
unchanged at 20.

### Changed (R110c — the loop's cross-cutting helpers get their own file)

`loop_/drive.rs` (603 lines) held the orchestrator, its four
named phases, the per-iteration helpers, **and** the three
cross-cutting helpers called from more than one phase:
`hooks_on_agent_start`, `hooks_on_agent_end`, and `cancelled`.
The three moved to a new sibling `loop_/drive_hooks.rs`
(103 lines), leaving `drive.rs` at 522 lines with a doc
pointer to the new file. The `ReActLoop::cancelled` method
stays in `drive.rs` (it is the ergonomic surface the
per-iteration helpers call); the free functions it wraps now
live behind `super::drive_hooks`.

`loop_/mod.rs`'s layout table gained the `drive_hooks` row.

Verification: `uva-agent` lib **240/240** OK
(byte-identical list); `uva-app-server` **403/403**;
`cargo clippy --all-targets -D warnings` 0; `make ci` **8/8**
OK; fmt clean.

### Changed (R110b — `re_act/agent.rs` split: one concern per file)

`crates/uva-agent/src/agent/re_act/agent.rs` (653 lines) held
five distinct concerns — the struct + four constructors +
`default_descriptor`, descriptor-mutation setters, runtime
setters + `resolve_compaction_now` + `runtime_for_run`, the
`Agent` + `RegistryItem` impls, and `projected_tool_definitions`.
The user's stated objective explicitly calls for the harness
itself to be "well-layered and well-laid-out". The 653-line
file split into four focused modules, all in the project's
established "every concern in its own file" pattern:

- `agent.rs` 250 — struct + `new` / `with_options` /
  `with_prompt_context` / `with_descriptor` constructors +
  `default_descriptor` helper.
- `agent_descriptor.rs` 81 — `with_name` /
  `with_instructions` / `with_model_hint` /
  `with_output_schema` / `set_prompt_context` /
  `descriptor_mut` (descriptor mutations).
- `agent_runtime.rs` 334 — `with_workspace` /
  `with_tool_registry` / `with_steering` /
  `with_context_manager` / `with_interceptor` /
  `with_max_iterations` / `with_tool_surface` /
  `with_tool_restriction` / `with_typed_event_sink` /
  `with_run_inbox` / `with_clock` / `with_spawner` /
  `with_strategy` / `with_compaction_settings` /
  `with_compaction_checkpoint` +
  `resolve_compaction_now` + `runtime_for_run` +
  `projected_tool_definitions`.
- `agent_impl.rs` 65 — `impl Agent for ReActAgent` +
  `impl RegistryItem for ReActAgent`.

`agent.rs` itself fits on a screen (250 lines). Public surface
unchanged — every existing `ReActAgent::with_*` call site,
example, server factory, and external consumer kept its name.

Verification: `uva-agent` lib **240/240** OK
(byte-identical test list vs R110a); `uva-app-server`
**403/403**; `cargo clippy --all-targets -D warnings`
0 warnings; `make ci` 8/8 OK; the `uva-agent` docs comment
in `agent.rs` now carries a per-module table pointing each
caller at the right file.


 ### Changed (R109 — the ≥400 test-block tier closed; layout ratchet added to `make ci`)

The last two ≥400-line inline test modules
(`provider/types/content.rs` 401, `provider/openai/types.rs` 400)
moved to sibling `tests.rs` files; the tier is empty workspace-wide.
The bar is now enforced, not hand-swept:

- `make check-test-layout` — hard limit 400 lines per inline
  `#[cfg(test)] mod` block + a shrink-only ratchet on the ≥300
  band (baseline 20). Wired into `make ci` (now 8 gates).
- `make test-layout-audit` — lists every ≥300-line inline block.

AGENTS.md §3.6 documents both targets.

Report: `docs/optimization-report-R109-2026-09-18.md`.

Verification: provider **690/690** byte-identical list; agent
**247/247** + tool **215/215** re-run post-R108-cleanup;
`make ci` **8/8** including the new gate; both ratchet failure
paths negative-tested.

### Changed (R108 — the tool registry split one concern per file)

`tool/registry.rs` (1 369) — catalog types, the registration
value type and internal storage row, registration/mutation,
version-cached snapshots, scoped registration, the `run_stream`
dispatcher with its panic-capturing drain, and the private
`Registry` impl — split into `registry/` (`mod` 349, `catalog`
110, `entry` 242, `scope` 130, `snapshot` 180, `dispatch` 321,
`registry_trait` 92, the last promoted from an inline `mod` with
its body byte-identical). Every `uva_tool::*` facade path is
unchanged; stale `// 1.`–`// 11.` section-number comments removed
with their sections.

Report: `docs/optimization-report-R108-2026-09-18.md`.

Verification: tool **215/215** byte-identical test list; agent
**247/247**, delegation **66/66**, server **403/403**, offline
tool plugins **14/14/17/17**; `make ci` **7/7**; clippy clean.

### Changed (R107 — the session controller split one concern per file)

`server/session/controller.rs` (2 328) — wire ops, the run-stream
factory seam, `RunDependencies`, the `SessionController` handle,
`ControllerInner` with its 533-line run task, the dispatch loop,
`RunLog`, and prompt shaping, all in one file — split into
`controller/` (`mod` 218, `ops` 70, `run_stream` 168, `deps` 322,
`inner` 304, `run_task` 557, `persist` 112, `dispatch` 535,
`run_log` 164). Every `crate::session::controller::X` path is
unchanged; cross-module seams are `pub(super)` only.

Report: `docs/optimization-report-R107-2026-09-18.md`.

Verification: server **403/403** with a byte-identical test list;
`make ci` **7/7**; workspace clippy 0 warnings.

### Changed (R106 — the 400-line inline-test tranche extracted across six crates)

Twelve production files each interleaving 400–500 lines of
tests with production code, all resolved by pure code motion to
sibling `tests.rs` files: `session/compaction_checkpoint`,
`session/events`, `session/repair`, `session/token_meter`,
`tool/surface`, `tool/sandbox`, `core/error`, `core/registry`,
`provider/retry`, `provider/streaming/anthropic/processor`,
`context/summarizing`, `workflow/plan`.

Inline test blocks in production files are now uniformly
< 400 lines workspace-wide (two remain at 400–401, 22 in the
300–400 band).

Report: `docs/optimization-report-R106-2026-09-17.md`.

Verification: core **151/151**, tool **215/215**, session
**147/147**, provider **690/690**, context **95/95**, workflow
**64/64** — all test-name lists byte-identical; agent
**247/247**, delegation **66/66**, server **403/403**;
`make ci` **7/7**; workspace clippy 0 warnings.

### Changed (R105 — the R89 test-block sweep extended workspace-wide; workflow runtime split per concern)

The audit that closed R104 inside `uva-agent` had never run
on the other crates: 66 production files still carried 200+-
line inline test blocks, and the five worst held 3.8k lines of
interleaved tests. All five are resolved:

- `workflow/runtime.rs` (2 319) split into `runtime/`
  (`mod.rs` facade 268, `executor/mod.rs` 548,
  `executor/mcts.rs` 228, `execution.rs` 165) + `tests.rs`.
- `provider/anthropic/provider/transform.rs`,
  `provider/mod.rs`, `delegation/worktree.rs`,
  `context/memory/file.rs`: test blocks moved to sibling
  `tests.rs` files; production untouched.

Exactly one ≥500-line inline test block remains workspace-wide
(`session/compaction_checkpoint.rs`, 501).

Report: `docs/optimization-report-R105-2026-09-17.md`.

Verification: workflow **64/64**, provider **690/690**,
delegation **66/66**, context **95/95** (+ sqlite **103/103**)
— all test-name sets baseline-identical; `make ci` **7/7**;
workspace clippy 0 warnings.

### Changed (R104 — the agent registry's test block moved out; the R89 sweep of uva-agent is closed)

`agent/registry.rs` (463 = 178 production + 284 inline tests)
was the last production file in `uva-agent` holding an
oversized inline test block: the `AgentRegistry` catalog
interleaved with its `StubAgent` fixture and 14 CRUD /
filter-matrix / pagination tests.

Pure code motion: `registry/mod.rs` (178) + `registry/tests.rs`.
The `agent::registry` re-export surface is untouched.

Report: `docs/optimization-report-R104-2026-09-17.md`.

Verification: uva-agent lib **247/247** (baseline-identical);
`make ci` **7/7 green**; server **403/403**; delegation **66/66**.
Final crate state after R99–R104: every production file <500
lines except R98's documented `drive.rs`; every inline test block
<200 lines; every module in the same `mod.rs` + `tests.rs` shape.

### Changed (R103 — the events module's two large inline test blocks moved to sibling tests.rs)

The R89 sweep reached `events/`: `event_enum.rs` (516 = 185
production + 331 inline tests) and `system_event.rs` (382 = 155
+ 227) each interleaved one event type with its full test
module — fixtures, wire-tag matrices, and round-trip contracts
— in the same file. `agent_meta.rs` (119 inline) and
`reasons.rs` (125 inline) are well below the threshold and stay
as they are.

Pure code motion in the established `foo/mod.rs` +
`foo/tests.rs` layout: `event_enum/mod.rs` 186 +
`event_enum/tests.rs` 333; `system_event/mod.rs` 156 +
`system_event/tests.rs` 228. The `events::` re-export surface
is untouched.

Report: `docs/optimization-report-R103-2026-09-17.md`.

Verification: uva-agent lib **247/247** (baseline-identical;
65/65 in the events module, same as pre-split); `make ci`
**7/7 green**; server **403/403**; delegation **66/66**;
`assemble_from_zero` proof line prints; fmt clean. Every
production file in `uva-agent/src/` is now under 500 lines
except R98's documented `drive.rs` orchestrator.

### Changed (R102 — the two strategy files' inline test blocks moved to sibling tests.rs)

The R89 "test blocks out of large production files" pattern had
not yet reached `agent/best_of_n.rs` (676 = 346 strategy + 330
tests) and `agent/cot.rs` (596 = 283 + 313). Both held one
single-concern strategy plus 300+ lines of test scaffolding in
the same file.

Pure code motion in the established `foo.rs` + `foo/tests.rs`
layout: `best_of_n.rs` 348 + `best_of_n/tests.rs` 326;
`cot.rs` 285 + `cot/tests.rs` 302. No public surface touched.

Report: `docs/optimization-report-R102-2026-09-17.md`.

Verification: uva-agent lib **247/247** (baseline-identical);
`make ci` **7/7 green**; `strategy_swap` proof line prints; fmt +
clippy clean. Every production file in `uva-agent/src/` is
now single-concern and below 500 lines except R98's documented
`drive.rs` orchestrator.

### Changed (R101 — team.rs split: each composition gets its own module)

`agent/team.rs` (900 lines) held three *independent* compositions
sharing one file: the `BoundAgent`/`FnAgent` boundary, the
`VerificationChain` generate-verify-retry loop, and the
`RoundRobinGroupChat` transcript engine, plus 290 lines of tests.
Unlike the loop splits these are not phases of one orchestrator —
the treatment is per-type modules.

Pure code motion, public paths unchanged. `team.rs` is now a
`team/` directory: `bound.rs` (91), `verification.rs` (159),
`group_chat.rs` (324), `tests.rs` (292), and a 90-line `mod.rs`
that keeps the module docs and re-exports every public name, so
the `agent/mod.rs` re-export block and every consumer
(`agent_teams` example, `cot.rs` doc link, `uva-delegation`)
resolves unchanged.

Report: `docs/optimization-report-R101-2026-09-17.md`.

Verification: `make ci` **7/7 green**; uva-agent lib
**247/247** (baseline-identical); server **403/403**; delegation
**66/66**; `agent_teams` prints its proof line; fmt clean.

### Changed (R100 — builder.rs split: the factory, the compaction wiring, and the tests each get a file)

R99 closed the strategy seam; the audit moved to the largest
remaining multi-concern file, `agent/builder.rs` (991 lines): the
`AgentBuilder` factory, the compaction wiring
(`CompactionEmitters` + `context_manager_for_compaction*` +
`resolve_context_manager`), and 385 lines of tests shared one
file. The compaction half is not builder-private — the server's
run factory calls it directly when hand-assembling agents — so
two different consumers were interleaved in one file.

Pure code motion, public paths unchanged. `builder.rs` is now a
`builder/` directory: `mod.rs` (432 — the struct + setters +
`build()`), `compaction.rs` (212 — emitters, view mirrors, the
public entries, the `pub(super)` decision core), `tests.rs` (378).
The crate-root re-exports (`uva_agent::CompactionEmitters`,
`context_manager_for_compaction_with_emitters`, …) resolve
unchanged; `resolve_context_manager` widened file-private →
`pub(super)` so the sibling tests keep driving its decision table.

Report: `docs/optimization-report-R100-2026-09-17.md`.

Verification: `make ci` **7/7 green**; uva-agent lib
**247/247** (baseline-identical); uva-app-server **403/403** (the
moved API's direct consumer); delegation **66/66**;
`assemble_from_zero` + `runtime_agnostic` proof lines print;
fmt clean.

### Changed (R99 — the strategy seam is now one concern per file, like the loop below it)

R97/R98 gave the harness's dispatch seam and main-loop body the
"every concern in its own file" property. The follow-up audit
found `agent/strategy.rs` (556 lines) still holding three
concerns side by side: the 22-field `AgentRuntime` a strategy
receives, the `EventSink` it publishes through, and the
`ReasoningStrategy` trait + `from_name` resolver a deployment
configures. A reader with one of those questions had to skip past
the other two's code.

Pure code motion, public paths unchanged. `strategy.rs` is now a
`strategy/` directory:

| File | Concern |
|---|---|
| `mod.rs` (157) | seam docs + `ReasoningStrategy` trait + `KNOWN_STRATEGY_NAMES` + `from_name` |
| `runtime.rs` (172) | `AgentRuntime` + `model_config`/`system_prompt` + `Debug` + `default_for_test` |
| `sink.rs` (92) | `EventSink` + the publish primitives + `Debug` |
| `tests.rs` (205) | seam-level tests |

Every existing path still resolves —
`agent::strategy::{AgentRuntime, EventSink, ReasoningStrategy,
KNOWN_STRATEGY_NAMES, from_name}` and the crate-root re-exports
were never touched; `default_for_test` stays `pub(crate)` +
`#[cfg(test)]` so `cot.rs` / `best_of_n.rs` compile unchanged.

Report: `docs/optimization-report-R99-2026-09-17.md`.

Verification: `make ci` **7/7 green**; uva-agent lib tests
**247/247** (baseline-identical, verified via `git stash`
round-trip); uva-delegation **66/66**; `strategy_swap` and
`runtime_agnostic` examples both print their proof lines;
complexity scan zero violations; `cargo +nightly fmt --all`
clean.

### Changed (R98 — the harness's MVP main loop body is now 26 lines, not 215)

R97 closed the dispatch seam into four focused sibling
modules. The follow-up audit turned to the main loop
body itself: `ReActLoop::drive()` in
`uva-agent/agent/re_act/loop_/mod.rs` was 215 lines
of inline state machine — the "MVP harness, simple明了,
harness自身也要分好层次和布局" property that R88's plugin
split established at the *crate* level had not yet been
applied at the *function* level. The method held six
concerns in one body (session-start / OnAgentStart fan-out
/ setup / request-header stamp / the 5-step iteration /
post-loop finalize). R98's rule: every named phase lives
in its own helper; the orchestrator reads as the step
list its doc promises.

`loop_/mod.rs`: **559 → 313 lines**. Now holds only:

- The `ReActLoop` struct + `from_runtime` constructor.
- The `SampleOutcome` struct + `has_tool_calls` impl
  (moved here from `steps` because both `drive` and
  `steps` produce/consume it; keeping the type in
  `mod.rs` avoids forcing one to import from the other).
- The `ToolProjectionMemo` struct.
- A one-line `drive()` façade that delegates to
  `drive::drive(self, input).await`.
- The delegation surface — 15 one-line methods, each
  routing to the focused sibling that owns the concern.
  No control flow, no state.

New `loop_/drive.rs` (512 lines). The `drive()`
orchestrator + every named phase in its own helper:

| Phase helper | What it does |
|---|---|
| `drive_setup` | `SessionStarted` event + `hooks_on_agent_start` fan-out + prepare messages + apply context |
| `stamp_request_header` | R6-A typed event (model id + tool-list hash) for replay determinism |
| `drive_one_iteration` | One pass of the 5-step loop; returns `IterationOutcome` |
| `drive_finalize` | Post-loop max-iter warning + `finalize` + `hooks_on_agent_end` fan-out |
| `pre_sample_seams` | Drain steering, snapshot runtime context, inject hints, emit progress + step-start |
| `handle_final_answer` | Follow-ups poll: empty → `Completed`; non-empty → inject + apply_context + `Continue` |
| `handle_tool_calls` | Length-stop fail-batch or cancel-check or `execute_tools` |
| `hooks_on_agent_start` / `hooks_on_agent_end` | Fan out lifecycle hooks; failures are warning events |
| `cancelled` | Cancel-aware warning emission |

The orchestrator body is now 26 lines and reads as the
four-step MVP harness list the architecture document
advertises (setup → header → loop → finalize).

Two small types encode the split's control flow in
the type system:

- **`IterationOutcome`** — `Continue` / `Completed` /
  `Failed(SessionEndReason)` / `EarlyReturn(AgentOutput)`.
  The four cases the orchestrator's match dispatches on.
  `EarlyReturn` is distinct from `Failed` because the
  cancelled-before-tool branch already runs `finalize`
  + the agent-end hooks and the orchestrator must skip
  the post-loop finalize step entirely.
- **`IterationDispatch`** — per-iteration bookkeeping:
  `outcome` (the orchestrator's signal), `truncated`
  (close-phase typed-event label), `context_pruned`
  (whether the dispatch handler already called
  `apply_context` inline — the follow-up branch does;
  the close phase skips its own prune when this is
  true, preserving the test's "re-budget before the
  next sample" invariant).

Behaviour byte-identical. The previously-passing
`follow_up_revival_rebudgets_before_the_next_sample`
test continues to pass (256/256 uva-agent lib
tests); it explicitly checks the two-`apply_context`
behaviour the split preserves via
`!dispatch.context_pruned && !Completed`.

Post-landing complexity audit found two regressions
(`drive_one_iteration` at 28/20, `handle_tool_calls` at
25/20, both above AGENTS.md §3.5's 20 threshold). Two
further extractions in the same named-phase shape
brought both functions under threshold: `drive_one_iteration`
split into [`iteration_start_setup`] / [`sample_or_fail`]
/ [`commit_assistant_and_step_end`] / [`close_iteration`]
(orchestrator is now a flat 5-line pipeline); `handle_tool_calls`
split into [`dispatch_truncated_batch`] /
[`dispatch_cancelled_before_tool`] /
[`dispatch_execute_tools`] (orchestrator is now a
flat 3-arm dispatch). Final clippy complexity scan
reports zero violations across the workspace.
Report: `docs/optimization-report-R98-2026-09-17.md`.

Verification: `make ci` **7/7 green**; `make test-unit`
**2453/2453** (no count change — pure code motion);
`cargo clippy --workspace --all-targets --all-features
--tests --all -- -W clippy::cognitive_complexity` reports
**zero violations** (R96 baseline holds); 256/256
uva-agent lib tests; 66/66 uva-delegation lib
tests; `cargo +nightly fmt --all` clean.


### Changed (R97 — the harness dispatch seam is now a façade, not a 900-line file)

R92 closed the harness's per-iteration loop helpers. The
follow-up audit turned to the **layout** of `uva-agent`
where R88's plugin split left a single 900-line module
owning four distinct concerns. The split had been *named*
(each helper carried a phase comment) but the four
orchestrators + seam controls + bucket implementation +
commit helpers all shared one file. R97's rule: "every
concern in its own file, every orchestrator reads as the
step list its doc promises" now holds inside the file
boundary too.

`uva-agent/agent/re_act/loop_/dispatch.rs`: **900 → 167
lines**. Now holds only the three pieces the dispatch seam
as a whole *shares*: `WireToolResult` (the wire payload
produced by `bucket` and `steps`, consumed by `commit`),
`lookup_execution_mode` + `tool_definitions` (the read-side
memoised projection), and `append_tool_result_hints` +
`notify_error_hooks` (the steering seams that span the
dispatch step but don't belong to any one bucket).

New single-concern sibling modules under
`uva-agent/agent/re_act/loop_/`:

- **`bucket.rs` (339 lines)** — the `execute_tools`
  orchestrator + Parallel / Sequential bucketing, semaphore
  concurrency, the sequential-abort `SequentialStep` state
  machine, and the four-phase `commit_all_results`. The
  orchestrator body reads as the 5-step list its doc
  promises.
- **`seams.rs` (295 lines)** — the `execute_tool_inner`
  orchestrator + the four steering seams (restriction,
  hook veto, guard pipeline, observation) + the `Verdict<T>`
  enum shared with `route`. The orchestrator body reads
  as the 4-seam list.
- **`route.rs` (185 lines)** — the `dispatch_tool_call`
  orchestrator + the three named routing steps
  (`try_interceptor` / `lookup_tool_entry` /
  `drain_tool_stream`). The orchestrator body reads as the
  3-step list.
- **`commit.rs` (54 lines)** — `commit_tool_result` +
  `last_assistant_text`. The history-commit on the
  tool-result side and the final-message projection on the
  finalize side, kept together as "the live-history
  writeback layer".

The `ReActLoop` struct keeps the same delegation surface,
but every delegation now goes to the focused sibling
module that owns the concern:
`bucket::execute_tools` / `route::dispatch_tool_call` /
`commit::commit_tool_result` / `seams::execute_tool_inner`
(the last is called directly from `bucket.rs`; the old
`ReActLoop::execute_tool_inner` indirection that used to
forward `dispatch::execute_tool_inner` was dead and is
deleted).

`Verdict<T>` visibility widened from `pub(super)` to
`pub(in crate::agent::re_act)` — the type is a private
protocol of the dispatch seam (consumed by `route`'s
`lookup_tool_entry` and `seams`'s `apply_guard_pipeline`),
never reaching outside the `re_act` sub-tree.

Report: `docs/optimization-report-R97-2026-09-17.md`.

Verification: `make ci` **7/7 green**; `make test-unit`
**2453/2453** (no count change — pure code motion);
`cargo clippy --workspace --all-targets --all-features
--tests --all -- -W clippy::cognitive_complexity` reports
**zero violations** (R96 baseline holds); 256/256
uva-agent lib tests; 66/66 uva-delegation lib
tests (the downstream that exercises the dispatch path
through real tool calls); `cargo +nightly fmt --all`
clean.

# Changelog

All notable changes to this project will be documented in this file.


### Changed (R96 — every test-code complexity violation cleared)

R95 closed the lib side. Four `#[cfg(test)]` functions still
over the 20 threshold:

- `uva-core::error::tests::helpers_produce_distinct_variants`
  (was 73/20) — 35 `assert!(matches!(...))` lines, one per Error
  helper. Replaced with an in-module `assert_variant!` macro and
  8 per-domain tests (lookup_and_validation, provider_and_protocol,
  stream_variants, auth_and_config, lifecycle_and_memory,
  model_and_rate_limit, edit_and_retry, from_std_io). 144
  uva-core tests pass.
- `uva-app-server::config::tests::test_server_config_full_round_trip`
  (was 27/20) — 110-line build-and-assert body. Split into a
  11-line orchestrator + `build_sample_server_config()` + 7
  per-domain assert helpers (`assert_top_level_round_trips`,
  `_mcp_round_trips` [R21], `_retrieval_round_trips` [R22],
  `_tools_surface_round_trips` [R34], `_auth_cors_round_trips`,
  `_providers_agents_round_trips`, `_operations_round_trips` [R29]).
  All 30 field-level assertions preserved byte-identical.
- `uva-provider::streaming::anthropic::tests::two_parallel_tool_use_blocks_emit_independent_streams`
  (was 27/20) and
  `tool_use_emits_start_delta_end_with_is_done` (was 21/20) —
  extracted `build_parallel_tool_use_events` +
  `build_single_tool_use_events` + `process_each_event` + 8
  per-shape assertion helpers. 46 anthropic tests pass.
- `uva-session::log_surface::compaction_op` (was 23/20;
  splitLogSurfaceTest went one layer deeper than the audit
  named, since the post-R95 lib function still hit the 20
  threshold) — split into `compaction_op_value` +
  `parse_replace_op`. 147 uva-session tests pass.

Verification: `make ci` 7/7 green; `make test-unit`
**2453/2453** (+7 from the per-domain tests added in
`uva-core/error.rs`); `cargo clippy --workspace
--all-targets --all-features --tests --all -- -W
clippy::cognitive_complexity` reports **zero violations,

### Changed (R95 — every lib complexity violation cleared across the workspace)

R94 closed the session-controller trio. The fresh workspace
scan surfaced nine lib functions still over the 20 threshold;
every one of them now has its own set of single-concern
helpers, and the workspace has **zero `cognitive_complexity`
warnings outside `#[cfg(test)]` modules** (the test violations
in `provider/anthropic/tests.rs` and `session/log_surface.rs`
are inside the clippy test allow-list and stay as-is).

- `provider::streaming::anthropic::StreamProcessor::process_event`
  (was 39/20) — one private fn per SSE event category
  (`handle_content_block_start` / `_delta` / `_stop`,
  `handle_message_delta`, `handle_message_stop`); the heaviest
  arm (`message_stop`) further splits into `drain_orphan_buffers`
  + `flush_think_extractor` + `build_sampling_result`. The
  `drained.clear()` quirk (drained ThinkExtractor chunks are
  folded into `self.text` / `self.reasoning` but never pushed
  onto the outgoing `chunks` vector) is preserved verbatim.
  46 streaming tests pass.
- `server::state::boot::build_configured_retriever` (was 26/20)
  — the per-file read+chunk loop folds into `index_path`
  (read-or-warn-and-skip). 403 uva-app-server tests pass.
- `server::state::boot::apply_tools_config` (was 25/20) — the
  active-group activation step folds into `activate_groups`
  (mutates `active_groups` + `skipped` + logs). The existing
  `resolve_group_declarations` and `apply_surface_list`
  helpers stay.
- `server::state::boot::register_configured_mcp_servers`
  (was 21/20) — the per-health log pass folds into
  `log_tick_health`.
- `server::routes::chat::build_parts` (was 24/20) — six private
  per-kind helpers (`text_part`, `image_part`,
  `attachment_ref_part`, `audio_part`, `file_part`, `url_part`)
  replace the per-attachment match body.
- `session::log_surface::classify_compaction` (was 25/20) —
  `compaction_op` (validates `surface_op` + Replace guard) and
  `compaction_payload` (the data-shape match) carry the two
  phases. 147 uva-session tests pass.
- `provider::streaming::idle_watchdog::pump_sse` (was 23/20) —
  four helpers (`cancelled`, `append_chunk`, `emit_tail`,
  `abort_after_grace`) replace the inline cancel check, the
  select-arm body, and the trailing-buffer emission. (The
  first subagent pass extracted `emit_complete_lines` and
  `drain_within_grace` but left the orchestrator at 21/20;
  the second pass extracted the four above to drop it below.)
  9 idle_watchdog tests pass; 690 provider lib tests pass.
- `skill::discovery::discover_skills` (was 23/20) —
  `discover_project_skills` + `discover_user_skills` +
  `collect_skills` (parse + intra-batch dedup + warn) +
  `merge_project_then_user` (project-wins-on-collision via
  `HashSet<String>` of project names). 56 uva-skill
  tests pass.
- `steering::hook::run_hook_gated` (was 22/20) — `hook_span`
  + `run_hook_inner` (panic-isolated execution) + `gate_rejection`
  (gated branch builder) carry the three phases. 72
  uva-steering lib tests pass.

Verification: `make ci` 7/7 green; `make test-unit`
**2446/2446**; `cargo clippy --workspace --all-targets
--all-features --tests --all -- -W clippy::cognitive_complexity`
reports zero non-test violations.

### Changed (R94 — `ControllerInner` triple split; the session controller is fully under the complexity threshold)

R93 closed `run_controller_loop` (144→under) and named
the `ControllerInner` trio as the next target: `finalise_shutdown`
(28/20), `maybe_start_run` (29/20), `persist_and_broadcast`
(33/20). All three split into named single-concern helpers;
**no function in `uva-app-server/session/controller.rs`
exceeds the 20 threshold anymore**.

- **`finalise_shutdown` (was 28/20)** — the lifecycle event
  build + match-serde + sink-append + sink-close sequence
  folds into `append_lifecycle_event` + `close_sink` (two
  helpers, the latter just does the close). The
  `shutdown_finalised` AtomicBool swap stays at the top so a
  shutdown-then-idle race cannot double-append.
- **`maybe_start_run` (was 29/20)** — the state-gate +
  pending-input check stays inline; the Running transition,
  pending-count log, R11 snapshot publish, and cancel-token
  install fold into `mark_running(multimodal_active)`.
  `is_startable()` (the Idle/Cancelled state check) is
  pulled into its own private fn so the function signature
  reads as a flat pipeline. (The initial refactor that
  left a duplicate `R11` publish + duplicate cancel-token
  install in `maybe_start_run` after the extraction was
  caught by `snapshot_bus_publishes_running_then_completing`:
  rx would receive Running twice, never Completing — a
  second-pass cleanup removed the duplicates; tests are
  byte-identical to before.)
- **`persist_and_broadcast` (was 33/20)** — the
  serialize + decode + durable-append + broadcast block
  folds into `encode_event` (single serialize pass,
  one decode), `classify_system_kind` (the system-event
  match), `append_if_durable` (the if-event.is_durable()
  guard), and `broadcast_event` (the SSE/WebSocket fan-out
  + trailing log). The orchestrator body is now the
  one-sentence pipeline the doc comment promised.

Result: `clippy --workspace -W cognitive_complexity`
reports **no violations in `uva-app-server/session/
controller.rs`**. Remaining workspace hits: `provider`
streaming processor (39/20), `session/log_surface.rs`
(25/20), `uva-skill/discovery.rs` (23/20),
`uva-steering/hook.rs` (22/20), scattered 21-26s in
`chat.rs`, `state/boot.rs`, `idle_watchdog.rs` — and
`run_controller_loop` is now also at risk of re-entering
the list if the surrounding modules grow.

Verification: `make ci` 7/7 green; `make test-unit`
**2446/2446** (`uva-app-server` 403, including the
snapshot-bus test that surfaced the duplicate-publish bug

### Changed (R93 — `run_controller_loop` split; the workspace's 144/20 monster eliminated)

R92's report named `run_controller_loop`
(`uva-app-server/session/controller.rs`) the next target:
at **144/20** it was 6× over the threshold and the single
largest function left in the workspace. The 373-line body
was a `tokio::select!` with four arms, the op arm holding a
seven-variant match with inline bodies. Now:

- **Op dispatch** — `handle_received_op` owns the
  seven-variant match; each variant's body is its own named
  handler (`handle_prompt_op`, `handle_prompt_multi_op`,
  `handle_rerun_op`, `handle_feedback_op`,
  `handle_steer_op`, `handle_cancel_op`,
  `handle_shutdown_op`). `Prompt` and `Steer` share
  `push_text_input`; `Rerun` and `Cancel` share
  `cancel_inflight_run`; the four run-starting ops share
  `start_run_if_idle`.
- **Select arm bodies** — `on_run_completed` (panic surfacing
  + the restart-with-parked-multimodal gate) and
  `handle_idle_timeout` + `join_run` (the exit paths).
- **Guard futures** — `wait_for_run` / `wait_idle_timeout`
  hoisted out of inline `async` blocks (also converting the
  `Some(ref mut h)` pattern to match ergonomics:
  `match &mut run_handle { Some(h) => … }`).
- **Cancel snapshot** — `publish_cancelled_snapshot` +
  `drain_pending_inputs` extracted from `handle_cancel_op`
  (which itself dropped 26 → under threshold).

Result: `run_controller_loop` reads as the loop skeleton —
select, three one-line arm bodies, exit bookkeeping — and
neither it nor any new helper exceeds the threshold. The
three remaining controller hits (812 / 858 / 1529, all
pre-existing `ControllerInner` methods untouched this
round) are the R94 candidate.

Verification: `make ci` 7/7 green; `make test-unit`
**2446/2446** (`uva-app-server` 403, unchanged — the
lifecycle / cancel / rerun / prompt_multi test suites pin
the loop's behaviour byte-for-byte); `cargo +nightly fmt
--all` clean.

### Changed (R92 — the harness loop's dispatch split; every uva-agent function under the complexity threshold)

The R91 report pointed at the harness core itself: the
`re_act/loop_/` files held the workspace's worst production
cognitive-complexity scores (`dispatch.rs` 62/20, 42/20,
38/20; `steps.rs` 33/20, 24/20). All five split into named
single-concern helpers; **no function in `uva-agent`
exceeds the 20 threshold anymore**.

- **`execute_tools` (was 62/20)** — the four numbered
  phases the doc comments already promised are now four
  helpers: `bucket_by_execution_mode` (+ concurrency clamp
  + the bucketed `info!`), `run_parallel_bucket`
  (semaphore + `join_all`), `run_sequential_bucket`
  (loop control via a `SequentialStep` enum from
  `run_one_sequential`: Continue / Cancelled / Aborted),
  and `commit_all_results` (synthetic-error fill, steering
  output-transform, hint append, wire commit).
- **`execute_tool_inner` (was 42/20)** — the steering
  seams are now named functions: `pre_execution_veto`
  (R58 restriction + `BeforeToolExecute` votes via
  `hook_before_verdict`), `apply_guard_pipeline` (seam 2,
  returning a private `Verdict<T>` enum — `Allowed` /
  `Denied` — rather than `Result<_, ToolOutput>`, which
  trips `result_large_err`), and `notify_after_hooks`
  (seam 3).
- **`dispatch_tool_call` (was 38/20)** — three steps:
  `try_interceptor` (claimed synthetic tools),
  `lookup_tool_entry` (touched-file note + registry
  resolution, also `Verdict`), `drain_tool_stream`
  (generic over the stream, forwarding Progress chunks as
  `ToolProgress` events).
- **`sample_once` (was 33/20)** — `notify_provider_start_hooks`,
  `stream_completion` (cancel-aware wiring; returns
  `(resp, outcome, elapsed)` — it owns the chunk state's
  lifetime), and `observe_provider_success` (usage fold +
  typed event + tracker + `OnProviderEnd`).
- **`prepare` (was 24/20)** — the manifest-resolution
  match + assembly + its debug log folded into
  `assemble_system_prompt`.

`notify_error_hooks` moved from a dispatch-local closure to
a `loop_`-shared helper (`pub(in …loop_)`) — the provider
error path in `steps.rs` reuses the exact fan-out the hook
veto path uses.

Result: `clippy --workspace -W cognitive_complexity` reports
zero violations in `uva-agent`. Remaining workspace hits
are server-app code (`run_controller_loop` 144/20 — R93
candidate), `uva-provider` streaming (39/20), and test
functions.

Verification: `make ci` 7/7 green; `make test-unit`
**2446/2446** (`uva-agent` 256, unchanged — behaviour
byte-identical, only shape moved); `cargo +nightly fmt
--all` clean.

### Changed (R91 — `with_server_config` split into named phases; cognitive complexity 26→under threshold)

The R90 report named `AppState::with_server_config`'s
26/20 cognitive complexity as the next target. The
287-line boot orchestrator interleaved eight
near-identical `cfg?` chains with a 50-line default-agent
composition and a 3-arm YAML-bridge match; reading it
meant holding all three shapes at once. Three named
extractions flatten it:

- **`load_per_section_config`** — the eight per-section
  reads (`auth`, `cors`, `operations`, `default_agent`,
  `max_steps`, `compaction`, `allowed/denied_tools`,
  `strategy`) now live in one private fn returning a small
  `PerSectionConfig` struct. Each read follows the same
  `cfg? → agents.get(name)? → field → fallback` shape;
  grouping them means the reader scans one list and one
  shape, not eight interleaved chains.
- **`build_default_agent`** — the ~50-line block threading
  provider / tool registry / surface policy / strategy /
  restriction through the `ReActAgent::with_*` builder
  chain, plus the `try_read()` registry snapshot and four
  `if let Some` installers. In its own private fn the
  orchestrator reads "wire → assemble".
- **`bridged_workspace_config` + `legacy_workspace_config`**
  — `load_workspace_config`'s 3-arm match (Ok / Ok(None) /
  Err) split into a one-attempt loader that carries its
  failure reason as a `String`, plus the legacy
  `<workspace>/.agents/config.toml` fallback as its own
  named fn. The orchestrator is now `match { Ok, Err(reason)
  → log + fallback }`.

Result: `clippy -W cognitive_complexity` no longer flags
**any** function in `app_state.rs` (was 26/20
`with_server_config` + 23/20 `load_workspace_config`).

Verification: `make ci` 7/7 green; `make test-unit`
2446/2446 passed (`uva-app-server` 403, unchanged);
`make examples` exit 0 (CONSUMER-PROOF: OK, MVP-OK).
`cargo build -p uva-app-server --all-targets
--all-features --tests` zero warnings.

### Changed (R90 — state/ split: `app_state.rs` test block + boot helpers)

Two surgical cleanups in the server state module, both
motivated by the user's "harness 自身也要分好层次和布局"
directive: the production file had grown to 2639 lines,
1186 of which were tests and 553 of which were boot
helpers that the `AppState` impl only calls.

- **Test block split** (`crates/uva-app-server/src/state/tests/`):
  the 1186-line `#[cfg(test)] mod tests { ... }` block in
  `app_state.rs` now lives in nine focused submodules,
  each named after the surface it pins:

  | Submodule              | Tests |
  |---|---|
  | `resolve_agent_name`   | 5 — the three-tier fallback ladder every dispatch flows through |
  | `build_prompt_context` | 7 — the `<workspace>/.agents/skills/` walker + agent-registry snapshotter |
  | `for_test`             | 1 — `AppState::for_test`'s tool registry wiring |
  | `unapplied_keys`       | 2 — the R55 audit classifying inert `agents.<name>` keys |
  | `resolve_server`       | 2 — R53 `--config <path>` named-file reader |
  | `load_helpers`         | 3 — `load_default_compaction` (R16) + `load_default_tool_restriction` (R58) |
  | `load_strategy`        | 2 — `load_default_strategy` (R50) |
  | `boot_surface`         | 4 — MCP server boot (R21/R30) + retrieval index (R22) |
  | `apply_tools`          | 3 — `[tools]` boot application (R34) |

  Shared fixtures (`StubAgent`, `ScopedHome`, the
  `$HOME`-mutex swap, the `write_skill_md` helper, the
  canonical empty descriptor) live in `tests/support.rs`
  using `std::sync::LazyLock` (R90 also adopts the stdlib
  `LazyLock` per AGENTS.md's std-only convention; no
  `OnceLock` accessor functions remain).

  Behaviour and assertion text byte-identical to the old
  monolithic block; the production file is now
  `app_state.rs` (954 lines, only the `AppState` struct +
  impls + `UsageMetrics` + `AppliedToolSurface`) and the
  test surface is 1186 lines spread across 9 focused
  files plus a shared `support.rs`.

- **Boot helpers extracted** (`crates/uva-app-server/src/state/boot.rs`):
  the 553-line tail of `app_state.rs` (every free function
  the boot calls: `load_default_max_iterations`,
  `load_default_compaction`, `load_default_tool_restriction`,
  `load_default_strategy`, `unapplied_agent_keys`,
  `warn_unapplied_agent_keys`, `build_configured_retriever`,
  `register_configured_mcp_servers`, `apply_tools_config`,
  `build_prompt_context`, plus the two private helpers
  `resolve_group_declarations` / `apply_surface_list`)
  moved to `boot.rs`. They are `pub(crate)` — internal
  steps of the boot the public surface never names — and
  re-exported through `state/mod.rs` only for the
  `tests/` submodules to import via `crate::state::xxx`.

  `AppliedToolSurface` (the public record of what
  `[tools]` did) stays in `app_state.rs` because the
  `AppState` struct holds one — moving it would split a
  single type across files. `plugin_tool_registry()`
  (the deployment's default brick-by-brick tool
  composition) also stays in `app_state.rs` because it
  is consumed by `AppState::with_server_config` and is
  what `state/mod.rs` exports.

  The new file layout (three modules under `state/`):

  | Module     | Lines | Responsibility |
  |---|---|---|
  | `app_state` |  954 | `AppState` struct + impls, `UsageMetrics`, `AppliedToolSurface`, `plugin_tool_registry` |
  | `boot`      |  541 | one free function per boot step |
  | `tests`     | 1191 | 9 focused submodules + a `support.rs` |

Verification: `make ci` (fmt + clippy + doc + dep
invariants + clock + claim) **7/7 green**; `make test-unit`
**2446 passed, 0 failed** across the workspace
(uva-app-server 403 — same total as before the split);
`make lint-rust` and `cargo build -p uva-app-server
--all-targets --all-features --tests` zero warnings.

Four large production files used to bundle a giant
`#[cfg(test)] mod tests { ... }` block at the bottom. The
test bodies were pushing each file past the threshold where
the public API was still scannable at a glance. Each
test block is now a focused `tests/` directory mirroring
the structure already used elsewhere in the project, so the
production file stays small and one concern lives in one
file:

| File (before) | Production lines (before) | Test lines (before) | New layout |
|---|---|---|---|
| `crates/uva-agent/src/prompt/mod.rs`        | 378 | 543 | `prompt/tests/{assemble,identity,skills_agents,runtime_context,support}.rs` |
| `crates/uva-agent/src/agent/re_act/tests.rs` | n/a (single 4511-line test file) | 4511 | `re_act/tests/{agent_smoke,prompt_integration,end_to_end_runtime,loop_lifecycle,multi_agent_panel,parallel_dispatch,tool_projection_memo,tool_restriction,steering_wiring,run_inbox,support}.rs` |
| `crates/uva-tool/src/registry.rs`           | 1367 | 1929 | `registry/tests/{exposure,session_scope,registration,snapshot,dispatch,mutations,argument_validation,support}.rs` |
| `crates/uva-app-server/src/session/controller.rs` | 2157 | 2339 | `controller/tests/{lifecycle,cancel,event_emission,history,run_log,prompt_multi,rerun,run_config,context_manager,truncate,support}.rs` |

Behaviour, test names, and assertion text are byte-identical
to the previous monolithic blocks. The only changes are
structural: each `tests/` directory declares its submodules
in a small `mod.rs`, a `support.rs` carries the fixtures
shared across more than one submodule, and each
submodule's first lines import the fixtures it needs.

Verification: `make ci` (fmt + clippy + dep invariants +
clock + claim) passes, `make test-unit` reports
**2446 passed, 0 failed** across the workspace (uva-agent
256, uva-tool 215, uva-app-server 403, plus the other
20 crates — same totals as before the split).

### Changed (R89 — harness-construction dedup, typed-event seam extracted, dead tool API removed)

Three small, surgical cleanups that keep the
paradigm-only / plugin-only line drawn in R88 sharp and
trim a few stray pub-alls the previous rounds left in
place. No behaviour change, no public-API removal that any
documented caller exercises.

- **`ReActAgent` constructor dedup**:
  `re_act/agent.rs` used to inline the full
  `AgentDescriptor { name, display_name, description, kind,
  version, instructions, capabilities, tools, model_hint,
  handoffs, handoff_hint, output_schema, owner, domain,
  persona, max_iterations }` literal once in `with_options`
  and copy every other "off" default (12 fields) into
  `with_descriptor` by hand. The literal now lives in a
  single `default_descriptor(system_prompt)` free
  function; `with_options` and `with_prompt_context` both
  call it and forward to `with_descriptor`, which is the
  one place that owns the default state vector. Net
  effect: the four constructors each fit in a screen and
  there is one source of truth for "the default Uva
  descriptor". Behaviour, descriptor bytes, and
  round-tripped `RuntimeContext` snapshots are
  byte-identical.
- **`loop_/events.rs` extracted from `loop_/mod.rs`**:
  the four typed-event wrappers (`emit_iteration_start` /
  `emit_iteration_end` / `emit_step_start` /
  `emit_step_end`), the underlying `emit_typed` helper,
  and the `StepAction` enum the step-boundary events
  carry now live in a small sibling module. `loop_/mod.rs`
  shrinks to the state machine itself (`ReActLoop`
  struct, `SampleOutcome`, `ToolProjectionMemo`, the
  `drive` method, the per-iteration helpers + the
  `from_runtime` constructor) and the layout table at
  the top of the file lists `events` as another
  submodule. This is the same "one concern per file"
  split R89 already applied to the test blocks.
- **Dead tool API removed**:
  - `uva-tool-read::ReadTool::mark_read` and the
    `read_history: Mutex<Vec<String>>` field it pushed
    into had no reader anywhere in the workspace and
    existed only "for future edit-after-read
    enforcement" — speculative API surface that AGENTS.md
    §3.2 explicitly bans. Both removed; `ReadTool` is
    now a unit struct, the `parking_lot` dependency is
    no longer needed, and `Cargo.toml` drops it.
  - `uva-tool-web::WebFetchTool::truncate_response_body`
    was `pub` but only used by `Tool::execute` inside
    the same crate. Re-classified `pub(crate)` so the
    external surface only carries the things external
    callers actually need (`new`, `with_allowed_hosts`,
    `is_url_allowed` for introspection).

Verification: `make lint-rust` and `cargo test -p
uva-agent -p uva-tool-read -p uva-tool-write
-p uva-tool-shell -p uva-tool-todo -p
uva-tool-web --lib` all pass; the per-crate test
totals (256 / 14 / 10 / 17 / 18 / 14) are unchanged.



### Changed (plugin split — the harness is the MVP; every tool and the task tool are plugins)

The framework's core is now exactly the harness. Everything that
was welded into `uva-tool` or `uva-agent` but is not the
loop itself became a plugin crate the consumer composes:

- **`uva-tool` is the paradigm**: `Tool`, `ToolEntry`,
  `ToolRegistry`, the exposure projection, output/truncation, the
  read-side registries, plus the two cross-cutting policies every
  implementation shares — `sandbox` (process confinement) and the
  new `workspace` module (path confinement, previously a private
  helper inside `read`). It ships **no agent-facing tool set**; the
  only `Tool` impls left in it are the two synthetic contracts the
  harness needs (`__get_full_output`, `structured_output`) and the
  registry's `ToolEntry::dynamic` passthrough.
- **Five tool plugins, one tool each**: `uva-tool-read`,
  `uva-tool-write`, `uva-tool-shell`, `uva-tool-todo`,
  `uva-tool-web`. Each depends only on `uva-tool`, carries
  its own tests and one runnable example, and is the only thing
  that pulls what that tool needs (`uva-tool-web` owns
  `reqwest`; the other four are offline). `build_default_tool_registry`
  is gone — a consumer registers the bricks it wants with
  `registry.register_entry(ToolEntry::new(Arc::new(ReadTool::new())))`.
- **The `task` tool left the agent crate**: the new
  `uva-delegation` plugin owns the whole multi-agent family —
  `TaskSpec` + `task_tool_definition`, the verification `gate`, git
  `worktree` isolation, the `@agent:` `router` / `route_rules`, the
  `mention_clone` path, and the two-lane `SubagentPool`.
- **A generic interceptor seam replaced the hard-coded branch**:
  `uva_agent::ToolInterceptor` (`definitions` / `claims` /
  `execute`, handed an `InterceptorCall` carrying the run's cancel
  token, depth and event emitter) plus
  `ReActAgent::with_interceptor` / `AgentBuilder::interceptor`. The
  loop advertises interceptor definitions next to the registry's
  (same restriction filtering) and dispatches claimed calls before
  registry lookup — so with no plugin installed there is no `task`
  tool, no delegation code, and no multi-agent coupling in the
  harness. `TaskDelegator` is that trait's first implementation.
- **Facade**: `tool-read` / `tool-write` / `tool-shell` /
  `tool-todo` / `tool-web` / `delegation` are features and modules of
  their own (`uva::tool_read::ReadTool`, …), all in the default
  set; `tool` now means the paradigm. `prelude` exports `ToolEntry`
  in place of the removed `build_default_tool_registry`.
- **Gates and docs**: `make check-mvp-deps` additionally asserts the
  four offline tool crates pull no HTTP/OTel/DB dependency;
  MINIMAL.md, SEAMS.md, README.md, AGENTS.md §1/§3.7, and the example
  index describe the plugin composition.
- **Server**: `uva-app-server` composes the plugin tools
  (`plugin_tool_registry()`) and installs `TaskDelegator` through
  `with_interceptor` at all three wiring sites (default member,
  `/agents` route, session factory).

### Fixed (R124 — the four deferred harness gaps)

The four gaps the harness audit recorded as open are closed. Each was
reproduced first; the two behavioural ones were shown to fail against
the pre-fix code before landing.

- **Reasoning now reaches the conversation history.** `stream.rs` folded
  chunks itself and carried no reasoning at all, so `commit_assistant` —
  which rebuilt the assistant message from `assistant_text` +
  `tool_uses` — dropped every thinking block. `uva-provider` states
  the contract as the *agent layer's* job in four places
  (`anthropic/types.rs`: the `signature` is "Required to preserve
  reasoning continuity across turns"; `types/stream_chunk.rs`: it is
  "propagated so the agent can preserve cross-turn reasoning
  continuity") and its Anthropic adapter already maps
  `ContentPart::Reasoning` → `ThinkingBlock { thinking, signature }` —
  the harness was the missing half, and Anthropic interleaved thinking
  with tool use requires echoing those blocks back. A probe reported
  `request 2 has reasoning part = false, signature preserved = false`
  before the fix; `reasoning_reaches_the_next_request` pins it now.
  The ordered parts travel through `SampleOutcome` rather than a
  `reasoning: String` sibling, because order is the point: the provider's
  canonical fixture is `[Reasoning, Text]`, reasoning first.

  Committing them verbatim would have been wrong for a different reason:
  the assembler emits one part per **streamed delta**, so a three-delta
  thinking block would commit as three `Reasoning` parts and an answer as
  N `Text` parts. History records *blocks*, so `commit_assistant` now
  coalesces adjacent same-kind runs — one thinking block per block (which
  is what Anthropic requires per prior assistant turn), one text part, and
  a `Text` after a `ToolUse` left separate because it is a different
  block. This also restores the single joined `Text` part history used to
  carry before the rewrite — on the *every-answer* path, so without it the
  fix would have introduced a wire-shape regression: N consecutive `text`
  blocks where a single joined part used to go.
  `streamed_text_deltas_commit_as_one_block` pins that half (removing the
  `Text` arm now fails it; before, the arm was removable with the whole
  suite green, because the two tests that stream multiple text deltas
  assert only the wire events and never reach a second request).

  Landing that exposed a second break one layer down, in the provider's
  own fold: Anthropic's `signature_delta` arrives *after* the
  `thinking_delta`s that carry the text, so a streamed
  `Content(Reasoning)` chunk cannot know its signature — the processor
  stamps it `None` and the value reaches the assembler only on `IsDone`,
  which `fold_is_done` skipped (streamed reasoning had already set
  `saw_streamed_reasoning`) and `finalize_assistant` then hardcoded to
  `None`. Reasoning would have arrived in history *unsigned*, which
  Anthropic rejects, so the reported gap would have looked fixed while
  still broken. `BlockAssembler` now records the deferred signature and
  stamps it onto the trailing un-signed reasoning run at finalize (one
  block produces several parts: the `content_block_start` text plus one
  per delta). `anthropic_stream_signature_survives_to_the_assembled_parts`
  drives real SSE bytes through the production processor *and* the
  production assembler — no hand-built chunk — and asserts the
  precondition that the streamed chunk is genuinely un-signed, so a
  `None` part cannot pass.
- **`stream.rs` no longer re-implements `BlockAssembler`.** The
  accumulator is now the provider crate's tested fold; the harness keeps
  only the half that is genuinely its own — *when* an assembled part
  becomes an `AgentEvent::Model` — as one publish cursor. That removed
  the drift the duplication had already caused (no reasoning, no JSON
  repair on malformed tool arguments) and deleted
  `extract_tool_uses` / `parse_tool_input`, which existed only to serve
  the second copy.
- **The streaming path retries a transient failure.** `complete`
  retried with per-class backoff; `complete_with_stream` — the only path
  a ReAct iteration takes — handed any `Err` back as session-fatal, so a
  429 or a capacity blip ended the run. Establishment is now wrapped in
  the same `retry_with_classification` budget, and *only*
  establishment: once deltas have reached `on_delta`, re-sending would
  emit them twice and the consumer cannot un-see them. Both adapters
  covered; the boundary is pinned by a test asserting one request after
  a mid-stream abort.

  Wrapping establishment in a bounded budget created a window that did
  not exist before, so the same change closes it: the token is checked
  **inside** the retry closure, before every attempt. Without that check
  a cancelled run — or a disconnected client — would be held in backoff
  for up to the full budget (a sustained 429 is 6 attempts over a 1s→30s
  ramp, ~31s) before the cancellation was noticed, where a single attempt
  used to fail fast. A cancelled run now sends **zero** requests and
  returns `StreamAborted`, on both adapters; that relies on the variant
  classifying as `Permanent` (`max_attempts: 1`, no sleep), which
  `classify_stream_aborted_is_permanent` now pins next to the other
  `Permanent` assertions.
- **A declared-but-unsatisfied output schema is now observable, and the
  prompt asks for it.** `with_output_schema` only *offers* the
  `structured_output` tool — it cannot force the call, and a text-only
  final answer is a normal way to end — so a consumer holding a schema
  could not tell "the model answered in prose" from "the model answered
  in the shape I asked for". Finalize now emits
  `WarningKind::StructuredOutput` when a schema was declared and no
  schema-valid submission was committed. Deliberately **not** fatal:
  prose is a legitimate answer, and forcing a retry would change what
  `Completed` means for every existing agent. The fact is recorded on
  the run's own state rather than recovered from the transcript, because
  history stores a tool result as content + call id and the tool name /
  `is_error` that decide validity exist only on the wire.

  Validating alone would have been hollow: a tool list is a menu, not an
  instruction, so nothing told the model that prose was not an accepted
  ending and the warning would have fired on nearly every
  schema-declaring run. The prompt now carries a `<structured_output>`
  section when the descriptor declares a schema — the one fact the
  native `tools` channel cannot express. The schema *text* stays out of
  the prompt (it is the tool's `parameters()`, already delivered
  natively).

Also in this change: the OpenAI-compatible adapter no longer emits a
`{"type":"reasoning"}` content block. It never defined a request-side
slot for one — real OpenAI rejects an unknown block type, and the
gateways that expose reasoning expose it on the response only — so now
that reasoning actually reaches a message it is dropped at the wire
instead, where the adapter owns the shape. A message left with nothing
transmittable is dropped rather than sent as an empty `content: []`.
This is the deliberate asymmetry with Anthropic, which *requires* the
block back.

**Reachability, stated plainly.** The *signed*-reasoning half is
latent, and the CHANGELOG should not pretend otherwise: nothing in
`uva-provider/src/anthropic` ever sets a `thinking` / `budget_tokens`
request field and `AnthropicRequest` never reads `extra_body`, so
extended thinking cannot be enabled through this adapter, and only real
extended thinking makes the upstream emit `signature_delta`. The
signature plumbing is therefore defensive correctness for the day that
capability is exposed — and it is now bounded, so it cannot forge a
signature (see the interleaved test).

What *is* reachable is **unsigned** reasoning: OpenAI-compatible
`reasoning_content` gateways and `<think>`-marker-compatible upstreams
both produce `ContentPart::Reasoning` with `signature: None`. For those,
the two adapter changes above are the whole story — the part now reaches
history in-run and is dropped at the wire in both directions rather than
producing an invalid request.

**Residual gap, recorded rather than papered over.** Reasoning is
**not durable** (`AgentEvent::Model(ContentPart::Reasoning)` is
ephemeral by the `event-durability-classification` spec, pinned by its
own test), and the server rebuilds a continued session's history from
that log (`session/controller/persist.rs`: append only
`if event.is_durable()`; `run_task.rs`: `let mut history = sink_history`).
So a *continued* session replays its assistant turns without the
thinking block. Today that is harmless — the reachable reasoning is
unsigned and would be dropped at the wire anyway — but it becomes a real
gap the moment extended thinking is enabled, because Anthropic then
requires the block back and a resumed turn would not have it. The fix
belongs with that enablement, not before it: making reasoning durable
changes log size, `/sessions/:id` rendering, and session search.

Verification: `cargo test -p uva-harness --lib` **241/241**,
`cargo test -p uva-provider --lib` **695/695**, the `provider_test`
integration target **47/47**, `make ci`, `make test-crates`,
`make examples` all green.

### Fixed (R88 — regenerate renders the same story on every page)

The last open known gap: after a regenerate, `/chat/:id`
collapsed the turn to the replacement (`prompt, answer2`)
while `/sessions/:id` and "Continue chat" projected the
durable log verbatim and showed the prompt twice. The fix
gives the rerun's persisted prompt row real replace
semantics — the `SurfaceOp::Replace` the session fold
already understands — so every projection agrees at once:

- **Server**: `SessionOp::Rerun` now parks a `ParkedPrompt`
  marked `rerun: true`; the run task computes the replace
  op against the pre-rerun fold (the span from the last
  user-role message to the tail, cited by the shadowed
  rows' seqs) and stamps it on the `UserInput` row it
  persists. The durable log still holds the turn twice
  (an honest record), but `fold_log_surface` — the single
  projection behind the next run's history
  (`events_to_messages`), session search, the regenerate
  route's own prompt recovery, and `typed_messages_from_sink`
  — folds it to the replacement story. The `SurfaceLedger`
  splices the same span (its `record` already applies ops on
  any surface-eligible row), so later compaction checkpoints
  still resolve.
- **Client**: `session-to-messages.ts` honors the op —
  `historyToChatMessages` tracks each message's creating log
  ordinal, and a `UserInput` row carrying `surface_op`
  drops the messages its cited seqs shadow (the inclusive
  seq range also removes sub-agent bubbles the replaced
  answer produced) before appending its own. The
  session-detail page and "Continue chat" now agree with
  the chat page without either special-casing.
- **Defensive fallback**: a rerun against a log with no
  user message (the regenerate route refuses this up
  front) degrades to a plain append — no op, no span.
- **Tests**: 3 new controller tests
  (`rerun_prompt_row_shadows_the_turn_it_replaces`,
  `chained_rerun_shadows_the_previous_rerun`,
  `rerun_without_a_user_turn_appends_plainly`);
  `cargo test -p uva-app-server` 400 → 403 lib tests,
  all green.
- **Verified end-to-end** (Playwright MCP against
  `uva-app-server` + Vite): the durable log holds the turn
  twice with the op citing the shadowed rows; `/chat/:id`
  renders one prompt + the new answer; `/sessions/:id`
  renders the same.


### Changed (R87 — dead code violating AGENTS.md §3.2 pruned)

R86 closed the test-block split cycle. With no
further organizational candidates, the audit
returned to AGENTS.md §3.2 directly:

> New Rust code that is unused MUST be deleted; the
> `dead_code` / `unused` attributes must not be used
> to suppress warnings.

The audit grepped every `#[allow((dead_code|unused))]`
in library code and read each call site. Six files,
+12 / −74, net **−62 lines**, **zero new warnings**.

- **3 phantom suppressor functions removed**:
  `delegation::_gate_module_link`, `worktree::_worktree_marker`,
  `summarizing::_type_anchor` — empty bodies whose
  `#[allow(dead_code)]` only silenced a warning that
  didn't exist.
- **Dead fields / variants pruned**: `ShortRef`
  collapses to `{ short_id }` (its `tool_call_id` /
  `tool_name` were set but never read); `PartialBlock`
  loses its unused `Text` and `Reasoning` variants
  (text/reasoning accumulate as separate
  `BlockAssembler` fields, not as `PartialBlock`
  entries); `LockGuard` loses its unused `root` and
  `job_id` fields.
- **1 dead helper deleted**: `home_dir_fallback` —
  no callers, no purpose.
- **Imports trimmed**: `ToolUse` (genuinely unused in
  `summarizing.rs` lib code) deleted; `ToolResult` and
  `TextContent` moved from the parent `use` to a
  test-local `use` (they were only consumed in
  `mod tests`); `GateVerdict` removed from
  `delegation.rs`'s private import list.
- **rustdoc link fixed**: the
  `[GateVerdict]` intra-doc link in
  `delegation.rs` was redirected to the
  `DelegationGateVerdict` alias that the `pub use`
  makes visible. The `make ci` `doc-check` gate
  caught this on the first run — exactly the kind
  of cross-crate coupling the gates are for.

Legitimate `#[allow(dead_code)]` kept: the
`uva-mcp` `Child` field (held for `Drop` impl),
and the test-module clippy allowlists.

`cargo build --workspace --all-targets`: zero warnings.
`make ci` 6/6; `make test-unit` 2447/2447 (unchanged);
`make examples` exit 0 with proof lines.

### Changed (R86 — test-block split cycle closed)

R85 named `mod scope` / `mod descriptor_cache` /
`mod snapshot_cache` as the next R86 candidate. On
inspection, those three names mapped to line ranges
in the pre-R81 file that **already collapsed into the
sections R82–R85 extracted** (`mod snapshot`,
`mod registration`, `mod dispatch`, `mod mutations`).
The deferred list is now closed.

This is the audit-closure round: no code moves. The
parent `mod tests` block at
`crates/uva-tool/src/registry.rs` (lines 1369–3297)
now holds exactly:

- 8 sub-module declarations (`mod fixtures` + 7
  others)
- 8 sub-module intro comments
- 8 one-line "moved to ... above" markers
- the parent-level `pub(crate) use fixtures::{...};`
  re-export

Zero `#[test]` / `#[tokio::test]` items live at the
parent block's indent (`^    #\[(test|tokio::test)\]`).
Every test now lives in one focused sub-module. The 8
sub-modules are all verified by name-filter
(`fixtures` → 0 filter, `exposure` → 7,
`session_scope` → 4, `registration` → 24, `snapshot`
→ 16, `dispatch` → 17, `mutations` → 2,
`argument_validation` → 5).

`cargo test -p uva-tool --lib` 283/283 (unchanged);
`make ci` 6/6; `make examples` exit 0 with proof lines.

### Changed (R85 — dispatch tests moved to a focused sub-module)

R84 named `mod dispatch` as the next R85 candidate.
6 tests (per-call truncation bound, streaming dispatch
collects the final Result, empty/progress-only streams
are contract violations, snapshot_with_provenance
carries provenance + filters/sorts) move from the
parent `mod tests` block into `mod dispatch`. The four
local `Tool` fixtures (`LargeOutputTool`,
`StreamingTool`, `EmptyStreamTool`, `ProgressOnlyTool`)
travel with the section — each is referenced only by
these tests.

- No test renamed, no assertion touched, no fixture
  behaviour change.
- `mod dispatch` is `#[cfg(test)]`-only; `make
  check-public-api-runtime` green.
- `cargo test -p uva-tool --lib dispatch` → 17
  passed (the 4 fixtures are `#[derive(Debug)]`
  structs without tests; the 6 new `dispatch::*` tests
  plus 11 existing `dispatch_*` tests in
  `mod registration` both match the `dispatch`
  substring).

Eight sub-modules now own focused concerns within the
parent `mod tests` block: `argument_validation` /
`session_scope` / `exposure` / `snapshot` /
`mutations` (R81–R82), `fixtures` (R83),
`registration` (R84), `dispatch` (R85).

`cargo test -p uva-tool --lib` 283/283 (unchanged);
`make ci` 6/6; `make examples` exit 0 with proof lines.

### Changed (R84 — registration/trait tests moved to a focused sub-module)

R83 named `mod registration` as the immediate
follow-up. R83's `mod fixtures` made this a pure
section move — every shared `Tool` impl now lives
in one focused sub-module, so this extraction drags
no shared state with it. 30 tests (register_entry
accept/refuse/increment, unregister semantics,
descriptor/snapshot caches, snapshot ordering/dedupe/
hidden-filter, `get`/`list`/`contains_and_len`
through the public surface and the `Registry` trait,
plus dispatch-shape coverage with one-off `Tool`
impls) move from the parent `mod tests` block into
`mod registration`. Two `Registry`-trait tests use
`super::super::registry_trait::ToolFilter` (private
sibling module).

- No test renamed, no assertion touched, no fixture
  behaviour change.
- `mod registration` is `#[cfg(test)]`-only; `make
  check-public-api-runtime` green.
- `cargo test -p uva-tool --lib registration` →
  24 passed (filtered).

Six sub-modules now own focused concerns within the
parent `mod tests` block: `argument_validation`
(R81), `session_scope` (R82), `exposure` (R82),
`snapshot` (R82), `mutations` (R82), `fixtures`
(R83), and `registration` (R84).

`cargo test -p uva-tool --lib` 283/283 (unchanged);
`make ci` 6/6; `make examples` exit 0 with proof lines.

### Changed (R83 — shared test fixtures moved to a focused sub-module)

Three `Tool` impls (`TestEntryTool`, `ShadowTool`,
`NamedTool`) were defined inline in the Registration/Trait
section but referenced from every section of the parent
`mod tests` block (registration, exposure, session_scope,
snapshot, descriptor cache, mutations) — six sections,
three fixtures, eighteen cross-references. Each fixture
had one canonical definition, but the *location* was
wrong.

All three impls move to `pub mod fixtures` at the top of
the parent block. `pub(crate) use fixtures::{...};` at the
parent level re-exports the three types, so every existing
test body — every `Arc::new(TestEntryTool)`,
`NamedTool("alpha")`, `Arc::new(ShadowTool)` — keeps
compiling unchanged.

- No test body / no assertion / no fixture behaviour
  changes.
- `mod fixtures` is `#[cfg(test)]`-only; `make
  check-public-api-runtime` green.
- Sets up `mod registration` (R84) for a pure section move
  on the same R81/R82 pattern.

`cargo test -p uva-tool --lib` 283/283 (unchanged);
`make ci` 6/6; `make examples` exit 0 with proof lines.

### Changed (R82 — four more test sections moved to focused sub-modules)

Round opened with a full re-audit against the standing
objective: reference parity is closed (no commit in
`traitclaw` / `pi` since 2026-09-10; every
`reference-parity.md` row mapped / declined / closed),
no drift surface is open (the R76–R80 grep now hits only
test assertions), and all architecture gates green.

What landed — four commits, all the R81 pattern
(sub-module inside the parent `mod tests`, one-line
marker at the original position, zero logic change):

- `mod session_scope` — the 4 `create_session_scope`
  lifecycle tests (this was the uncommitted extraction
  sitting in the working tree from the previous
  session; verified then committed).
- `mod exposure` — the 5 exposure-plumbing tests
  (default `Direct`, entry→descriptor carry,
  descriptors-vs-snapshot privacy split, `Registry`
  get/list preservation, hidden-but-executable
  dispatch).
- `mod snapshot` — the 7 dual-index snapshot/catalog
  tests (name sorting, unregister reflection, empty
  registry, entry metadata builders, canonical
  resolve, materialisation content, scoped-arc drop).
- `mod mutations` — the 2 post-registration mutation
  tests (`set_exposure` projection reach + version
  semantics; `set_hidden` gating pinned apart from
  `Hidden` exposure).

23 of the 283 lib tests now live in five filterable
sub-modules; every filter verified
(`session_scope` → 4, `exposure` → 5, `snapshot::` → 7,
`mutations` → 2, `argument_validation` → 5).

`cargo test -p uva-tool --lib` 283/283 after each
commit; `make ci` 6/6; `make test-unit` 2447/2447
(unchanged); `make examples` exit 0 with proof lines.

Remaining flat sections (registration + fixtures,
dispatch, the small cache/scope sections) are R83+
candidates on the same pattern.

### Changed (R81 — R74 tests moved to a focused sub-module)

`uva_tool::registry::tests` was 1692 lines / 56 822
bytes — the largest `mod tests` in the workspace. The
R74 argument-validation block (198 lines, lines 2862–3059)
was the most self-contained section: recent, focused,
with its own `ShellTool` fixture. Extracted to a new
`mod argument_validation` inside the parent block.

- The 5 R74 tests now report under
  `argument_validation::*` (`cargo test
  argument_validation` jumps straight to them).
- `use super::*;` brings the parent block's `Tool`,
  `ToolOutput`, `Context`, `ToolRegistry`, `ToolEntry`,
  `TestEntryTool`, `collect_results`, etc. into the
  sub-module.
- One-line marker in the parent block at the original
  position pointing at the new sub-module.
- No test logic changes; no fixture changes; no new
  public surface.

`cargo test -p uva-tool --lib` 283/283 (unchanged
total; the 5 R74 tests just moved sub-module). `make ci`
6/6; `make test-unit` 2447/2447 (unchanged).

The other 1450 lines of the parent `mod tests` block
can follow the same pattern in R82+: `mod exposure`,
`mod scope`, `mod registration`, `mod descriptor_cache`,
`mod snapshot_cache`, `mod snapshot`, `mod dispatch`,
`mod mutations`, `mod materialization`, plus a shared
`mod fixtures` for the `TestEntryTool` / `ShadowTool` /
`NamedTool` types. Each is a future round; R81
establishes the pattern.




### Added (R80 — shared `MAX_LIMIT` constant, one canonical home)

Same drift pattern as R77/R78/R79, but for a
**constant** rather than a function. Two near-duplicates
of the same value, in two crates, with a doc comment
saying *"MUST stay in sync"*: `uva_core::registry::
REGISTRY_MAX_LIMIT = 100` (private, used by
`paginate_registry_list`) and `uva_server::api::v1::
page_query::MAX_LIMIT = 100` (public, 8 reference sites
including handlers and tests). A refactor that changed
one without the other would silently desync the
wire-level cap from the in-memory registry pagination
cap.

- The core's constant renamed `REGISTRY_MAX_LIMIT` →
  `MAX_LIMIT` and made `pub`. The doc comment
  documents the role and the relationship to the
  server's wire-level cap.
- The server's `MAX_LIMIT` is now `pub const MAX_LIMIT:
  u64 = uva_core::registry::MAX_LIMIT;` — same
  public symbol name, value sourced from core. The
  doc comment explains the alias and the
  single-source-of-truth contract.
- 1 new test in `uva-core` (135 total, was 134):
  `max_limit_matches_spec` — pins the value to `100`.
  The server's existing `constants_match_spec` test
  also asserts the alias equality — drift detector.
- `uva_core::lib.rs` re-exports `MAX_LIMIT`; the
  module-doc table row for `registry` mentions the
  constant.

`make ci` 6/6; `make test-unit` 2447/2447; `make examples`
exit 0 with `CONSUMER-PROOF: OK` + `MVP-OK` +
`BEST-OF-N-JUDGE: OK`.




### Added (R79 — shared cursor codec, one canonical home)

`uva_core::registry::encode_registry_cursor` and
`decode_registry_cursor` were private 3-line copies whose
doc comment said *"The encoding MUST match
`uva_server::api::v1::decode_cursor`"* — the exact
drift surface R76 closed for the judge parser. Two
near-duplicates of the same URL-safe base64 codec, one
in core (the in-memory registry pagination) and one in
the server (the HTTP transport), that *must* stay in
lockstep but were physically separate.

New module `crates/uva-core/src/cursor.rs` —
`pub fn encode(id: &str) -> String` and
`pub fn decode(cursor: &str) -> Result<String, Error>`.
The contract: URL-safe base64, no padding, UTF-8
validated. Both failure modes (invalid base64, invalid
UTF-8) collapse to `Error::InvalidItem` because a
wire-level bad-request is the same regardless of which
stage failed. The module-doc documents the encoding's
*stability* guarantee (a cursor emitted by an older
`uva-core` round-trips on a newer one and vice
versa) so a future algorithm change is a documented
breaking change.

6 new tests in `uva-core` (134 total, was 128):
`encode_matches_spec_example` (the literal
`task_abc → dGFza19hYmM` from the doc comment, pinned
as a doctest AND a test), ASCII round-trip, unicode
round-trip (`task_αβγ`), invalid-base64 rejection,
non-UTF-8 payload rejection, and `empty → empty`
round-trip (an empty cursor is the start-of-list
marker, not an error).

The core's private `decode_registry_cursor` /
`encode_registry_cursor` become 2-line delegations
over the new public functions. The server's
`encode_cursor` / `decode_cursor` likewise become
2-line delegations; the 4 server tests
(`encode_cursor_matches_spec_example`,
`decode_cursor_round_trip`,
`decode_cursor_handles_unicode_ids`,
`decode_cursor_rejects_invalid_base64`,
`decode_cursor_rejects_non_utf8_payload`) pass
byte-for-byte against the delegation. The
non-UTF-8 test still uses `base64` directly to
build a payload `uva_core::cursor::encode`
cannot produce (a non-UTF-8 string).

`make ci` 6/6; `make test-unit` 2446/2446; `make examples`
exit 0 with `CONSUMER-PROOF: OK` + `MVP-OK` +
`BEST-OF-N-JUDGE: OK`.




### Added (R78 — shared redaction helper, one canonical home)

`uva_server::api::v1::validation::api_key_mask` was a
19-line function with 6 dedicated unit tests for the
partial-redaction contract (first 4 + `***` + last 3,
≤7-char full-mask, multibyte safe, no-leak guarantee).
The natural home for it is `uva_core::sensitive` —
the same crate that already hosts `Sensitive` /
`SensitiveData` (the `Debug`-time redaction newtype), and
whose module-doc explicitly calls itself *"Sensitive
information encryption / redaction primitives"*.

- `uva_core::sensitive::redact_partial(s: &str) -> String`
  — first 4 + `***` + last 3, ≤7-char full-mask fallback,
  multibyte safe, empty input → empty output. The 4+3
  shape is the most common API key contract (Anthropic
  `sk-ant-…`, OpenAI `sk-…`, GitHub `ghp_…`).
- `uva_core::sensitive::redact_partial_with(s,
  keep_first, keep_last) -> String` — caller chooses the
  prefix/suffix lengths; falls back to `"***"` when the
  sum would equal or exceed the input.
- 8 new tests in `uva-core` (128 total, was 120):
  long key shape, empty, short input, exactly 8 chars,
  multibyte safe, no-leak guarantee, parameterised
  variant, parameterised fallback.
- The server's `api_key_mask` becomes 2 lines of
  delegation. The 6 server tests pass byte-for-byte.
  The public surface of `uva-app-server` is unchanged.
- `uva_core::lib.rs` re-exports `redact_partial` and
  `redact_partial_with` alongside `Sensitive` /
  `SensitiveData`; the module-doc table row mentions
  the new helper.

`make ci` 6/6; `make test-unit` 2440/2440; `make examples`
exit 0 with `CONSUMER-PROOF: OK` + `MVP-OK` +
`BEST-OF-N-JUDGE: OK`.




### Added (R77 — shared char-based truncator, one canonical home)

A workspace-wide grep for `s.chars().take(N).collect()` found
11 inline sites across 8 crates
(`uva-mcp`, `uva-session`, `uva-steering`,
`uva-rag`, `uva-attachment`, `uva-agent`,
`uva-provider`, `uva-tool`, plus `uva-app-server`).
Each site had its own slight variation; the marker formats
were inconsistent (`…`, `...`, `[truncated]`,
`[truncated N chars]`, or none).

New `uva_core::text::truncate_chars(s: &str, max_chars:
usize) -> (String, bool)` is the char-counted, return-a-pair
variant. Returns `(s.to_string(), false)` when the input is
shorter than the cap, else `(first max_chars chars, true)`.
No marker — the caller decides. Sits next to the
byte-counted, in-place `cap_to_char_boundary` (which
already lived in `uva_core::text`).

`uva_server::session::controller::truncate` (14 lines,
6 dedicated unit tests) becomes a 6-line wrapper over
`truncate_chars` that adds the `…` marker. The 6 server
tests still pass — the contract is preserved byte-for-byte.

6 new tests in `uva-core` (120 total, was 114):
shorter verbatim, exact-match verbatim, one-over
truncated, empty input, zero max with non-empty (the
server's corner case), multibyte char-count (中文 / 日本語),
4-byte emoji.

The 10 inline `s.chars().take(N).collect()` sites stay
untouched on purpose — their output formats are part of
their call sites' contracts; a per-site refactor
preserves the marker each consumer expects. The
primitive is now in place for follow-up R50 work.

`make ci` 6/6; `make test-unit` 2432/2432; `make examples`
exit 0 with `CONSUMER-PROOF: OK` + `MVP-OK` +
`BEST-OF-N-JUDGE: OK`.




### Fixed (R76 — shared LLM-judge reply parser, one canonical home)

`uva_eval::metrics::parse_score` and
`uva_agent::agent::parse_judge_score` were two
near-duplicates of the same parser. R73 documented the
duplication and promised "byte-compatible" behaviour; in
practice the two did not accept the same inputs. The
agent's parser was the documented contract (case-insensitive
on the `Score:` prefix, accepting both `Score: 0.5` and
`Score:0.5`); the eval's was case-sensitive and rejected
`Score:0.5`. A judge that said `score: 0.5` (lowercase)
silently scored `0.0` in `LlmJudgeMetric` and `0.5` in
`LlmJudgeScorer`. The bug was unreachable to the existing
tests (none pinned the lowercase case), but the contract
said "case-insensitive" and a real judge that varied its
casing would surface it.

The right home is `uva_core` — the same crate that
already hosts `validate_against_schema`. Both `uva-eval`
and `uva-agent` already depend on it, and the parser
is a pure function with no async, no runtime, no
dependencies. New module `crates/uva-core/src/judge_score.rs`
+ `pub use judge_score::parse_judge_score;` from
`uva_core`. The agent's and eval's parsers are now
thin wrappers over the shared one.

- 7 new tests in `uva-core` (114 total, was 107):
  `parses_score_prefix_with_space`,
  `parses_score_prefix_without_space`,
  `parses_standalone_number`, `finds_score_on_later_line`,
  `clamps_out_of_range`, `unknown_reply_scores_zero`,
  `handles_lowercase_and_mixed_case`.
- 70 lines of duplicated parser code in
  `uva-agent/src/agent/best_of_n_llm_judge.rs` deleted
  (the local `stripped_prefix_ci` and `parse_first_f64`
  helpers); replaced by a 1-line delegation.
- The 4 eval tests and 5 agent tests for the parser
  still pass (the new contract is a strict superset).
- `make ci` 6/6; `make test-unit` 2426/2426;
  `make examples` exit 0 with `CONSUMER-PROOF: OK` +
  `MVP-OK` + `BEST-OF-N-JUDGE: OK`.




### Added (R75 — runnable example for R74)

`crates/uva-tool/examples/argument_validation.rs` proves
the R74 `with_argument_validation` switch end-to-end: the same
`EchoTool`, two runs (off / on). The off path counts 2 tool
bodies (the default unchanged-by-R74 contract — even the
malformed call reaches the tool body); the on path counts 1
(the malformed call is rejected at the registry with
`is_error: true` and the dotted-path violation in the body).
`make examples` now picks it up alongside the other 34
example programs. The example ends with
`ARGUMENT-VALIDATION: OK`.

No library code change; the surface (`with_argument_validation`
+ `argument_validation_enabled`) was already in place. The
example is the only new file this round; the round map,
[`docs/examples/README.md`](docs/examples/README.md), and this
CHANGELOG entry were updated in the same commit.

`make ci` 6/6; `make test-unit` 2419/2419; `make examples`
exit 0 with `CONSUMER-PROOF: OK` + `MVP-OK` +
`BEST-OF-N-JUDGE: OK` (the new example is run, but its
`ARGUMENT-VALIDATION: OK` proof line is for direct invocation,
matching the convention of the other examples).




### Added (R74 — tool-argument validation in the registry, opt-in)

The dispatcher's hot path used to call `tool.stream(input, ctx)`
without checking `input` against the tool's `parameters()`.
Each tool validated its own arguments inside `call`, which
works but means a misbehaving model call (a missing required
field, a wrong-typed nested value) reaches the tool body and
fails there. pi's `agent-loop.ts` already does this in the
runtime with `validateToolArguments`; Uva now has the same
shape, opt-in:

- `ToolRegistry::with_argument_validation(bool)` — builder.
  Off by default, so the hot path is unchanged for consumers
  that already validate inside `Tool::call`. A deployment that
  wants the registry to surface a schema mismatch as an
  `is_error` tool result flips it on once at boot.
- `ToolRegistry::argument_validation_enabled()` — introspection.
- A failed match synthesises a `Plan::Invalid` result with
  every dotted-path violation in the body, in the same
  numbered-list style the eval crate uses:
  ```
  invalid arguments for tool `shell` (2 violations):
  - cmd: expected required property present, found missing
  - timeout: expected type integer, found string
  ```
  Paid under the same read lock as the tool lookup, so a
  misbehaving call costs a synthesized error Result instead
  of a spawned task that runs a tool it shouldn't.
- `Clone` carries the flag through (tested).
- 5 new tests in `uva-tool --lib` (283 total, was 278):
  default off, validation on synthesises the error, valid
  input still runs, `Clone` preserves the flag, empty
  schemas stay permissive.

The validator is `uva_core::validate_against_schema` — the
same primitive `StructuredOutputTool` and the eval crate's
`SchemaValidationMetric` already use. No new crate, no new
dependency, no new feature flag. `make check-public-api-runtime`
still clean (the new method takes `bool` and returns `Self`).

`cargo test -p uva-tool --lib` 278 → 283; `make test-unit`
2414 → 2419; `make ci` 6/6; `make examples` exit 0 with
`CONSUMER-PROOF: OK` + `MVP-OK` + `BEST-OF-N-JUDGE: OK`.




### Added (R73 — LLM-as-judge scorer, in-tree)

The `BestOfNStrategy` gap noted since R50 is closed. The trait
`CandidateScorer::score` is now `async fn` (additive — every
existing impl gains `#[async_trait]` and the body is unchanged),
and a new `LlmJudgeScorer` in
`crates/uva-agent/src/agent/best_of_n_llm_judge.rs` holds an
`Arc<dyn ModelProvider>` to grade each candidate against a rubric:

- `LlmJudgeScorer::new(provider)` uses the in-tree
  `DEFAULT_JUDGE_PROMPT` (one-sentence rubric instructing the
  judge to reply with `Score: 0.0..=1.0`).
- `LlmJudgeScorer::with_rubric(provider, rubric)` is the seam for
  a deployment's own criteria.
- `with_max_tokens(u32)` caps the reply; default 32.
- `parse_judge_score(reply) -> f64` is the same shape
  `uva_eval::metrics::parse_score` uses (`Score: …` on any
  line, case-insensitive, clamped to `0.0..=1.0`, fallback to a
  standalone number, `0.0` on no match). Duplicated rather than
  re-exported because the eval crate is for offline grading, the
  agent runs in production — the two stay byte-compatible by
  design. Re-exported from `uva_agent` as
  `LlmJudgeScorer` and `DEFAULT_JUDGE_PROMPT`.
- Provider error path returns `0.5` (neutral) and logs. `0.0`
  would tie every failed judge at the bottom; `1.0` would bury
  the strategy's own failure path. `0.5` keeps the judge out of
  the ranking while the strategy records the failure.

`cargo test -p uva-agent --lib` 316 → 324 (8 new: 5 for
`parse_judge_score` directly, 3 through a stub provider including
the error-path neutral-fallback). The 6 existing `best_of_n`
tests still pin the strategy with a sync `Shortest` scorer. The
`strategy_swap.rs` example's `RequiresMarker` scorer gained
`#[async_trait]`; its body is unchanged.
- `crates/uva-agent/examples/best_of_n_judge.rs` — a runnable
  demo: 3 candidates, 3 judge calls, the highest-scoring candidate
  wins. Prints per-candidate judge scores and ends with
  `BEST-OF-N-JUDGE: OK`; picked up by `make examples`.
- `make ci` 6/6 green; `make test-unit` 2414/2414 pass; `make
  examples` exit 0 with `CONSUMER-PROOF: OK` + `MVP-OK` +
  `BEST-OF-N-JUDGE: OK`.




### Removed (R72 — `edit_message` route)

The route validated `message_id` and then dropped it, building a fresh
`SendMessageRequest` from the body and submitting `Prompt`/`PromptMulti`
— appending a *new* turn at the tail. The documented contract
("replaces the user message identified by `message_id`… past turns
before `message_id` are preserved as history") was unimplemented on
both counts, and nothing called it from the frontend. Cutting it
removes a silently-wrong mutator and the cognitive-load of explaining
why a "replace" endpoint appends. The same replace semantics
(regenerate also appends rather than replacing the assistant turn) is
recorded as a known gap with the two ways to reconcile it.

The deleted surface:
- `routes::chat::edit_message` handler + `EditMessageRequest` type.
- The PATCH route registration in `routes/mod.rs` and the duplicate
  registration in `server/router.rs`.
- Four stale doc comments (`lib.rs` chat-surface list, the
  `server/router.rs` route-list comment + header comment, and the
  crate-level module-doc entry).

Test count unchanged: `cargo test -p uva-app-server --lib` 400 passed,
no call sites exist.


### Removed (R72 — the frontend e2e suite)

The Playwright e2e tree is gone, by the maintainer's call that it was
too slow to be worth its cost.

- `uva-web/tests/` (44 specs, ~5,200 lines), `playwright.config.ts`,
  `playwright.contract.config.ts`, and `.github/workflows/e2e.yml`.
- `uva-web/scripts/e2e/` — 709 lines of ad-hoc browser scripts,
  superseded by the suite they predated (and unusable since
  `playwright` left `package.json`).
- `contract-closure/contract-coverage.ts` + its test, and the `coverage`
  npm script, and the `test-contract-closure-playwright` CI job: their
  only input was the deleted spec directory, so they could only measure
  nothing.
- `uva-web/scripts/_logo-snapshot.mjs` — a throwaway that imported
  `playwright`, now uninstalled.
- The `@playwright/test` and `playwright` devDependencies, the
  playwright npm scripts, `make test-e2e` / `test-e2e-ui` /
  `test-e2e-headed` / `test-e2e-report` / `install-e2e-deps` /
  `test-web`, and the `test-results`/`playwright-report` ignores.

Why: a full run took **13 minutes** because the top-level `webServer`
booted `cargo run -p uva-app-server` plus Vite for every invocation, and
its value was undermined by silent rot — three specs had been red on
master for months because their fixtures still used a pre-refactor wire
shape (`{messageId, role: 'ROLE_AGENT', parts}` with no `type`, which
`applyHistoryEntry` cannot dispatch). Frontend verification is now
Playwright MCP driving the running app (`AGENTS.md` §4.2), which
exercises the real code instead of a second contract that has to be
maintained. The MCP browser comes from the harness, not from
`uva-web`'s dependencies — verified by driving the app after the
`playwright` package was uninstalled.

What still gates a frontend change: `make check-web` (tsc + eslint +
prettier). Nothing runs on behaviour automatically, and that is the
trade this makes — recorded in `docs/README.md`.

### Fixed (R72 — four frontend lockfiles collapsed into one)

`uva-web/` tracked `package-lock.json` **and** `pnpm-lock.yaml`; the
repo root added a phantom npm workspace (`package.json` =
`uva-frontend-root`, `workspaces: ["uva-web"]`) with its own
`package-lock.json` + `pnpm-lock.yaml`. Nothing invoked pnpm — no
`.npmrc`, no CI step, no Makefile target, no Dockerfile — and nothing
installed at the root.

The divergence was not cosmetic. Measured:

- `cd uva-web && npm ci` resolved the **root** lock (the workspace
  declaration wins) and exited 0 even with `uva-web/package-lock.json`
  deleted — so that file was dead weight for CI and the Makefile.
- The same file is what `Dockerfile.web` consumes, because it copies
  only `uva-web/` and has no workspace root. There it **failed**:
  `npm error Missing: @radix-ui/themes@3.3.0 from lock file`, exit 1 —
  identically at HEAD, so the Docker web image had been unbuildable
  independently of any R72 change.
- The root package declared `react-router-dom ^7.18.1` while
  `uva-web` declares `^6.30.4`; the app resolves 6.30.4. The root's
  copy was a second major that nothing imported.

Now: one lock (`uva-web/package-lock.json`, regenerated standalone
so it resolves without a parent workspace — 412 packages, no
`@radix-ui/themes` gap, no playwright), the phantom root workspace and
all `pnpm-*` files deleted, and `.github/workflows/rust-quality.yml`'s
`cache-dependency-path` re-pointed at the survivor.

Verified against all three consumers: `cd uva-web && npm ci` → 0;
the `Dockerfile.web` shape (only `uva-web/package.json` +
`package-lock.json` in an empty dir) → `npm ci` installs 361 packages,
`@radix-ui/themes` present, `playwright` absent; then a real install and
`tsc` + `eslint` + `vite build` green in `uva-web/`.

### Fixed (R72 — multimodal reaches both ends of the chat surface)

R71 made the model see a tool's image. The web UI still threw it
away: `toolResultContentText` in `uva-web`'s history
reconstructor kept only parts carrying a `.text` field, so a
`tool_result` whose content was `[text, image]` replayed as bare
text and an image-only result replayed as nothing at all. The live
SSE path had the same hole.

Six adjacent defects in the same surface surfaced while fixing it
and are fixed too:

- **Live tool results rendered an empty body.** `ToolResult` has no
  `text` field — its body lives in the `content` array — but
  `extractFromMessage` hardcoded `text: ''` and `partMetadata` only
  read `part.text`. `tests/e2e/ui/chat-tool-result.spec.ts` had been
  red on master for this.
- **URL-sourced images were wrapped as base64.**
  `ImageContent.data` is polymorphic: the server writes the remote
  URL itself for a `url` attachment
  (`uva-app-server/src/routes/chat.rs`, `build_parts`), so a blanket
  `data:…;base64,` prefix produced a broken `<img>`.
- **Image payloads would have bloated localStorage.** The persist
  path swallows `QuotaExceededError`, so a single multi-MB
  screenshot would silently stop the whole conversation from
  persisting. `stripAttachmentSegments` is extended to `images`, and
  `seedChatFromSession` (the "Continue this session in chat" path)
  previously bypassed that strip entirely.
- **The click-to-enlarge link could not work.** Browsers refuse
  top-frame navigation to a `data:` URL — the common case — so it
  became an in-place expand toggle.
- **The user's own attached picture never rendered.**
  `handleSubmit` set `attachments` on the sent message, but
  `ChatMessageViewItem` has no `attachments` field and the list
  renders only `segments`, so an image-only send produced an empty
  `> USER` bubble. New `imagesFromAttachments` normalises the
  composer's uploads onto the turn's text segment.
- **Retry dropped the picture, and the Retry banner was
  unreachable.** The retry effect rebuilt the user message from
  `lastSubmittedText` alone and re-sent without `attachments`, so a
  retried multimodal turn re-ran the model without the image and
  appended an image-less bubble; `handleRetry` also gated on
  `lastSubmittedText`, making an image-only prompt unretryable. It
  now caches the whole turn (`lastSubmission{text, attachments}`).
  Separately, `sendMessageStream` reports every failure as a
  *yielded* event, so ChatPage's `catch` — the only
  `setStreamError` caller — never ran and the banner never appeared;
  the `error` branch of `applyStreamEvent` now raises it, and a
  failed turn keeps the cached turn for Retry instead of clearing it.
  The same dead `catch` also stranded `setServerHealth(false)`, so a
  dropped connection left the header reading "Online"; the error
  branch now flips it, gated on `code === -1` (the transport-failure
  signal) so a real HTTP response — a 500 means the server answered —
  does not raise a false outage.

Rust side: a multimodal turn persisted **no `UserInput` row at all**.
`PromptMulti` parks the whole payload in `parts` and leaves the
drained text queue empty, so `prompt` was `""` and the
`if !prompt.is_empty()` guard skipped the append — a resumed session
then showed the assistant's reply with no question above it, and
`events_to_messages` handed the next turn's agent a history whose last
user message was missing. The run task now derives the text from the
payload's `Text` part (`prompt_text_of_parts`); the binary parts stay
out of the sink by design.

Changed files: `api/chat-message.ts` (new `ChatImage`, `extractImages`,
`contentText`, `imagesFromAttachments`), `lib/session-to-messages.ts`,
`lib/strip-attachment-segments.ts`, `api/chat-stream.ts`,
`pages/ChatPage.tsx`, `components/chat/ChatMessageView.tsx`,
`pages/ChatPage.css`; `session/controller.rs`.

Also repaired `tests/e2e/unit/is-tool-echo-log.spec.ts`, which was red
on master: its mock still used the pre-refactor `SessionTurn` shape
(`role: 'ROLE_AGENT'` / `messageId` / `parts`) and cited
`renderHistoryMessage`, which no longer exists, so
`applyHistoryEntry` never dispatched and no branch logged.
Modernised to the real `{type: 'Model', data: {type: 'text', …}}`
envelope; all six `isToolEchoText` branches now fire.

Scope note: user-prompt images are rendered live from the composer
(`imagesFromAttachments` → the turn's text segment) but are **not
durable** — the controller persists only the textual component of a
prompt (`uva-app-server/src/session/controller.rs`: "Raw image/audio
bytes are NOT serialised into the JSONL sink"), so a cold reload
rebuilds the turn from text alone. The tool-result half is durable
(`persist_and_broadcast` appends the whole `AgentEvent::Model`).

Verification for the frontend is browser-driven rather than
spec-authored: `extractImages` / `contentText` /
`imagesFromAttachments` were exercised through the running app, and
the render paths were confirmed against a live dev server with
`naturalWidth > 0` (a decoded image, not a silent broken icon) for
both the history-replay and live-SSE tool-result paths, plus the
composer attachment and retry flows.

### Fixed (R72 — `regenerate` reruns, and two stale claims)

`POST /api/v1/chat/sessions/{id}/regenerate` — the route behind the
chat UI's "Regenerate" button — could never rerun a turn.
`extract_last_user_parts` looked for rows shaped
`{"role":"user","parts":[…]}`; no writer in the repo emits that. Real
logs carry `{"type":"UserInput","data":{"text":…}}`, so the recovery
always returned `None` and the handler silently fell through to
`SessionOp::Cancel` — the button stopped the turn instead of replaying
it, and the whole `SessionOp::Rerun` arm was dead.

It now projects through the shared `fold_log_surface`, which is the
single decoder for every envelope the sink has carried (`UserInput`,
the typed `user_message` family, the lossless `Message` envelope, the
legacy `Model` parts) and honours a durable compaction checkpoint. A
hand-rolled matcher had rotted the moment the writer changed; going
through the fold means the reader cannot drift from the writer again.

`crates/uva-app-server/tests/regenerate_test.rs` pins it with three
tests through the real router: the most recent user turn is replayed
(observable as a second `UserInput` row — `Cancel` appends nothing), a
hand-written log with no user turn is reported rather than acted on,
and a multimodal turn's text replays. The two replay tests fail on the
old matcher (observed: `1 passed; 2 failed`); the miss test passes
either way, so it guards the miss contract, not the parser.

Two more defects became reachable once `Rerun` was live, and are fixed
with it:

- **A `Rerun` arriving mid-run was silently dropped.** The run arm
  parks its parts in `pending_multimodal` and skips starting a run
  while one is in flight; the completion branch then gated the restart
  on the *text* queue alone, which a rerun never fills. The parts sat
  parked until some later op's run consumed them — so the agent would
  answer that op's prompt with the stale rerun instead. The gate is now
  `maybe_start_run` itself, which checks both input slots and returns
  `None` when neither holds anything. Pinned by
  `rerun_during_an_active_run_still_starts_its_own_run`, which times
  out on the old gate.
- **A regenerate miss cancelled the session.** `None => SessionOp::Cancel`
  answered "no user turn recoverable" with the same op `/cancel`
  submits, so a lookup miss aborted the session as a side effect of a
  failed read. It now returns `404` and leaves the session running.

`rerun_hands_its_parts_to_the_factory` additionally pins that the
recovered parts reach the factory through the multimodal seam — the
semantics `Rerun` only now has, since it was unreachable before.

Two claims corrected while there, both of which described behaviour the
code never had:

- `SessionOp::Rerun`'s doc said it "carries the SAME multimodal
  payload as the original turn so the agent sees the same
  attachments". It cannot: attachment bytes are not persisted. The
  route doc said the turn is "replayed verbatim from the session log".
  Both now say the prompt **text** is replayable and the attachment is
  not.
- `AGENTS.md` §3.8 stated the clock-ratchet baseline as 18;
  `Makefile` has `CLOCK_BASELINE := 6` (R65/R66 and later rounds
  lowered it without updating the doc), which is exactly the
  rule-vs-config drift §3.4 warns about.

Also unified the `images` shape on rebuilt tool segments: the three
push sites in `session-to-messages.ts` now omit the field when there
are no images instead of one site writing `[]` and another
`undefined`, so a rebuilt segment and a freshly-created one are
byte-identical.

### Fixed (R72 — Regenerate works end to end, and the transcript folds right)

The server work above left the button inert: `handleRegenerate` only
awaited the POST, so the rerun's frames broadcast to zero subscribers
and nothing on the page changed. Wiring it up surfaced three more
defects, including one that had been mis-rendering every multi-turn
session.

- **`findAssistantTarget` merged answers across a user turn.**
  `session-to-messages.ts` scanned backwards for *any* assistant
  message and never stopped at a `role === 'user'` boundary, so a
  four-row transcript `[user, answer1, user, answer2]` folded to three
  messages with `answer1answer2` in ONE bubble, rendered *above* the
  prompt that produced it. Every multi-turn session on `/sessions/:id`
  was affected; the retained specs never noticed because
  `session-to-messages.spec.ts` has no `UserInput` case at all. The scan
  now stops at a user message (still skipping subagent bubbles).
- **The Regenerate handler rebuilt nothing.** It now snapshots the
  transcript, hides the trailing answer while the rerun runs, waits for
  the rerun to produce an answer, and appends only that. Three gates
  were needed because each alone fires too early: a row beyond the
  baseline (the rerun's *prompt* row lands before the agent is even
  invoked), `status !== 'working'` (true in the gap between the `202`
  and the run actually starting), and an answer at the end of the
  rebuild. Every failure path restores the snapshot, so a failed rerun
  cannot leave a hole where the reply used to be.
- **`regenerate` resolved the controller before reading the log.**
  `get_or_create_session_controller` eagerly creates an unknown
  session, so the miss path — the one case that must have no side
  effects — was the very path that created a session. Reading first
  also lets an absent `events.jsonl` (created-but-never-written) be
  reported as the miss it is instead of a `500`.

New: `getSessionDetail` (`api/chat-stream.ts`) reads one session's
durable transcript.

Scope, stated plainly: the chat page renders the rerun as a
**replacement** (prompt kept once, superseded answer dropped), while
`/sessions/:id` and "Continue chat" render the log as it is —
`prompt, answer1, prompt, answer2`, because regenerate *appends* the
replayed turn rather than replacing it. The two views therefore differ
for any regenerated session. That is a deliberate split for now (the
durable log stays honest and the chat view shows what the user asked
for), but it is a divergence, not agreement, and it is recorded in
`docs/README.md` as a gap. The provider context is unaffected: it sees
`prompt, answer1, prompt` — a normal multi-turn exchange, not the
consecutive duplicate the controller warns about at
`controller.rs`'s `Prompt` arm.

Verified in the running app (Playwright MCP, per `AGENTS.md` §4.2):
against a transcript that gains the replayed prompt first and the
answer later, clicking Regenerate goes from
`1 user / ["OLD ANSWER"]` to `1 user / ["NEW ANSWER"]` — the old answer
replaced rather than stacked, and the prompt not duplicated.

Two more routes lied about what they do, both found in the same audit:

- **`feedback` recorded every rating into a phantom session.** The
  handler resolved a hardcoded `"default"` session id, so every 👍/👎
  from every session was written to a log that does not belong to the
  conversation — and `get_or_create_session_controller` *created* that
  session on the first click. The handler's own comment called this a
  temporary scaffold ending "production deployments wire the full
  path", but this router is production and nothing else wires it.
  `session_id` now rides in the request body (the message id is a
  client-side label and cannot identify a session), is validated like
  any path parameter because it reaches the sink's filesystem path, and
  the frontend sends it.
- **`RunLog` counted rows locally while the sink owned the ordinal.**
  The run's ledger indexed each row by a counter seeded at run start,
  but the controller's op loop appends out-of-band too — the `Feedback`
  row and the shutdown marker go straight to the sink. Every such row
  advanced the sink's counter without the run knowing, so the ledger's
  ordinals drifted, and since those ordinals are what a compaction
  checkpoint's `source_event_seqs` are validated against, a good splice
  could look unresolvable: `try_fold_log_surface` errors and the lenient
  fold silently replays the span the checkpoint had replaced.

  The fix lands in two steps, and the second is the one that matters:
  `RunLog` first read the sink's counter back after each append, which
  closed the systematic drift but not the race (the counter can move
  between the append returning and the read). `SessionSink::append` now
  **returns** the ordinal it assigned under its own lock — it is the
  only party that can report it — and the ledger records that value.
  Surfaced by routing `feedback` to real sessions; the same class of
  "the doc and the behaviour disagreed" defect.

- **`edit_message` never uses `message_id`.** It is only passed to
  `validate_resource_name` and then dropped: the handler builds a fresh
  `SendMessageRequest` from the body and submits `Prompt`/`PromptMulti`,
  appending a NEW turn at the tail. The documented contract —
  "Replaces the user message identified by `message_id`… Past turns
  before `message_id` are preserved as history" — is unimplemented on
  both counts. Recorded as a known gap rather than fixed here:
  implementing replace semantics means truncating the durable surface,
  which is a controller-level design change, and the route has no
  caller (the frontend never invokes it).

### Fixed (R71 — multimodal payloads survive to the model)

Three holes flattened a binary payload into a string placeholder
before the provider saw it:

- **`uva-mcp`: an MCP `tools/call` image/audio reply was dropped.**
  `McpTool::call` did `ToolOutput::text(result.text())`, and `text()`
  renders every non-text block as `[image content]`, so a server's
  screenshot / chart / OCR reply lost its base64 payload.
  `McpCallResult::parts()` now projects each block onto the provider
  vocabulary (`Text` / `Image` / `Audio`, with an unmodelled binary
  kind reporting its mime and size), and `McpTool::call` returns
  `ToolOutput::from_parts(…)` when anything non-text is present.
- **`uva-tool`: no multipart `ToolOutput` constructor.** Added
  `ToolOutput::from_parts(Vec<ContentPart>)` — the multimodal
  counterpart of `ToolOutput::text`.
- **`uva-provider` (Anthropic): a tool-result image was
  Debug-formatted.** `AnthropicToolResultContent` was a text-only
  struct, so `transform_part` emitted `format!("{:?}", …)`. It is now
  a `text | image` tagged enum and `tool_result_blocks` maps each inner
  part (an error result keeps its `Error: {:?}` block). The OpenAI
  adapter already handled this via `extract_media_parts`.

Tests: `uva-mcp` 56 (4 new), `uva-agent --lib` 316 (1 new
end-to-end), `uva-provider --lib` 690 (3 new). `make ci` 6/6.

### Fixed (R70 — R63 server-side wiring completion)

The R63 parallel session added `crates/uva-app-server/src/routes/session_search.rs`
and the matching test file but never wired them into the server: the route
referenced `state.session_search`, a field that did not exist on `AppState`.
Closed the gap:

- `crates/uva-app-server/src/state/app_state.rs` — added the
  `session_search: SharedSessionSearch` field, initialized it from
  `workspace_root.join("sessions")` (the same subdirectory the
  `SessionRegistry` writes into), and added the missing `tool_registry`
  field to `for_test`'s struct literal.
- `crates/uva-app-server/src/routes/mod.rs` — registered `pub mod
  session_search;` and restored the pre-existing `pub mod skills;` and
  `pub mod tool;` declarations that were tracked files but missing from
  the module table.
- `crates/uva-app-server/src/server/router.rs` — mounted
  `GET /api/v1/sessions/search` on the api_routes builder.
- `crates/uva-app-server/src/routes/session_search.rs` — added the
  required `state.session_search.sync().await` call before the search.

Verification: `cargo test -p uva-app-server --test session_search_test`
4/4 pass (was 2/4 before); `cargo run -p uva-session --example
session_search` prints `SESSION-SEARCH: OK`; `make ci` 6/6 green.
Commits: `7f63ba2` (register the search module in
`uva-session::lib`), `2a6185e` (track the example),
`c7e6e7d` (complete the server wiring).

Report: this turn's commit `a3004ae`.

`uva_test_support::FakeProvider::text(text: &str)` — a
one-line constructor that returns a provider answering every
`complete()` call with `Content::text(text)`. Mirrors traitclaw's
`MockProvider::text()`. A consumer test that previously had to
write a full `CompletionResponse { content: Content::text(...),
..Default::default() }` for the common case of one canned text
reply can now write `FakeProvider::text("answer")`.

Verification: `cargo test -p uva-test-support` 17/17;
`make ci` green. The traitclaw `MockProvider::error()` port was
considered and declined — it requires adding a struct field for one
constructor's error path, deferred to the first consumer that
needs provider-error testing.

### Changed (R68 — scheduler-store deterministic tests + drop baseline 9 → 6)

Report: this turn's commit `2242e8a`.

Two `uva_scheduler::store` tests pinned:

- `gc_completed_drops_old_completed_rows` (the cutoff-vs-`last_fired_at`
  relative-time check)
- `sample_job` (the helper used by every store test, so the next-fire
  time is deterministic too)

`make clock-audit` 9 → 6 and `CLOCK_BASELINE` follows. `cargo
test -p uva-scheduler` 14/14. The remaining 6 `Utc::now()`
calls in `scheduler.rs` are intentional wall-clock semantics —
they test 'after N seconds the interval rearms' and 'next_deadline
returns zero for imminent', which pinning would break.

### Changed (R67 — five more deterministic tests + drop clock baseline 14 → 9)

Report: this turn's commit `0dc06d0`.

R65/R66 pattern applied to 5 more test sites that used `Utc::now()`
without a clock-dependent assertion:

- `uva_server::routes::attachment::create_response_flattens_ref_fields`
- `uva_attachment::filesystem_backend_reports_not_found_for_missing_hash`
- 3 sites in `uva_eval::export::tests` (CSV/JSON export helpers)

All pin to `2026-01-01T12:00:00Z`. `make clock-audit` 14 → 9 and
`CLOCK_BASELINE` follows. `make ci` green; 30/30 eval tests,
72/72 server tests pass.

### Changed (R66 — three more deterministic tests + drop clock baseline 17 → 14)

Report: this turn's commit `ff74561`.

Following the R65 pattern, three more test sites that used
`chrono::Utc::now()` without a clock-dependent assertion now pin to
`2026-01-01T12:00:00Z`:

- `uva_session::operation::tests::snapshot_builder_records_compaction`
  records a `last_compaction` timestamp but only asserts
  `is_some()` and `seq == 7`.
- `uva_eval::tests::report_summary_shows_pass_rate_and_average` and
  `report_summary_empty_suite_avoids_division_by_zero` record
  `generated_at` on an `EvalReport` but `summary()` does not include
  the timestamp in its output.

`make clock-audit` count 17 → 14; `Makefile` `CLOCK_BASELINE` 17 → 14
per AGENTS.md §3.8. `make ci` green; `cargo test -p uva-eval` 30/30;
`cargo test -p uva-session` 145/145.

### Changed (R65 — deterministic test + dropped clock baseline)

Report: this turn's commit `7cd50666`.

- **`runtime_context_from_runtime_populates_required_fields`** in
  `crates/uva-agent/src/prompt/mod.rs` no longer reads
  `chrono::Utc::now()` — the test had no clock-dependent assertion
  (every field is checked for *non-empty*), so the real-clock
  dependency was gratuitous noise that risked spurious failures
  across a midnight boundary. Pinned to `2026-01-01T12:00:00Z`,
  the same fixed instant the sibling test
  `runtime_context_from_runtime_uses_injected_clock` uses for its
  clock-injection contract.
- **`Makefile` `CLOCK_BASELINE := 18` → `17`** per
  `AGENTS.md` §3.8 ("lower the baseline whenever the audit
  shrinks"). `make check-clock` still passes (17 ≤ 17); the gate
  continues to fail the moment a production call site appears.

### Changed (R64 — ergonomics-after-parity: five small traitclaw/pi ports)

Report: `docs/optimization-report-R64-2026-09-14.md`.

R62's parity audit closed the big-port loop (R14–R34 absorbed
traitclaw/dsh/pi/pi-subagents). This round is the **ergonomics** round:
five small ports and two documentation fixes that were below the
big-port threshold but real, each answering a question the parity
audit surfaced.

- **`uva-agent::agent::strategy::default_for_test(provider, cancel)`**
  (R1, `b1c265f8`) — traitclaw's `make_runtime` factory adapted to
  where `AgentRuntime` lives in Uva. Three strategy test modules
  (`strategy.rs`, `cot.rs`, `best_of_n.rs`) each built the same
  22-field struct literal inline; the factory collapses 65 lines of
  boilerplate. `pub(crate)` + `#[cfg(test)]` keep it out of the
  public surface.
- **`AGENTS.md` §3.7** documents `uva-core::registry::Registry<T>`
  + `paginate_registry_list` (R2, `f4f08bb3`) — the spine of every
  "named catalog" in the workspace — and why `ToolRegistry` uses a
  private `mod registry_trait` submodule so `ToolFilter` /
  `ToolEntry` don't leak. The pattern was discoverable only via grep.
- **`AGENTS.md` §3.7** adds a 3-question rubric for "should we drop
  this crate?" reviews (R3, `08403509`). All 5 audited optional
  crates (`uva-{scheduler,workflow,rag,skill,mcp}`) answered
  yes to all three (194 tests across them, all green).
- **fmt-fix** the R1 factory call sites (R4, `aee87425`) so
  `make fmt-check` is clean.
- **`FakeProvider::tool_then_text<S, V>(tool_name, tool_input, text)`**
  (R5, `5db19b52`) — ports traitclaw's `MockProvider::tool_then_text`.
  Refactors the `tool_call_then_final_answer` integration test from
  14 lines of `Content::parts` boilerplate to 5 lines of intent.

### Added (R63 — session search: the one gap the parity audit found)

Report: `docs/reference-parity.md` row R63;
the standalone report file (`optimization-report-R63-2026-09-12.md`)
is tracked in a separate working copy.

pi ships `SessionSearchService`: search the agent's *own* history, not the
model's knowledge. R62's parity audit found it missing here, which matters
for a deployment — a user who cannot find last week's conversation does not
trust the store.

- **`uva_session::SessionSearch`** — the seam. Four methods
  (`search_sessions`, `search_entries`, `sync`, `remove`), runtime-neutral,
  so an index can live in memory, in `SQLite`, or behind a service. pi's
  `notify`/`close` have no counterpart on purpose: `sync` is cheap enough
  to call on read, and `Drop` is the close.
- **`JsonlSessionSearch`** — the shipped implementation over the JSONL
  layout this crate already writes. It folds each log through
  `fold_log_surface`, so a query matches *what the model saw* (compaction
  replacements included) rather than raw rows, and it ignores tool
  **results** — indexing machine output would let a query match whatever a
  session happened to `cat`.
- Matching is whole-word and case-insensitive (`tokens` does not answer a
  query for `token`), ranking is term coverage with a length penalty
  (documented in the module), and the index re-reads only logs whose size
  changed. `remove` is a **tombstone**: a forgotten session stays
  unsearchable across a later `sync`, which is what "deleted but not yet
  erased" needs.
- **`GET /api/v1/sessions/search?q=&limit=&session_id=`** — the HTTP face,
  on the v1 `List<T>` envelope, backed by the same index a Rust consumer
  builds. The literal route wins over `/sessions/{id}` (pinned by test).
- **`cargo run -p uva-session --example session_search`** — the
  exported `SESSION-SEARCH: OK` proof, showing the ranking, the snippet
  and the tombstone.

### Fixed (R62 — no runtime type in a library's public API, and the reference map)

Report: `docs/optimization-report-R62-2026-09-12.md`;
current map: `docs/reference-parity.md`.

- **`uva-tool`'s `CleanupTask` held a `tokio::task::JoinHandle`** in a
  public struct, while its own doc claimed "no `tokio` type appears in
  this crate's public API". It now holds a **stop callback**
  (`Box<dyn Fn() + Send + Sync>`, invoked at most once) with
  `CleanupTask::from_stop` as the seam for a consumer running its own
  cleanup loop on another executor, and a manual `Debug`. The
  `start_cleanup_task` constructor is still the tokio-bound plugin — the
  runtime type stops at the crate boundary.
- **`make check-public-api-runtime`** fails the build if a public item in
  any library crate names `tokio`/`async_std`/`smol`/`futures::executor`
  (comments stripped, `uva-app-server` exempt as the application crate).
  It is part of `make ci`, and it has teeth: pointed at R61's source it
  reports the `JoinHandle` line.
- **`docs/reference-parity.md`** — the objective's first axis, made
  checkable: every traitclaw crate and pi package mapped to its uva
  counterpart (verified against the source), the declines with their
  reasons (TUI, bundled coding agent, full MCTS, CBOR, chord/facets,
  builtin runtime seams), and the one verified open gap: pi's session
  `search` service.

### Fixed (R61 — the start-here tutorial is compiled, and it was broken)

Report: `docs/optimization-report-R61-2026-09-12.md`.

`MINIMAL.md` is the document a new consumer reads first. Nothing had ever
compiled it, and when a doctest gate was finally pointed at it:

- **The Step-1 code fence was never closed.** It opened at line 57 and
  the next ```` ``` ```` only appeared at line 135 — so the rendered page
  showed one giant code block containing the "Step 2" heading, its prose,
  and the second fence marker. The guide's two code steps were unusable
  as written.
- `Content::text(self.answer.into())` did not compile (ambiguous
  `Into`), and Step 4's "when you outgrow the prelude" table pointed at
  `uva::session::JsonlSessionSink` — a path that does not exist (it
  is `uva::session::jsonl::JsonlSessionSink`).
- The one path that *was* right exposed a facade wart:
  `SubagentPool` (R31) was only reachable as
  `uva::agent::agent::pool::SubagentPool`; it is now re-exported at
  the agent crate root, so `uva::agent::SubagentPool` is the name.

**The gate**: `crates/uva/src/lib.rs` carries
`#[cfg(doctest)] #[doc = include_str!("../../../MINIMAL.md")]`, which
turns the guide's `rust` fences into doctests. CI already runs
`cargo test -p uva --doc` twice (default features, then the
seven-feature MVP subset), so the guide's central claim — "these seven
features are enough" — is now *compiled* rather than asserted. Step 2's
fence became a function taking the provider, so it reads as the assembly
and compiles without a scripted provider inline.

### Changed (R60 — the two oldest performance wins are enforceable too)

Report: `docs/optimization-report-R60-2026-09-12.md`.

R52 built a ratio gate for the catalog memos, which could be compared
against the expression they replaced because that expression is still in
the tree. R42's ASCII fast path and R48's token-unit accumulator replaced
code that is *gone*, so their claims stayed untested prose in two reports.

- **Faithful copies of the replaced algorithms now live in the benchmark**
  (`slow_estimate_token_count` — the per-character CJK walk;
  `slow_estimate_messages_token_count` — one `String` per message) and
  `--check` measures each against the current code in one process:
  `estimate_token_count` (ASCII) **133×** against a 50× floor, and
  `estimate_messages_token_count` **2.0×** against a 1.3× floor.
- The message pair calls the *current* `estimate_token_count` inside the
  old message-level loop on purpose, so it isolates R48's change instead
  of re-measuring R42's fast path through it.
- Floors are set from measurement: dropping either optimisation lands at
  1.0×, far below the floor, while the observed factors (133×; 1.9–6.3×)
  stay clear of it even on a busy machine — a gate that fails on noise
  gets ignored, which is worse than no gate.

### Performance (R59 — the tool projection is computed once per run, not once per iteration)

Report: `docs/optimization-report-R59-2026-09-12.md`.

A tool-using turn asks the loop for the model-facing tool list once per
iteration, and the answer costs ~21.5 µs at 40 tools (the largest single
item on the per-iteration path, measured since R42). Between iterations
almost nothing about it changes: the registry is version-keyed, the
surface and the agent's restriction are fixed when the loop is built, and
the only moving input is the set of already-called `Deferred` tools.

- **The loop memoises the projection** (`ToolProjectionMemo`, keyed by
  registry version + the sorted promoted-tool set) and returns the
  shared `Arc<Vec<ToolDefinition>>`. Since `CompletionRequest::tools` is
  already an `Arc`, a hit is a refcount bump instead of a deep clone of
  every schema — and instead of recomputing the projection.
- **Interleaved A/B on one machine** (the bench tree at HEAD vs this
  one, two pairs, minutes apart): the scripted one-tool-call turn went
  `360.9 µs → 319.1 µs` and `274.1 µs → 238.2 µs` — one full projection
  saved per turn, ~12 % of the turn.
- **Two tests pin the contract**, asserted through the provider's
  captured `Arc`s (what the wire actually carries): an unchanged tool set
  reuses the same allocation across iterations, and a `Deferred`
  promotion yields a *new* list (the real schema instead of the
  placeholder), so the memo cannot serve a stale projection.
- The R43 short-circuit that skips the transcript scan when no `Deferred`
  tool exists moved out of the projection into `promoted_tool_names`, so
  the path that *keys* the memo pays nothing for it either.

### Added (R58 — the last inert config keys are real, and enforced)

Report: `docs/optimization-report-R58-2026-09-12.md`.

R55 made the inert keys *loud*; this round makes two of them work.
`agents.<name>.allowed_tools` / `denied_tools` were the pair with a
safety edge — an operator who wrote `denied_tools = ["shell"]` had
restricted nothing.

- **`ToolRestriction` is applied at both ends.** The excluded tools are
  not advertised to the model, **and** a call to one is refused before
  dispatch with a model-facing error naming the restriction, surfaced as
  a `Guard` warning event while the run continues — the same shape as a
  steering guard denial. That is what makes the key a control rather
  than a suggestion; the previous state (a visibility-only filter) would
  have let a determined model call the tool anyway, which is why R55
  declined to ship it.
- **`ReActAgent::with_tool_restriction` + `AgentBuilder::tool_restriction`**
  — the library seam, for a consumer scoping one agent without any
  config file. It is distinct from `with_tool_surface`: that is the
  deployment's grouping/cap policy, this is the agent's own scope.
- **The synthetic `task` tool follows the rule**: a restriction that does
  not permit `task` turns delegation off, so it cannot slip past a list
  meant to narrow the agent.
- **`agents.<name>.{allowed_tools,denied_tools}`** resolve once at boot
  (`load_default_tool_restriction`) and ride the same two hops as the
  strategy: `RunDependencies.with_optional_tool_restriction` →
  `AgentRunConfig.tool_restriction` → the run factory and the registered
  agents. Empty lists stay allocation-free (no restriction object, the
  pre-R58 path).
- The R55 audit now reports only the four keys that are still inert
  (`description`, `model`, `hidden`, `color`), and `DEPLOYMENT.md`'s
  table lists five applied keys with the enforcement contract spelled
  out.

### Fixed (R57 — the frontend passes the gates AGENTS.md §4 already required)

Report: `docs/optimization-report-R57-2026-09-12.md`.

`AGENTS.md` §4 asks the web frontend for 0 lint, formatting, a build,
tests, and UI tests. Nothing enforced the first three: CI ran the
Playwright `e2e` workflow (server + secrets + browsers) and a Rust-only
quality workflow, so `tsc`, `eslint`, and `prettier` were never run on a
pull request.

- **Four source files were unformatted** (`src/App.tsx`,
  `src/api/client.ts`, `scripts/e2e/verify_ui.mjs`,
  `tests/e2e/unit/session-to-messages.spec.ts`) — `npx prettier --check .`
  is clean now. `make fmt-web` also formats the *whole* tree instead of
  only `src/**`, so "formatted" means the same thing to the tool and to
  `AGENTS.md`.
- **`.prettierignore` actually ignores lockfiles**: the existing `*.lock`
  pattern never matched `pnpm-lock.yaml`, so every `--check` run reported
  it. `pnpm-lock.yaml` is listed explicitly.
- **`make check-web`** (tsc → eslint → prettier, no browser needed) and a
  **`web` job** in `.github/workflows/rust-quality.yml` that runs it after
  `npm ci`. The Playwright suite stays in its own workflow, where the
  server and secrets already live.
- `npm run build` (tsc + vite) still succeeds, and the two source files'
  diffs are pure line-joining — no behaviour change.

### Docs (R56 — a map for the docs, crate docs for the crates that had none, and a rustdoc gate)

Report: `docs/optimization-report-R56-2026-09-12.md`.

The repo's premise is that its documentation *is* the assembly tutorial.
Two holes in that premise: there was no index over 47 files in `docs/`,
and three crates had **no crate-level documentation at all** — including
`uva-tool` (one of the seven core pieces) and
`uva-test-support` (the crate a consumer adds to test their own
assembly).

- **`docs/README.md`** — the reading order for a new consumer
  (`MINIMAL.md` → `SEAMS.md` → examples → README → `DEPLOYMENT.md`),
  what each directory holds, every optimization round grouped by theme
  with a pointer to its report, and a **known-gaps table** consolidating
  the deferrals that until now lived only in the tail of the 19th-most-
  recent report. Linked from the root README.
- **Crate docs for `uva-tool`, `uva-app-server`,
  `uva-test-support`** — each with what the crate is, what is
  deliberately *not* in it, and a compiling example (the tool one
  implements a `Tool`, registers it, and asserts it reaches the model's
  advertised schema; the server one describes the boot and the route
  surfaces; the test-support one drives `FakeProvider`).
- `uva-tool`'s docs state the crate's load-bearing decision up
  front: **registered**, **shown**, and **runnable** are three different
  questions with three different mechanisms.
- **74 rustdoc warnings fixed, and gated.** `make doc-check`
  (`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`) is part
  of `make ci` now. The warnings were not cosmetic: `uva_core`'s
  error docs described a wire-code method that does not exist,
  `hook_map.rs` told consumers to attach a `HookMap` via a
  `Steering::with_hook_map` that does not exist (and claimed the loop
  consults the map — it reads `Steering::hooks` instead), `router.rs`
  named a `SequentialRouter` twice that was never written, and
  `uva-tool` documented three registry methods under names they no
  longer have. All corrected to the real API; the stale sentences were
  rewritten rather than unlinked.

### Changed (R55 — no config key is silently ignored)

Report: `docs/optimization-report-R55-2026-09-12.md`.

R53 and R54 fixed knobs that were read from the wrong place and knobs
that were read by nobody. One surface was left: `agents.<name>` accepts
six keys the boot parses and never applies — `description`, `model`,
`allowed_tools`, `denied_tools`, `hidden`, `color`. Silence there is a
safety problem, not just untidiness: an operator who writes
`denied_tools = ["shell"]` has restricted nothing and has no way to find
out.

- **A boot audit names them**, once per configured agent:

  ```text
  WARN uva.agent: agent config keys are parsed but not applied; what
  a run may see and call comes from [tools] (groups / hidden / deferred)
  and the registry's privacy flag — see DEPLOYMENT.md
  agent=reviewer keys=["description", "model", "allowed_tools",
  "denied_tools", "hidden", "color"]
  ```

  The classification lives in one testable function
  (`unapplied_agent_keys`), and a config that uses only the three keys
  the boot applies (`max_steps`, `compaction`, `strategy`) produces no
  warning at all — a warning on every healthy deployment is noise that
  gets tuned out.
- **`DEPLOYMENT.md` states what `agents.<name>` applies** (a three-row
  table) and points at the mechanisms that *are* enforced: the `[tools]`
  surface (`hidden` refuses dispatch, not just the advertisement) and,
  for a library consumer, `uva_tool::RestrictedRegistry`
  (`ToolRestriction { allow, deny }`, visibility-only by design).

### Fixed (R54 — the address and the caps the deployment asks for)

Report: `docs/optimization-report-R54-2026-09-12.md`.

`UVA_HOST` and `UVA_PORT` were set by `Dockerfile.server` and
both compose files, documented in `DEPLOYMENT.md` — and read by nothing.
The binary took only `--host`/`--port`, so a container (whose `CMD` is a
bare `uva-app-server`) bound `127.0.0.1:8080`: **a published port that
could not be reached**. `host` / `port` in the deployment config were
read by nobody either, and `max_agents` capped nothing.

- **The address resolves from every source a deployment has**:
  `--host`/`--port`, then `UVA_HOST`/`UVA_PORT` (clap's `env`
  fills the same fields the flags do), then the config's `host`/`port`,
  then the defaults. Each part resolves independently, so
  `--port 7000` with `host = "0.0.0.0"` in the config means exactly
  that. `BindAddress::resolve` is the one place it is decided; the
  server logs the resolved address before binding, and `boot_server`
  returns it so the log, the bind and the announced probe URLs are one
  value.
- **`max_agents` is enforced.** `POST /api/v1/agents` is refused with
  the limit and the knob in the message once the registry holds that
  many — an open deployment can no longer grow its agent set (and every
  session's handoff manifest) without bound. Re-registering an existing
  name stays allowed at the cap, since that replaces rather than adds.
- **The config sections that did nothing are gone.** `rate_limit`
  (`enabled` / `requests_per_minute` / `burst`) and `skills` were parsed
  and never read; an operator enabling limiting was protected by
  nothing. Limiting belongs at the proxy (`DEPLOYMENT.md` says so now);
  skills are discovered from `<workspace>/.agents/skills/`. `skill`
  also drops the vestigial `model_override`. Old files keep working —
  `serde` ignores the keys.
- `BootedServer` + `boot_server` replace `create_server`: the boot
  resolves the deployment config **once**, computes the address, builds
  the state with that config, and returns the router plus the address
  (R53's "one resolution point" extended to the one value that needs it
  before any state exists).

### Fixed (R53 — the config file the operator names is the config file the server reads)

Report: `docs/optimization-report-R53-2026-09-12.md`.

`--config <path>` configured **only the provider bridge**. Every other
section read at boot — `agents` (`max_steps` / `compaction` /
`strategy`), `[tools]`, `auth`, `cors`, `operations`, `retrieval`,
`mcp_servers` — came from `<workspace>/config.toml` regardless, so
`make dev-server` (which passes `--config config.yaml`) silently dropped
half of the file it was handed. Nothing failed; the settings simply did
not apply.

- **One resolution point**: `resolve_server_config(workspace_root,
  config_path)` — the `--config` path when it exists, then
  `config.toml`, then `.uva/config.toml`, format chosen by
  extension. `AppState::new` resolves it once and every section reads
  that value, so the named file is the deployment's configuration. A
  candidate that is missing or does not parse is logged and skipped
  (the next is tried, then defaults), matching the provider bridge's
  existing fail-soft contract.
- **The repeated parse is gone**: ten helpers each re-read and
  re-parsed the same file at boot; they now take the resolved
  `ServerConfig`, and the trivial ones (`auth`, `cors`, `operations`,
  `default_agent`, `tools`) were deleted in favour of reading the field
  at the call site.
- **`--config` is documented for what it does** (every boot section,
  three formats), and the fallback chain is spelled out.
- Proven end-to-end: booting the real server with
  `--directory <ws> --config <tmp>/cfg.yaml` logs
  `loaded server config path=.../cfg.yaml` and
  `configured reasoning strategy agent=agent strategy="best-of-n"` from
  a YAML `agents.agent.strategy` — a setting that could not reach the
  run factory before this round. Two tests pin it: the named file wins
  over a conflicting `config.toml`, and a broken candidate falls
  through instead of ending the boot.

### Changed (R52 — the performance claims are enforced, not just measured)

Report: `docs/optimization-report-R52-2026-09-12.md`.

R42–R51 made the framework's own overhead measurable and then smaller;
nothing stopped it from coming back, because the harness only *printed*
numbers and absolute timings do not survive a shared CI runner.

- **`hot_paths -- --check` / `make bench-check`** — the harness gained a
  second mode that enforces the **ratios** instead of printing the table.
  Each invariant pairs a memoised path with the exact expression it
  replaced and measures both **in one process**, so the factor is
  machine-independent: `descriptors_cached` vs `descriptors`,
  `snapshot_cached` vs `snapshot`, and the tier/group wrappers against
  their uncached scans. Measured today: 1570×, 429×, 69×, 3.6× against
  floors of 100×, 100×, 10×, 2×.
- **A fourth CI job (`bench`)** runs `make bench-check`, so a cache that
  stops hitting or a scan that gets reintroduced fails the build with a
  message naming the invariant. The fast `make ci` gate is unchanged —
  the ratio check needs a release build, so it is its own job.
- The floors carry headroom on purpose (the smallest margin, the group
  wrapper, sits at 3.6× against a 2.0× floor): a regression that removes
  a memo lands near 1×, far below any of them, while runner noise cannot
  reach them.

### Performance (R51 — the visible catalog is a memo, not a scan)

Results, measurements and the in-run baselines:
`docs/optimization-report-R51-2026-09-12.md`.

Every consumer that answers "what should the model be told about?"
started from `ToolRegistry::snapshot()` — which clones two `String`s per
tool and sorts, for the *whole* registry, and was then filtered down.
The tier cap was the worst case: a five-tool small-model deployment paid
for forty descriptions to keep five names.

- **`ToolRegistry::snapshot_cached()`** — the catalog behind the same
  version-keyed memo as `descriptors_cached` (R43). Registration,
  removal and `set_hidden` all bump the version, so a stale catalog
  cannot reach the model; an unchanged registry hands back the shared
  `Arc` (measured 3.90 µs → 8.0 ns, 490×). `snapshot()` stays for
  callers that want a fresh owned `Vec`.
- **`AdaptiveRegistry` and `GroupedRegistry` (and
  `RestrictedRegistry::visible_names`) clone only what they keep** —
  the tier cap now clones `max_visible_tools` names instead of every
  tool's name and description, and the group filter clones only the
  advertised names. Same run, same machine: the tier path
  **3.84 µs → 56 ns (68×)**, the grouped path **5.15 µs → 1.49 µs
  (3.5×)**.
- The benchmark harness gained the catalog entries plus an **in-run
  baseline** for each consumer (the pre-memo expression, measured in
  the same process as the memoised one), so these numbers do not depend
  on comparing across machine states.
- Two tests pin the memo's contract: it reuses the allocation until the
  registry changes, and it follows `set_hidden` in both directions.

### Added (R50 — the third paradigm, and the seam's deployment hop)

Results and evidence:
`docs/optimization-report-R50-2026-09-12.md`.

R49 made the loop a part. R50 makes it a *useful* one — a second
shipped paradigm that uses the runtime's concurrency, and a config knob
so the choice is not compiled in.

- **`BestOfNStrategy` + `CandidateScorer` + `LongestAnswer`**
  (`crates/uva-agent/src/agent/best_of_n.rs`) — sample N answers,
  score them, publish the winner. Standard practice (self-consistency,
  green-patch selection, structured extraction) at a known multiple of
  one answer's cost.
  - **Concurrency through the seam, not around it**: candidates are
    fanned out with `AgentRuntime::spawner` — the deployment's executor
    — and collected over `futures` channels, so no runtime type appears
    in the strategy and a consumer on another runtime gets its own
    executor used. `ReActStrategy` and `ChainOfThoughtStrategy` are
    sequential; this one is the reference for a strategy that spawns.
  - **Alternatives stay off-stream**: only the winner is published as
    the answer (`Model` + `ModelDone`), with one `Progress` event per
    candidate and a selection event naming the winner's index and
    score. A chat UI never sees five answers.
  - **The scorer is the caller's**: any `CandidateScorer` (length,
    similarity, a test run, an LLM judge). Ties go to the lowest index,
    so a script produces the same winner twice; empty answers are
    failures, not candidates; a pre-cancelled run issues no requests,
    and cancellation reaches in-flight samples through
    `complete_with_stream`.
- **`strategy::from_name` + `KNOWN_STRATEGY_NAMES`** — resolve a
  strategy from the string a deployment writes, case-insensitively and
  with the obvious aliases (`cot`, `bon`). An unknown name is a typed
  `Error::Validation` naming every accepted value.
- **`AgentRunConfig.strategy` + `RunDependencies::with_optional_strategy`
  + `agents.<name>.strategy`** — the deployment hop: `uva-app-server`
  resolves the config string **once at boot**
  (`AppState::default_strategy`, via `load_default_strategy`), threads
  it through the controller into the run factory, and installs it on
  both the session runs and the registered agents, so the two agree.
  An unknown name is a boot warning plus a ReAct fallback — a typo in
  one agent's table must not stop the server from starting.
- **`uva_provider::completion_to_sampling`** is now reachable from
  the provider root (it was `traits::completion_to_sampling`), so a
  consumer converting a `CompletionResponse` into an `AgentEvent`'s
  payload does not have to know the module layout.
- **`cargo run --example strategy_swap -p uva-agent` now covers
  three paradigms** and asserts all three from the provider's recorded
  requests: ReAct advertises a tool and runs it once, Chain-of-Thought
  advertises none, best-of-N samples three times with a custom scorer
  and publishes the candidate that carries the required marker.

### Added (R49 — the reasoning loop is a part, not a fact)

Results and evidence:
`docs/optimization-report-R49-2026-09-12.md`.

Every other capability in this framework is a swappable trait —
provider, tools, context manager, steering, sink, cancel token,
executor, retriever, transport. The *loop itself* was not: `ReActAgent`
owned the reasoning and the plumbing in one 2 600-line file, so a
different paradigm meant reimplementing the plumbing.

- **`ReasoningStrategy` + `AgentRuntime` + `EventSink`**
  (`crates/uva-agent/src/agent/strategy.rs`) — the loop behind the
  same kind of seam as everything else. A strategy receives a runtime
  holding every assembled piece (provider, registry, surface policy,
  workspace, descriptor, prompt context, context manager, peer
  registry, steering, max iterations, typed sink, inbox, clock,
  spawner, cancel token, depth) and publishes `AgentEvent`s through a
  cloneable sink. `AgentBuilder::strategy(Arc::new(..))` installs one;
  `ReActStrategy` is the default and the reference implementation.
- **One method, streaming-first.** Reference designs expose `execute()`
  plus an optional `stream()` that defaults to an error, which makes
  streaming second-class. Here the event stream **is** the contract and
  errors travel as `SystemEvent::SessionEnded { reason }`, so every
  strategy streams and `Agent::run` is a three-line adapter.
- **A second shipped paradigm: `ChainOfThoughtStrategy`** — one
  streaming completion, no tools advertised, a step-bounded
  step-by-step instruction, the runtime's context manager honoured, and
  `parse_steps()` to recover the labelled chain from the answer. It is
  the proof that the seam is real: a whole paradigm with no change to
  the agent.
- **`cargo run --example strategy_swap -p uva-agent`** assembles
  both through one `AgentBuilder` and asserts the difference from the
  provider's own recorded requests: ReAct advertises 1 tool and runs it
  once, Chain-of-Thought advertises 0 and runs none. Prints
  `STRATEGY-SWAP: OK`.
- **The clock gate counts code only.** The ratchet was counting the
  literal string inside doc comments (including the docs that forbid
  it), so a new comment could "fail" the gate; comment lines are now
  excluded, the baseline is the 18 in-crate test uses, and
  `make clock-audit` prints only real call sites.

### Performance (R48 — the turn is now 2× R42's baseline)

Results and evidence:
`docs/optimization-report-R48-2026-09-12.md`.

R42's harness measured `estimate_messages_token_count` at 197 µs for a
100-message history and every round since has paid it once per
iteration (the context-window check) plus once per dropped pair. The
cost was one `String` per message: the estimator concatenated every
part of a message before measuring it.

- **`uva_core::token::TokenUnits`** — the heuristic split in two:
  count the units a text contributes (ASCII bytes, CJK chars, no
  allocation), then convert. `estimate_token_count` is now
  `TokenUnits::of(text).tokens()`; a caller measuring several parts as
  one unit accumulates the counts and converts once. Identical by
  construction — the counts are additive and the conversion is the same
  arithmetic — and a test pins accumulation against the concatenation
  for empty / ASCII / CJK / mixed / emoji inputs.
- **`estimate_messages_token_count` accumulates instead of
  concatenating**: **38 759 ns → 16 407 ns** for 100 messages (2.4×),
  and the context manager's no-eviction fast path **118 439 ns →
  71 152 ns** (1.7×), both min-of-5 on the same harness.
- **End to end: the scripted turn is 170 006 ns**, against 352 158 ns
  when R42 first measured it — **2.07× faster**, from four changes
  (linear truncation, the ASCII fast path, the descriptor memo, the
  unit accumulator) that do not alter a single output byte.

### Docs (R47 — the seam map, and two stale recipes it caught)

`SEAMS.md` (new, linked from the README and `MINIMAL.md`) is the lego
map the project's own purpose implies: every trait a consumer can
substitute, what the workspace ships for it, and the line of code that
installs a replacement — the seven loop pieces, the runtime seams
(`Spawner` / `Clock` / `IdGen`), and every opt-in extension
(memory, retrieval, skills, delegation, inbox, MCP, sandbox, workflow,
eval, scheduler, attachments, full-output store). It names the two
seams that are deliberately crate-internal (`CommandRunner`, the JSONL
reader) so a reader knows the difference between "swap this" and "ask
for a feature".

Writing it surfaced two stale recipes:

- **`MINIMAL.md` told consumers to call `AgentBuilder::skill_registry`,
  which does not exist.** Skills reach an agent as a `PromptContext`
  entry plus the registered `skill` tool; the row now says so.
- **The same table said `.run_inbox(Arc::new(RunInbox::new(..)))`** —
  `RunInbox` is the trait, `MpscInbox` is the concrete type and it is
  built with `MpscInbox::channel()`.

### Docs (R46 — the docs catch up with R37–R45)

R37–R45 changed what the framework compiles, requires, and measures;
four documents were still describing the state before them.

- **`crates/uva-core/README.md`** rewritten: the feature list
  stopped at `error` / `registry` / `sensitive` / `text` / `token` and
  omitted `cancel`, `spawn`, `clock`, `idgen`, `schema`, and
  `full_output` — i.e. everything R36–R39 added to the crate a consumer
  adds first. It now also states the crate's one rule (no runtime, no
  provider SDK) and shows the `SharedClock` / `_at`-variant discipline.
- **`crates/uva-agent/README.md`**: cancellation is described as
  the `CancelToken` trait with `AtomicCancelToken` as the default
  (it was documenting `tokio_util::sync::CancellationToken` as *the*
  token), and the runtime contract gets its own section: the
  `Spawner` seam, `AgentBuilder`, and the neutral / tokio-plugin /
  needs-a-reactor table from R39.
- **`DEPLOYMENT.md`**: the test section recommended
  `cargo test --workspace`, the one command AGENTS.md §3.3 forbids and
  the Makefile no longer runs. It now points at `make ci` /
  `make test-crates` / `make test-sqlite` / `make examples` and names
  the CI workflow that runs them.

### Changed (R45 — the wall clock goes through one seam, and a spill file stops colliding)

Results and evidence:
`docs/optimization-report-R45-2026-09-12.md`.

R36 introduced `uva_core::Clock` and R40 wired it into the agent.
An audit for AGENTS.md §3.8 ("new code must not call
`chrono::Utc::now()`") found the rule still had production hold-outs —
and that one of them was a real bug.

- **Clock-routed timestamps**: `ImageAttachmentRef::from_bytes_at` +
  `AttachmentStore::with_clock` (the store, not the byte backend, owns
  `saved_at`), `MemoryEntry::at` + `SqliteMemory::with_clock`,
  `EvalRunner::with_clock` for the report's `generated_at`,
  `OperationSnapshot::new_at`, `JournalEntry::{success_at,failure_at}`,
  `parse_retry_after_at`, and `RunDependencies::with_clock` for the
  server's session-log rows, operation snapshots and hash-only
  attachment responses. `AppState` now builds **one** clock and hands
  it to every timestamped component (including the MCP supervisor's
  `tick`), which is what §3.8 asks for.
- **Fixed: managed spill files collided.** `spill_to_file` named the
  file `{session}-{tool}-{%Y%m%d%H%M%S}.txt`, so two spills inside one
  second shared a name and the second overwrote the first — losing the
  full output the truncation marker still promised. The suffix is now a
  `uva_core::IdGen` ULID (the workspace's id vocabulary, sortable by
  creation, and collision-free).
- **`make check-clock`** (new, and part of `make ci`): a ratchet over
  `chrono::Utc::now()` occurrences in `crates/*/src` — 25 today, all in
  doc comments and `#[cfg(test)]` modules, and the gate fails if the
  count grows. `make clock-audit` prints every occurrence for review.

### Changed (R44 — the server stops re-copying the telemetry crate; the bench stops measuring the scheduler)

Results and evidence:
`docs/optimization-report-R44-2026-09-12.md`.

- **`uva-app-server`'s `middleware/trace_context.rs` now uses
  `uva_telemetry::propagation`** instead of carrying its own copy of
  the W3C machinery (the propagator install, the idempotence flag, the
  extract/inject shims, and two duplicate context structs). What stays
  server-side is exactly the HTTP layer: the `HeaderMap` adapters, the
  middleware, and minting this hop's trace/span id. The telemetry
  module's doc — which claimed the server "does not parse or format
  `traceparent` itself" — is true again, and the 4 middleware tests
  (round-trip, `tracestate` passthrough, generated-header case) pass
  untouched.
- **The hot-path harness reports a minimum over five passes and drives
  the turn on a pooled executor** (`futures::executor::ThreadPool`, not
  tokio) instead of spawning an OS thread per run. Two consecutive
  single-pass runs had differed by ~30% on every entry — including
  untouched ones — so small deltas were unmeasurable; the min-of-5
  numbers are stable (e.g. `descriptors_cached` 9.6 ns vs `descriptors`
  17 519 ns).
- **`project_tool_definitions` was measured and deliberately left
  alone**: 26.6 µs for 40 tools (~10% of the scripted turn) is not worth
  a stateful projection cache plus an invalidation key for the
  transcript-dependent part — 0.01% of a real turn whose model call is
  hundreds of milliseconds. The numbers and the reasoning are in the
  report and in a comment at the call site.

### Performance (R43 — the turn got 33% faster)

Results and evidence:
`docs/optimization-report-R43-2026-09-12.md`.

R42 built the harness and fixed two quadratic paths. Extending it with
an end-to-end number made the per-iteration preparation visible: the
loop rebuilt the model-facing tool list from scratch on every pass,
including a **deep clone of every tool's JSON schema**.

- **`ToolRegistry::descriptors_cached()`** — the descriptor list memoised
  by the registry's existing monotonic version counter, so a caller on a
  hot path holds one `Arc` and pays the deep clone only when the catalog
  changes. The agent loop now uses it: **13 299 ns → 8.5 ns** per
  iteration on a 40-tool registry (1560×), and the memo is proven to
  invalidate on register/unregister (including through a clone).
- **The transcript scan is skipped when nothing needs it.**
  `called_tool_names` feeds `Deferred` promotion only, and it is
  O(messages × parts) — it grows with the conversation. With no
  `Deferred` tool in the catalog the loop no longer scans at all.
- **Measured end to end: `ReActAgent` turn (scripted provider, one tool
  call, 40-tool registry) 352 µs → 236 µs (1.5×)**, of which the
  per-iteration tool-list preparation went from ~45 µs to ~21 µs. The
  bench table now leads with that number.

### Performance (R42 — the first numbers, and two quadratic paths they exposed)

Results and evidence:
`docs/optimization-report-R42-2026-09-12.md`.

The docs made performance claims ("no per-call allocation in the hot
paths", "cheap heuristic") with nothing measuring them. R42 adds a
dependency-free benchmark binary for the work a turn repeats, and it
immediately found two problems worth more than the harness:

- **`crates/uva-agent/benches/hot_paths.rs`** (`make bench`) — nine
  fixtures covering token accounting, stream assembly, wire repair,
  schema validation, tool-surface projection, context truncation and
  text truncation. `harness = false`, no criterion: the number that
  matters is the shape of the curve.
- **`TruncatingContextManager` was quadratic in the history length.**
  It re-estimated the *whole* tail after every dropped pair, so a
  200-message history that did not fit the window burned **34.2 ms per
  `prepare`** — on the path the loop runs before every model call. The
  running total is now maintained by subtracting what a drop removed
  (the estimator is a sum over messages, so the two are equal by
  construction), and the entry condition keeps the whole-list estimate
  it had: **34.2 ms → 0.37 ms (94×)**.
- **`uva_core::token::estimate_token_count` decoded every character
  to ask two questions it can usually skip.** Prompt text is
  overwhelmingly ASCII, where every character is one byte and none is
  CJK — `str::is_ascii` (SIMD) answers both in one pass, and the byte
  length *is* `ascii_count`. Non-ASCII still takes the original
  char-by-char loop: **13.2 µs → 59.8 ns (220×)** on 4 KB of ASCII,
  with byte-identical results (the fast path is the same formula with
  `ascii_count = len`, `cjk_count = 0`).
- The estimator's callers moved with it: `estimate_messages_token_count`
  (100 messages) **197 µs → 29 µs (6.7×)**, and the no-eviction context
  fast path **282 µs → 80 µs (3.5×)**.
- Behaviour is unchanged: all 2585 tests pass, including the two that
  pin the truncating manager's hybrid entry condition (a system prompt
  that alone exceeds the window must still let the tail be evicted).

### Added (R41 — the gates run in CI, from one place)

Results and evidence:
`docs/optimization-report-R41-2026-09-12.md`.

R37–R40 established five invariants (the MVP subset pulls no HTTP
client, `uva --no-default-features` compiles, the runtime-free
crates stay runtime-free, the facade gates its modules, the examples
keep working) — and nothing ran them automatically. CI covered only the
contract-closure scanner and Playwright.

- **`.github/workflows/rust-quality.yml`** — three jobs:
  `gates` (fmt + clippy + both dependency invariants), `tests` (every
  member one crate at a time, the SQLite tier, the facade's
  feature-gating doc tests in the default *and* the MVP configuration),
  and `examples` (every example plus both standalone consumer crates).
  Each step is a `make` target, so a contributor reproduces CI locally
  with the same command and the gate list lives in one place.
- **Makefile**: `fmt-check`, `ci`, `test-crates`, `test-sqlite`,
  `examples`. `CRATES` is derived from the tree (`$(wildcard
  crates/*)`), so a new workspace member needs no Makefile edit; and
  `cargo test --workspace` appears nowhere, per AGENTS.md §3.3.
- **README**: the Test section now points at those targets, and says
  which checks the CI workflow runs.

### Changed (R40 — every crate declares the runtime it actually needs)

Results and evidence:
`docs/optimization-report-R40-2026-09-12.md`.

`[workspace.dependencies]` handed every crate `tokio = { features =
["full"] }`. That meant `uva-session` compiled tokio's `net`,
`process`, `fs`, and `signal` subsystems to move a JSONL file, and the
`Spawner` seam added in R39 sat next to a feature set that still
assumed the runtime owned everything.

- **The workspace entry no longer says `full`.** Each crate declares
  the features its own code uses, with a comment saying why: provider
  `["macros", "time"]` (the `select!` SSE pump and retry backoff),
  session `["rt", "sync", "time", "macros"]`, test-support `["sync"]`
  (its fakes never spawn), tool and agent `["rt", "sync", "time", "fs",
  "process", "io-util", "macros"]`, server the whole application set.
  Test-only users (`context`, `steering`, `skill`, `rag`, `eval`,
  `macros`, `workflow`) moved to dev-dependencies with exactly the
  features their tests need.
- **Two invariants are now enforced by `make`:**
  - `make check-mvp-deps` (extended in R39/R38) — the seven-feature
    subset pulls no HTTP client, exporter, framework, or database, and
    `uva --no-default-features` compiles.
  - `make check-no-runtime` (new) — `uva-core`,
    `uva-scheduler`, `uva-macros`, `uva-eval`,
    `uva-workflow`, and `uva-telemetry`
    (`--no-default-features`) pull **no tokio** in a lib build. These
    are the pieces whose docs promise the consumer the runtime is
    theirs; the target fails the day one of them inherits tokio again.
- **Measured**: the MVP subset's lib build loses `socket2` and tokio's
  `net` / `signal` / `io-std` / `parking_lot` / `rt-multi-thread`
  features. The crate-count effect is deliberately reported as small
  (**124 → 123**): the builtin `shell` tool's `process` feature implies
  `mio` + `signal-hook-registry`, and a consumer riding the default
  builtin set keeps them until the tools themselves get a runtime seam
  (recorded as R41 work in the report).

### Added (R39 — the loop does not choose your runtime)

Results and evidence:
`docs/optimization-report-R39-2026-09-12.md`.

Two rounds cut what the MVP *compiles*; this one cut what the loop
*requires*. `Agent::run` detaches one task per turn (a caller that
stops polling the event stream must not cancel the run) and that
detach was a hard `tokio::spawn` — so the framework's headline promise,
"the executor is the consumer's choice", did not hold for the one place
that needs an executor at all.

- **`uva_core::spawn`** — `Spawner` (plus `BoxFuture` /
  `SharedSpawner`): detached work with no runtime in the signature,
  object-safe, with a default `spawn_blocking`. It compiles with
  **zero** runtime dependencies, and its own test proves it by driving
  a future with `Waker::noop()`.
- **The agent loop uses it**: `ReActAgent::with_spawner` /
  `AgentBuilder::spawner`, defaulting to a tokio-backed spawner (the
  crate already links tokio for its own channels and timers, so the
  default costs nothing).
- **Tool dispatch uses it**: `ToolRegistry::with_spawner` and a
  propagated `Clone`, so `run_stream`'s per-tool tasks go wherever the
  registry was told.
- **Proof, not a promise**:
  `cargo run --example runtime_agnostic -p uva-agent` →
  `RUNTIME-AGNOSTIC: OK`. A full turn — scripted provider, one tool
  call, 11 events — on a ten-line spawner (`std::thread::spawn` +
  `futures::executor::block_on`), with a plain `fn main` and no
  `#[tokio::main]` anywhere.
- **The boundary is documented** rather than implied (agent crate docs,
  README, `uva_core::spawn`): the builtin tools (`tokio::fs` /
  `tokio::process`), the provider HTTP adapters, and the JSONL session
  sink are tokio-bound *plugins*; timers (retry backoff, the streaming
  idle watchdog) still need a reactor.
- **Fixed: `uva --no-default-features` did not compile.** The
  prelude re-exported `crate::core::*` unconditionally while `pub mod
  core` is feature-gated, so the documented "the prelude is empty, not
  broken" claim had never been compiled. Fixed, and
  `make check-mvp-deps` now asserts that configuration alongside the
  dependency check.
- `uva::prelude` gained `Spawner` / `SharedSpawner`.

### Added (R38 — HTTP is a feature, not a default)

Results and evidence:
`docs/optimization-report-R38-2026-09-12.md`.

R37 proved the smallest agent compiles and runs; it still linked the
whole HTTP stack. R38 makes the transport opt-in, so a consumer with
their own provider and no outbound fetch compiles **no HTTP client at
all**: no `reqwest`, `hyper`, `rustls`, `h2`, or `tower`.

- **`uva-provider`'s adapters are features** (`anthropic` /
  `openai`, both on by default). The trait, the wire types, the typed
  profiles, the retry classifier, `json_repair`, and the token counter
  trait compile without either; the adapter-only internals
  (`openai_streaming`, `streaming::anthropic`, `pump_sse`,
  `count_message_tokens`, …) are gated with them.
  `uva-core/reqwest` — the `From<reqwest::Error>` impl — now follows
  the adapter features instead of being unconditional. Naming an
  adapter a build does not have is a **typed runtime error**
  (`adapter_not_compiled` / `no_adapter_compiled`), not a compile error
  and not a misleading "unsupported provider type"; `config.rs` and
  `profile.rs` share one construction site per adapter
  (`openai_provider` / `anthropic_provider`) so the env-key and
  explicit-key paths cannot drift.
- **`uva-tool`'s `web_fetch` is a feature** (`web`, on by default).
  Without it, `build_default_tool_registry` registers `read` / `write` /
  `shell` / `TodoWrite` and neither `WebFetchTool` nor `reqwest` exists;
  the registry and render-kind tests now assert *the compiled* builtin
  set in both configurations.
- **The facade splits transport from vocabulary**: `provider` is the
  `ModelProvider` trait and the wire types, `provider-anthropic` /
  `provider-openai` are the adapters, `tool` is the offline builtins and
  `tool-web` is `web_fetch`. The default set enables all of them, so
  `cargo add uva` is unchanged; the seven-feature subset in
  `MINIMAL.md` now means no HTTP. Three more `compile_fail` doc proofs
  (adapter and `web_fetch` absence) plus three reachability tests pin
  both directions.
- **`make check-mvp-deps` gained the transport names**
  (`reqwest|hyper|rustls|h2|tower|webpki`, prefix-matched) and was
  verified to fail when `tool-web` or `provider-openai` is added to the
  subset.
- **Measured**: the facade's seven-feature tree drops from **158 to 124**
  distinct crates, and `docs/examples/minimal-consumer`'s whole tree
  from **165 to 125**. `prometheus` deliberately stays: it is four net
  crates and it is the tool-instrumentation contract the server's
  `/metrics` route serves.

### Added (R37 — the minimal agent, made provable and lean)

Plan + audit: [`MINIMAL.md`](MINIMAL.md), results and evidence:
`docs/optimization-report-R37-2026-09-12.md`.

This round attacked the gap between the repo's promise ("use it as a
library, from zero") and what a consumer actually had to compile.

- **`ModelProvider::embed` has a default implementation** — one empty
  vector per input text. Embedding was a required method, so every test
  fake, the replay provider, the stub, and the Anthropic adapter each
  carried a three-line no-op or a hardcoded error for a capability most
  providers do not have. `uva-rag` is the only consumer and it goes
  through its own `Embedder` trait, so a provider that never drives RAG
  now writes nothing. **Twelve implementations lost their bodies**:
  seven test fixtures in `uva-agent` (five in
  `agent/re_act/tests.rs`, two in `tests/test_support.rs`), the provider
  stub, the replay provider, the facade assembly example, the server's
  `ToolCapturingProvider`, and the Anthropic adapter's hardcoded error.
  The two fakes whose tests *pinned* the old error/no-op contract were
  re-pointed at the new one (deliberate decisions, not accidental).
- **`docs/examples/minimal-consumer/`** — the smallest complete agent,
  checked in and runnable: **one** dependency (`uva` with
  `default-features = false`), the seven-feature MVP subset from
  `MINIMAL.md`, a scripted provider that requests a tool, a hand-written
  `Tool`, and an assertion that the tool really ran (2 model calls, 1
  tool call, 12 structural events) → prints `MVP-OK`.
  `MINIMAL.md` (new) is the 4-step guide it executes.
- **`make check-mvp-deps`** — fails the build if the MVP feature subset
  ever pulls `opentelemetry`, `opentelemetry-otlp`, `tonic`, `axum`,
  `rusqlite`, or `sqlx`. The gate was verified to fail when `telemetry`
  is added back to the subset.
- **`uva-telemetry` is now two switchable halves instead of an
  all-or-nothing dependency**: console/file logging always compiles;
  `metrics` gates the Prometheus vectors; `otlp` gates the OTel
  exporter, W3C propagation, and every `opentelemetry*` + transport
  dependency. `default = ["metrics", "otlp"]` for direct consumers, while
  in-tree members declare what they use — `uva-tool` takes
  `default-features = false, features = ["metrics"]`, so **an agent
  binary no longer links the OTel SDK, tonic, or the OTLP HTTP
  transport**. `uva-app-server` names `["metrics", "otlp"]` explicitly;
  the facade's `telemetry` feature turns on both. The MVP feature subset
  drops from **182 to 158** unique crates. `tracer.rs` was split:
  console/file tracing moved to `console.rs`, the OTLP half stayed in
  `tracer.rs`, and the init result was renamed `TracerInitResult` →
  `TracingInit` (its `Otlp` variant exists only with the feature).
- **Removed dead weight**: `uva-telemetry` declared `uuid` and
  `tokio` that no line of the crate used; `uva-core` — whose entire
  pitch is runtime neutrality — hard-depended on `reqwest` (for a single
  `From<reqwest::Error>` impl, now the opt-in `reqwest` feature enabled
  only by `uva-provider`) and on `tokio` (now a dev-dependency).
- **Fixed: an OTLP-configured process lost every console log.** The OTLP
  registry composed the level filter and the OTel layer but no `fmt`
  layer, so setting `UVA_OTLP_ENDPOINT` sent spans to the collector
  and nothing to stdout — a boot against an unreachable collector was
  silent. The console layer is now composed alongside them, verified by
  booting the real server with `UVA_OTLP_ENDPOINT` set and reading
  the `OpenTelemetry OTLP tracing initialized … protocol=Http
  sampler=…` line back from its log.

### Added (R34 — one dependency to assemble, a policy for the tool surface, judged selection, durable checkpoints)

Plan: `docs/optimization-report-R34-2026-09-12-plan.md`.
Results + evidence: `docs/optimization-report-R34-2026-09-12.md`.
The round's audit found a durable checkpoint one call short of
working, a tool-surface seam reachable only from code, and no
single-dependency entry point.

- **The `uva` facade crate** (new, 19th member) — one dependency
  instead of nine. Every piece is a feature-gated re-export
  (`uva::agent`, `uva::tool`, …) and `uva::prelude` is a
  27-name curated subset with each collision decided and documented:
  `Result` is `uva_core::Result`, `Tool` is the trait (the derive
  macro stays at `uva::macros::Tool`), `Context` is
  `uva_tool::Context`, and `SessionEndReason` — two genuinely
  different types — is deliberately excluded. Default features assemble
  a basic agent; `skill`/`attachment`/`mcp`/`rag`/`scheduler`/`eval`/
  `workflow`/`telemetry`/`sqlite`/`server`/`test-support` are opt-in.
  The crate docs are the assembly tutorial and 11 `compile_fail` doc
  tests plus a gating integration test pin the feature matrix. The
  crate holds **no logic**, so it cannot drift behaviourally.
- **`ToolSurfacePolicy`** (`uva-tool` + agent + server) — the R33
  tool surface, reachable from a deployment: `groups` with an
  `active_groups` set (a member of an inactive group is not advertised,
  and still executes), a deterministic `max_visible` cap, and deferred
  advertisement. `apply` validates every name before writing anything
  (`SurfacePolicyError` for an unknown tool, an empty group name, a
  duplicate or a twice-claimed tool) and `ToolRegistry::{set_exposure,
  set_hidden, exposure}` mutate a registered entry with a fail-visible
  result, refreshing the registry version so the server's definition
  cache invalidates (that cache was a process-global `static`;
  it is now per-`AppState`). A `[tools]` server config section applies
  the policy at boot, where an unknown tool name is a **warning, not a
  startup failure** — an MCP server that failed to register must not
  take the deployment down — with `AppliedToolSurface::skipped`
  recording what was ignored. `ReActAgent::with_tool_surface` /
  `AgentBuilder::tool_surface` / `AgentRunConfig.tool_surface` apply the
  visible-name filter on top of the R33 projection; with no policy the
  definitions stay byte-identical (the R33 regression test was
  extended).
- **Host-side candidate selection for `best_of`** (`uva-workflow`) —
  `WorkflowHost::select_candidate(&SelectionRequest)` with a default
  body returning `None` (keep R33's first-passing-gate rule), so no host
  breaks and the document format is unchanged. `SelectionRequest` /
  `SelectionCandidate` / `CandidateOutcome` / `GateVerdict` give a
  judge (rubric, LLM, longest answer) what it needs. The choice is
  **recorded** (`JournalEntry.winner`, `CallRun.winner`), so a replay
  never asks the host again; naming a candidate that did not succeed is
  a typed `WorkflowError::Selection` that stops the run instead of
  hiding a wrong judgement behind a plausible winner; and the `Arc<T>`
  host forwarder was extended so an `Arc<MyHost>` cannot silently fall
  back to the default.
- **Durable compaction checkpoints, end to end** (`uva-session`,
  `uva-context`, `uva-agent`, `uva-app-server`) — the emitters
  were never installed, so the log never recorded a compaction and a
  resume replayed (and re-paid for) the pre-compaction history. Now:
  `SurfaceLedger` resolves provenance **through the log** (the manager's
  in-memory indices cannot address the surface, since the loop prepends
  a never-logged system prompt), `CompactionCheckpoint` writes the
  lifecycle triple immediately (a crash mid-compaction stays detectable
  by `orphaned_compactions`) and writes the `Replace` checkpoint only
  when its span is provable — an unprovable record is dropped with a
  warning instead of writing wrong provenance; records that arrive
  before the run's rows land park and are retried by `resolve_pending`.
  `log_surface::{fold_log_surface, try_fold_log_surface}` fold a raw log
  and the production resume projection uses them, so a resumed session
  rebuilds the compacted surface. Clean cutover: the old
  `compaction_emitter_bridge`, `CompactionRecordView::to_typed_event`
  and `TypedEventSink::record_compaction` are **removed** — all three
  wrote a `Replace` with the manager's indices and empty
  `source_event_seqs`, exactly the wrong-provenance write the design
  forbids.
- **Examples** — `assemble_from_zero` (facade/prelude only),
  `tool_surface_policy`, `compaction_checkpoint`; `workflow_best_of` now
  shows a judging host too. The ladder is 28 offline examples.

### Added (R33 — the tool surface, wire hygiene, and workflow selection)

Plan: `docs/optimization-report-R33-2026-09-12-plan.md`.
Results + evidence: `docs/optimization-report-R33-2026-09-12.md`.
Reconnaissance found three mechanisms that existed as vocabulary but
that nothing consumed; R33 wires each of them up.

- **The tool surface, in one place** (`uva-tool` + `uva-agent` +
  `uva-app-server`) — `ToolEntry::with_exposure` / `exposure()` carry a
  `ToolExposure` onto `ToolDescriptor`, and
  `uva_tool::{project_tool_definitions, called_tool_names}` turn
  descriptors + an optional visible-name filter + the set of
  already-called names into the model-facing `Vec<ToolDefinition>`:
  `Direct` sends the full schema, `Deferred` sends name + description
  with a permissive schema until the transcript shows the tool was
  called (then promotes it), `Hidden`/`is_hidden` sends nothing.
  Promotion comes from the transcript, so it is replay-stable and holds
  no mutable state. Both definition builders (the agent loop's
  `tool_definitions` and the server's operator listing) now use the one
  projection, which is also how `AdaptiveRegistry`'s tier cap and
  `GroupedRegistry`'s groups finally get a consumer — both were
  schema-less projections with no caller. A registry with no
  `Deferred`/`Hidden` tools produces byte-identical definitions to
  before (pinned by test).
- **Wire hygiene** (`uva-provider`) — `json_repair::{repair_json,
  parse_tool_input_reported, parse_tool_input_logged, ToolArgsQuality}`
  salvages tool-call arguments that are not valid JSON (raw control
  characters inside strings, invalid escapes) and **reports** whether
  the value came from a strict parse, a repair, or the raw fallback, so
  a production log shows a salvaged call. This deletes a duplicate
  private parser in `assembler.rs` whose fallback disagreed with the
  shared one (`Value::Null` vs `Value::String`), so identical input no
  longer yields different `ToolUse.input` depending on the path, and
  every finalization site routes through the single shared function.
  Refusing a *truncated* argument stays the length-stop guard's job —
  a half-written argument is reported, never silently completed.
- **Structured provider error signals** (`uva-provider`) —
  `error_body::{ProviderErrorBody, parse_provider_error_body}` parse the
  OpenAI-shaped and Anthropic-shaped error bodies (plus a bounded
  excerpt for anything else), and
  `RetryClass::{ContextOverflow, Auth}` + `classify_provider_error_body`
  make quota exhaustion, context overflow, capacity overload and auth
  failures distinguishable from the body alone. HTTP 429 with
  `insufficient_quota` is `Quota` (one attempt: a dead balance never
  recovers in-session) while a plain 429 stays `RateLimit`; text-only
  bodies keep their previous class. Both adapters' 429 paths now read
  the body they were previously discarding.
- **`best_of` workflow selection** (`uva-workflow`) — `Step::BestOf`
  / `BestOfStep` (wire kind `best_of`) run one call per candidate under
  the existing caps and concurrency bound and keep the first candidate
  whose gate passes; `CallStatus::Superseded` records the losers without
  ever dragging a passing run down, and `WorkflowRun::winner_of(step_id)`
  names the winner. An all-failed selection fails the step while still
  handing the caller every failure text, and journal prefix replay
  reproduces a `best_of` run without re-spawning. This is traitclaw's
  parallel-branch strategy expressed declaratively: no VM, no scoring
  closure — the selector is the gate the workflow already runs.
- **Examples** — `deferred_tools` (tool surface), `tool_arg_repair`
  (wire hygiene), `workflow_best_of` (selection). The ladder is now 25
  offline examples, all indexed in [`docs/examples/README.md`](docs/examples/README.md).

### Added (R32 — tool groups, SQLite memory, output transformers, teams + rule routing, example ladder)

Plan: `docs/optimization-report-R32-2026-09-12-plan.md`.
Results + evidence: `docs/optimization-report-R32-2026-09-12.md`.
R32 also completes R31's Wave 2 (its plan's item P) and repairs
everything R31 left broken — see the R31 report.

- **`uva_tool::GroupedRegistry`** (traitclaw `GroupedRegistry`
  parity) — named tool groups with independent activation. The
  read-side wrapper offers only the tools of **active** groups (plus
  tools no group claims) while dispatch is untouched, so a tool whose
  group is deactivated **stays executable**: visibility is a
  prompt-budget decision, not a security boundary. Declarations are
  validated (`GroupError` for an empty name, an unknown tool, a
  duplicate, or a tool claimed by two groups) and one tool belongs to
  one group, so visibility never depends on declaration order.
  `ToolRegistry::contains` is the new wiring-side lookup the
  validation needs (hidden tools included, same key dispatch uses).
- **`uva_context::memory::sqlite::SqliteMemory`** (feature
  `sqlite`, traitclaw-memory-sqlite parity) — single-file durable
  memory: `FTS5`/BM25 long-term recall (queries are sanitised into
  quoted terms, so `NEAR(` and `*` are text rather than syntax
  errors, and a term-less query falls back to newest-first), a durable
  working-memory table, a session registry, read-only opens that
  refuse every write, and typed errors that keep `SQLite`'s own
  message. The conversation tier deliberately delegates to the
  sink-backed `Memory` (documented divergence: uva's durable event
  log is already the conversation source of truth). New optional
  dependency: `rusqlite` with `bundled` (no system `libsqlite3`,
  `FTS5` compiled in); the default build is unchanged.
- **Output transformers + full-output retrieval** (traitclaw
  transformers parity) — `uva_core::{FullOutputStore,
  InMemoryFullOutputStore}` (bounded by entries **and** bytes, true
  LRU where a `get` refreshes recency, handles never reused),
  `uva_tool::__get_full_output` + `full_output_tool` (the
  model-facing retrieval path, `ToolOutput::error` for an unknown
  handle), and `uva_steering::{TransformerChain, JsonExtractor,
  BudgetAwareTruncator}` (chain order matters: extract, then budget).
  A large tool result is now *stashed and cited* instead of dropped.
- **`uva_agent::agent::team`** (traitclaw-team parity) —
  `BoundAgent` (a name and one `prompt -> text` call: no session,
  provider or runtime), `FnAgent`, `Verifier`/`Verdict`,
  `VerificationChain`/`ChainOutcome` (generate → verify → retry, with
  the rejection reason appended to the next prompt; a rejection is
  data, only a transport failure is an `Err`), and
  `RoundRobinGroupChat` with `TerminationCondition`,
  `MaxRoundsTermination`, `MarkerTermination`, per-turn attribution
  and an explicit `StopReason`.
- **`uva_agent::agent::route_rules::ConditionalRouter`
  (traitclaw conditional-router parity) — ordered regex rules,
  first match wins, default fallback, `RoutePatternError` for a bad
  pattern (never a panic). It implements the existing `Router` trait,
  so rules and `@agent:` mentions are two strategies behind one seam.
- **The progressive example ladder** — 22 offline examples plus
  [`docs/examples/README.md`](docs/examples/README.md) as the index.
  New: `tool_groups`, `sandbox_policy`, `sqlite_memory`,
  `output_transformers`, `agent_teams`, `workflow_fanout`,
  `derive_tool`, `eval_suite`, `replay_provider`,
  `run_inbox_steering`, `runtime_context`, `token_meter`,
  `delegation_gate`, `worktree_isolation`.
- **Fixed — tests no longer need ambient credentials.** Four lib tests
  in `uva-app-server` (`routes::health` ×3, `server::tests`) and eight
  integration tests (`boot_wiring_test`, `operation_endpoint_test`)
  booted the production path and therefore required
  `ANTHROPIC_API_KEY`/`OPENAI_API_KEY` to be exported; they now use a
  hermetic state or a temp workspace whose provider reads a key the
  test itself sets (`crates/uva-app-server/tests/support/mod.rs`).
  The `assemble_with_provider_profile` example selected its default
  profile by insertion order (Anthropic) and so needed a credential to
  run despite documenting that it does not; it now selects the keyless
  stub explicitly.
- **Fixed — R31 code that had never faced a linter:** the workflow
  step bound (`Option::map`), two collapsible `if`s (workflow +
  grouped registry), `PathBuf`→`Path` in the sandbox smoke test, and
  `?` in `SubagentPool::release`.

### Added (R31 — file-backed memory, sandbox policy, subagent pool, workflow runtime)

Four reference designs landed, plus the two Wave-2 pieces R31's plan
named but did not ship (they are in R32 below). Results and the
repair record:
`docs/optimization-report-R31-2026-09-12.md`.

- **`uva-context::memory::file`** (pi-subagents §memory) —
  `FileMemory`: scoped long-term memory (`local` / `project` / `user`
  directories), `MEMORY.md` index bounded to 200 lines plus one
  frontmatter file per ULID entry, deterministic keyword recall
  (name 3× / description 2× / body 1×, newest-id tie-break),
  read-only mode, and path defenses (agent-name whitelist, symlink and
  traversal refusal).
- **`uva-tool::sandbox`** (dsh §sandbox) — `ExecutionPolicy`
  (`ReadOnly` / `WorkspaceWrite` / `DangerFullAccess`),
  `SandboxBackend::confine(policy, root) -> ConfinedCommand` with
  confinement as **argv construction** rather than a wrapper process,
  `BwrapBackend` (read-only bind of `/`, workspace bind, private
  `/tmp`, `--unshare-pid`, `--unshare-net` under `ReadOnly`,
  `--die-with-parent`), one-shot availability probing that is
  **fail-closed** (a confined policy with no usable backend refuses to
  run instead of running unconfined), denial dialect classification,
  and the `effective_policy` fold (`grant > session event >
  deployment default`). The `shell` tool consumes it end to end and
  renders `[sandbox: file access denied under <mode> mode]`.
- **`uva-agent::agent::pool`** (pi-subagents §agent-manager) —
  `SubagentPool`: two independent lanes (bounded background with a
  FIFO queue, unbounded foreground), nested children uncharged so a
  parent awaiting a child can never deadlock behind a slot it holds,
  100 tombstones addressable by id, sticky process-wide pool
  discriminators so a stale ticket frees nothing, and
  `InvocationRecord` capturing requested-vs-effective model/tier plus
  usage and status.
- **`uva-workflow`** (new crate, pi-subagents §workflow) —
  `WorkflowSpec` (`agent` / `fan_out` / `pipeline` steps) executed by
  `WorkflowRuntime` through a single `WorkflowHost` effect seam
  (`spawn_agent` / `run_gate`), so caps, concurrency and abort are
  enforceable and tests inject a fake host; `WorkflowCaps` (agents
  1000 / items 4096 / nested 256), `WorkflowControl`
  (pause/resume/skip/retry/abort), and `WorkflowJournal` (JSONL keyed
  by sha256 of a call's identity fields, torn-tail tolerant, replaying
  the longest matching prefix and re-running the tail).
- **Fixed (R32-opening repair):** R31 landed with a NUL-corrupted
  `tests/sandbox_smoke.rs`, an unformatted workspace, a
  `uva-workflow` crate that never compiled, a `Waker` moved out of
  a shared reference, a `ModelTier` moved out of `&self` in
  `tier_substituted()`, four unused `delegation.rs` imports, and three
  tests asserting behaviour the code never had. All repaired; the
  sandbox smoke test is now a real six-test suite that drives the
  shell tool's spawn path, including a genuine bubblewrap denial.

### Added (R29 (cont.) — the remaining seven phases landed)

Phases A–F of the R29 plan shipped in `8f1821cf`; these are
G–M, the rest of the same plan. Plan + the binding contract
(including every deviation) live in
`docs/optimization-report-R29-2026-09-11-plan.md`
and
`docs/optimization-report-R29-remaining-contract.md`.

- **`uva-scheduler`** (new crate, pi-subagents §3) — a
  runtime-neutral cron/interval/once dispatcher. Three modules:
  `Job` (typed `JobKind`, `JobStatus`, ULID ids),
  `Scheduler` (`tick(now) -> ScheduleTick` — the caller owns the
  timer, so the crate works on any executor), and
  `ScheduleStore` (one JSON file per job under
  `<root>/schedules/`, PID-tagged lock files with stale-holder
  reaping, `temp + rename` atomic writes). 14 tests cover due /
  paused / once / interval advance, lock release, reload
  round-trip, and the `gc_completed` sweep.
- **`uva_agent::MentionClone` + `LeaderRouter::mention_clone_mode`**
  (pi-subagents §2) — route a `@agent: …` mention through a
  throwaway clone instead of the leader's own `task` tool call,
  so the dispatch leaves no reasoning or tool_use block in the
  leader's transcript. The clone runs at `parent_depth + 1` so
  its events ride the standard sub-agent notification path, and
  its tool registry is deliberately empty (a test pins that no
  builtin leaks into an invisible turn). `mention_clone_mode`
  defaults to `false` — existing callers keep the `task` path.
- **`SessionEvent::LifecycleShutdown` + `SessionController::close`**
  (pi-subagents §I) — closing a session now appends a terminal
  `lifecycle_shutdown` event to the durable log and closes the
  sink, after every child has been signalled. Both the loop's
  join and `close()`'s wait are bounded by **3 s**, so a hung run
  cannot keep the process alive. `close()` is idempotent. Test
  drives a live blocking run through `close()` twice and asserts
  exactly one shutdown event on disk.
- **Compaction lifecycle events** (dsh §12) —
  `SessionEvent::{CompactionStart, CompactionSummary,
  CompactionEnd}` plus `SurfaceToken` / `CompactionOutcome`.
  `SummarizingContextManager::with_compaction_lifecycle_emitter`
  fires `Start → Summary → End(Committed|Failed)` around the
  summariser call, so a crash mid-compaction is detectable as an
  unmatched `Start` rather than an ambiguous gap.
  `uva_session::compaction_outcomes` is the loader-side view:
  matched lifecycles report their outcome, orphans report
  `Interrupted`.
- **`assistant_chunk` raw-delta preservation** (dsh §20) — every
  streamed provider delta is now written to the typed event log
  as `SessionEvent::AssistantChunk { chunk_seq, finish_reason,
  data.delta }`, alongside the assembled assistant turn.
  `uva_session::assemble_chunks` folds a chunk run back into
  the exact streamed text. Off unless a typed sink is wired, so
  the in-memory test path pays nothing.
- **`ContextManager::set_compaction_details` + `CompactionDetails`**
  (pi §13) — the ReAct loop records the files a run read
  (`read`) or modified (`write`) and hands them to the context
  manager before the next `prepare`, so a compaction's summariser
  prompt can name what touched disk. Only the deterministic
  builtins participate — shell-command file inference is
  deliberately omitted as guesswork.
- **`OperationRequest` wire union + `POST
  /api/v1/chat/sessions/{id}/operation`** (pi §5) — the
  discriminated operation endpoint (`prompt | skill |
  prompt_template | compaction | navigation`), opt-in via
  `[operations] enabled = true`. `prompt` dispatches to the same
  path `/messages` uses (shared `submit_session_op` helper, so
  there is one validation + controller-resolution path); the
  declared-but-unbuilt kinds answer `501 not_implemented` with
  the standard envelope. When the flag is off the route is not
  registered at all — the path falls through to the normal 404.

### Changed

- `test-support` moved into `crates/` and renamed to
  **`uva-test-support`** — the workspace now keeps every crate
  under `crates/` with the uniform `uva-` prefix (dir name =
  package name, matching all 14 siblings). Consumers
  (`uva-agent`, `uva-app-server` `test-utils` feature +
  dev-deps, `uva-tool` dev-deps) follow the rename; no public
  API changed — only the crate identity and paths.

### Internal / Verified (R29 (cont.))

- Per-crate lib tests: core 84, telemetry 26, provider 633,
  context 69, tool 224, skill 56, session 99, steering 57,
  agent 213, **scheduler 14**, attachment 15, mcp 6, rag 38,
  server 387, test-support 1 → **1922 passing / 0 failing**
  (R29 phases A–F baseline: 1877; +45).
- `uva-app-server` integration suites: 387 lib + 4
  (`operation_endpoint_test`) + 2 + 2 + 10 + 4 + 5 + 4 doctests,
  all green.
- `cargo +nightly fmt --all` exit 0; `cargo clippy --workspace
  --all-targets --all-features --tests -- -D warnings` exit 0.
- All 8 examples exit 0 (6 under `uva-agent`, 1 under
  `uva-mcp`, 1 under `uva-rag`); `docs/examples/external-consumer`
  prints `CONSUMER-PROOF: OK`.
- Live boot smoke on a real binary: retrieval index built
  (`documents=1`), skills seeded, `/livez` · `/readyz` ·
  `/api/v1/agents` · `/api/v1/tools` all 200, and the gated
  operation endpoint answered `200` (prompt queued), `501`
  (compaction), `422` (unknown kind). The queued prompt reached
  the agent loop and made a real provider call (rate-limited by
  the configured key — the pipeline, not the provider, is what
  this smoke proves).
- Test-robustness fix (post-G–M): the
  `file_provider_uses_discovery_under_the_hood` test read the
  developer's real `$HOME` skill set and flaked ~1-in-4 runs
  under parallel load (25 found vs 1 expected). The
  `HOME_LOCK` + `ScopedHome` + `with_scoped_home` trio moved
  from `discovery.rs`'s private test module into a shared
  `#[cfg(test)] test_util` module, and the provider test now
  runs inside the scope (driving the never-yielding
  `collect()` via `futures::executor::block_on`). Verified by
  30 consecutive full-suite runs with zero flakes.

### Deliberate divergences (R29 (cont.))

| Area | Plan / reference | uva | Why |
|---|---|---|---|
| MentionClone tool set | one synthetic `agent` tool | empty registry | The peer-dispatch seam that would back it does not exist yet; emptiness is the honest state and the test pins that no builtin leaks in |
| Compaction lifecycle → log | emitter bridge | loader-side `compaction_outcomes` view | The bridge would be unused plumbing until the loop installs it; a real consumer view is testable and useful today |
| File-activity tracking | read / write / shell-with-mutation | read / write only | Inferring a shell command's file effects is a guess, and a wrong claim in the summariser prompt is worse than an absent one |
| Operation endpoint path | `/v1/runs/{agent}/operation` | `/api/v1/chat/sessions/{id}/operation` | Matches uva's actual route tree; the planned prefix does not exist |
| Operation endpoint config | bare `enable_operation_endpoint: bool` | `[operations] enabled` | Every other subsystem uses a named config table (`cors`, `rate_limit`, `retrieval`); one convention, not two |

### Added (R29 — four-reference absorption: traitclaw + dsh + pi + pi-subagents)

- **`uva_tool::ToolRestriction` + `RestrictedRegistry`** (dsh `ToolRestriction` parity, §10) — per-scope allow / deny filter for tool visibility. Two restrictions compose by **intersection** (the dsh "intersect-and-shadow" rule): every ancestor scope contributes a filter, and a tool is visible only if every ancestor permits it. Own registrations stay exempt so a delegated child keeps the tools it answers through. Wire format: `{ "allow": ["read", "write"], "deny": ["shell"] }`; default `{}` means pass-through. `RestrictedRegistry` is a cheap-to-clone wrapper exposing `is_visible` and `visible_names` for the agent loop. 12 tests cover empty / deny / allow / own / intersect.
- **`uva_tool::ToolOutputDefinition::presentation_meta` + `finalize_content`** (dsh `ToolDefinition` parity, §8) — completes the R17 output contract. `presentation_meta(args, value)` is a pure projection for UI cards that lets a tool serve the LLM and a UI panel without leaking UI vocabulary into model-visible text. `finalize_content(output)` is the last-mile transform on every outcome (including pipeline failures) so tools can override the failure-path shape without touching the success path. 4 new tests pin the contract.
- **`uva_provider::credential::normalize_api_key`** (dsh `INVALID_CREDENTIAL` parity, §6) — pre-send credential classifier. Trims ASCII whitespace, rejects empty / illegal-character / unknown-source keys BEFORE they reach the transport. `WorkspaceConfig::resolve_api_key` and `ProviderProfile::resolve_api_key` now run the classifier at resolve time and return `Error::config` with a stable message; the server boundary maps these to `INVALID_CREDENTIAL` (deliberately NOT in the default retryable set). 9 unit tests.
- **`CompletionRequest::replay_state` + `CompletionResponse::replay_state`** (dsh `assistantProvenance.replayState` parity, §7) — opaque provider-native state the adapter can mint on response and the runtime can pass back on the next request (Anthropic prompt cache id, OpenAI response id, …). Threaded through `re_act.rs::step_2_sample_once`; the adapter validates and either consumes or drops. Forward-compat: `#[serde(default)]` on both fields. 3 round-trip tests.
- **`uva_tool::ToolSchemaBuilder` + `ToolFeatures`** (pi-subagents `invocation-config.ts:isolationParam` parity, §7) — small builder wrapping a JSON Schema with feature-gated field removal. `with_feature("worktree_isolation", &features, "isolation")` drops the property AND its `required` entry when the feature is off, so the LLM never sees a description for a refused capability. 9 tests cover empty / chain / merge / property add / drop / feature off / feature on / features default / features chain.
- **`AgentMeta::requested_model` / `requested_thinking` / `effective_model` / `effective_thinking` / `overrides_applied`** (pi-subagents `invocation-config.ts:overridden` parity, §6) — when caller-supplied model / thinking is outranked by descriptor / frontmatter, the `AgentEvent::Agent` envelope carries both pairs plus an `overrides_applied: bool` so UIs can render "model · thinking: low (asked max)" instead of silently substituting. `AgentMeta::with_overrides(...)` is the builder. 3 round-trip tests.

### Internal / Verified

- R29 runs on the post-R28 baseline; tree clean. Per-crate test totals: core 84, telemetry 26, provider 633, context 64, tool 224, skill 56, session 89, steering 57, agent 206, attachment 15, mcp 6, rag 38, server 379 → **1877 passing / 0 failing**.
- The 36 new R29 tests are: 12 ToolRestriction, 4 ToolOutputDefinition extension, 9 credential, 3 replay_state round-trip, 9 ToolSchemaBuilder, 3 AgentMeta overrides (sums: 40 — the report quote of 36 covers the lib-only test additions; the +4 is the agent_meta tests the per-crate count absorbed).
- `cargo +nightly fmt --all` exit 0; `cargo clippy --workspace --all-targets --all-features --tests -- -D warnings` exit 0.
- All 8 examples (`assemble_from_scratch`, `assemble_with_provider_profile`, `assemble_with_skills`, `assemble_with_tier_steering`, `assemble_with_builder`, `register_remote_tools`, `grounded_agent`, `fan_out_with_group_join`) exit 0; `docs/examples/external-consumer` still prints `CONSUMER-PROOF: OK`.
- `uva-app-server --directory <ws> --port 18080` boots; `/livez` 200, `/readyz` 200, `/api/v1/agents` 200, `/api/v1/tools` 200.

### Added (R30 — correctness seams + production robustness + two DX/quality crates)

Eight designs from the reference repos landed. Plan: `docs/optimization-report-R30-2026-09-12.md`. Verified: fmt clean, clippy `--all-targets -D warnings` 0, **per-crate tests green**, `docs/examples/external-consumer` `CONSUMER-PROOF: OK`.

**Correctness / production** — `uva-provider`: `RetryClass::{EmptyResponse, Quota}` (dsh llm-retry completion — empty completions become typed errors instead of silent success; quota exhaustion is its own class so it never re-cycles against a dead balance); shared `streaming::idle_watchdog` + `pump_sse` (per-read watchdog, defaults to 120s; SSE cancel-drain grace unified); `validation::is_empty_terminal_completion` shared between Anthropic + OpenAI parse seams (4 tests + 4 types/stream_chunk tests + 6 integration tests). `uva-mcp`: `public_tool_name` + `NamingPolicy` (`mcp__<server>__<raw>`, sha-suffixed on lossy normalization — two servers with the same raw name now coexist via deterministic prefixing); `McpSupervisor` (tick(now) clock injection, per-outage reconnect budget, generation-swap re-sync on `tools/list_changed`, fail-soft per-server health surfaced through `AppState::mcp_health`); `StreamableHttpTransport` (reqwest-backed, full wiremock integration tests); `uva-mcp/tests/{mcp_protocol,supervisor,http_transport}.rs` (52 passing). `uva-session`: `token_meter::TokenMeter` pure fold (disjoint usage buckets, provider-usage anchoring, projected next-request tokens = `max(0, anchor + signed surface_delta)`); `SessionEvent::Usage.usage: Option<UsageBuckets>` (serde-default back-compat — old payloads still parse); `OperationSnapshot::usage_buckets + context_pressure` (serde default); `uva-context::CompactionSettings::should_compact_anchored` opt-in (heuristic path unchanged). `uva-agent`: `RunInbox` trait + `MpscInbox` (futures mpsc, runtime-neutral, default empty — byte-identical behavior for inbox-less callers); steering drained at iteration start + between tool commit and next sample + after compaction prep; follow-ups revive a stopping run; `SystemEvent::SteeringInjected` + `SteeringSource::{Steering, FollowUp}` (non-exhaustive match, observable in the event stream); **length-stop guard** — `stop_reason ∈ {length, max_tokens}` with tool calls fails the whole batch via model-facing error `ToolResult`s (no truncated JSON ever runs); `prompt::RuntimeContext` snapshot seam — volatile env facts leave the system prompt, render as an appended user-role snapshot only when the rendered body changed (chrono clock injected; dsh §`renderContextSnapshot` parity); `agent::gate` (verification command on the task tool — exactly-once verdict via injectable `CommandRunner`, killed gates report killed explicitly); `agent::worktree` (git-worktree isolation for delegated children — `uva/agent-<ulid>` branch on dirty exit, removed on clean, fake git runner exercised by 6 tests; the `worktree_isolation` feature name `uva-tool/schema_builder` R29 advertised is now real).

**DX / quality** — new crate `uva-macros`: `#[derive(Tool)]` + helper `#[tool(name, description, mode)]` attribute; generates a full `impl uva_tool::Tool` delegating to inherent `async fn execute(&self, &Context) -> ToolOutput`; schemars-derived `parameters()` (type-driven, per schemars 1.2 guidance — value-driven breaks for enums / `Option::None`); compile-error diagnostics for misuse (non-struct, generics, missing `execute`, missing description, bad attribute, bad mode); self-dev-dep integration suite, 30 tests. New crate `uva-eval`: `EvalSuite` / `TestCase` / `Metric` (sync) / `AsyncMetric` / `EvalRunner` (case passes iff ALL metric scores ≥ threshold; no-metrics fallback = keyword); built-ins `KeywordMetric`, `LlmJudgeMetric` (local `JudgeProvider` seam + clamped score parse), `SchemaValidationMetric` (reuses `uva_core::schema::validate_against_schema`); `EvalReport::summary() + to_json_string() + to_csv_string() + export_json + export_csv` (RFC4180 escaping); 30 tests. `uva-test-support`: `ReplayProvider` implementing `uva_provider::ModelProvider` from chunk scripts or session `events.jsonl`; per-run cursor, `assert_consumed` returning `Result<(), ReplayError>` (cancellation aborts undrained cursor; dsh `assertConsumed` parity); 17 tests.

### Deferred (R30-deferred → R31)

- **SubagentWorkflow scripting + journal prefix-replay** (pi-subagents §1) — vm-isolated scripts + JSONL prefix replay. The scripted-VM part is JS-shaped; porting to Rust means a declarative `WorkflowSpec` + caps + journal + gate orchestration as a new `uva-workflow` crate. Sized ~1500 LOC, deserves a focused round with its own design phase.
- **OS-level sandbox seam + Landlock/bwrap backends** (dsh §sandbox) — argv construction + Landlock ABI ruleset builder + policy fold over `PersistedEvent`. New crate `uva-sandbox` (or an `agent` module); kernel-ABI availability makes WSL2 the gating factor for tests.
- **SQLite durable sink + FTS5** (traitclaw memory-sqlite parity) — `SqliteSessionSink` implementing the `SessionSink` trait (rusqlite, bundled). New dev-dep `rusqlite` adds build cost; tracked separately.
- **GroupedRegistry / dynamic tool groups** (traitclaw registries parity) — operator-chosen focus sets where `get_tools()` returns active groups and `find_tool()` searches ALL groups.
- **File-backed scoped long-term memory** (pi-subagents memory parity) — `MemoryFile` impl of `uva_context::Memory` long-term tier, with MEMORY.md index file + frontmatter entries + symlink defense.
- **Two-pool subagent concurrency manager** (pi-subagents agent-manager parity) — background 10-queued / foreground unbounded / nested no-slot, tombstones, invocation records.
- **Progressive numbered example ladder** (traitclaw `examples/` parity) — 15-25 micro-crates each demonstrating one seam. Currently the README's 8 example commands cover the main surface; a per-concept ladder is a docs/PR pass, not a code change.

### Original R30 deferred (still pending)

- **DeferredHandle / OperationRequest discriminated union** (pi §1 + §5) — provider-neutral cross-gateway polling. Needs a real gateway; we ship the wire shape only in R31.
- **Compaction log-only events + `SurfaceOp.replace` checkpoint** (dsh §12) — the durability seam is identified but not yet wired into `SummarizingContextManager`.
- **`assistant/chunk` raw-event preservation** (dsh §20) — every `StreamChunk` durably logged; doubles log size, enables live-resume.
- **SessionEventMap extensibility** (dsh §1+§2) — Rust has no TypeScript `interface` declaration merging; the `inventory` crate gives us a mechanical substitute but a one-round port is too thin to commit.
- **TUI widgets** (pi coding-agent) — uva is HTTP, not TUI.
- **Cross-extension RPC envelopes** (pi-subagents §5) — uva is a single binary, no event bus; HTTP gateway already carries the equivalent reply envelope.
- **FsAdapter / ShellOutputUpdate** (pi §10) — bash tool already has truncation + sanitization; a `Result<T, FsError>` boundary on the FsAdapter is a follow-up.
- **`shutdownChildSession` lifecycle event** (pi-subagents) — needs the `SessionController` refactor that R26's `GroupJoin` was the seed for.


### Deliberate divergences from the references



| Area | Reference | uva R29 | Why |
|---|---|---|---|
| Tool output `render` | blanket trait impl | default trait method on `Tool` | R17: a blanket impl makes a concrete override a coherence error |
| Credential rejection | throw `INVALID_CREDENTIAL` directly | `Error::config("Invalid API key in <env>: …")` mapped at the server boundary | Uva's `Error` taxonomy owns the variant; the transport layer is the only place that knows the HTTP code |
| `presentation_meta` | UI card JSON | `Option<serde_json::Value>` on the definition | The same value can ride the wire verbatim — no extra wrapper |
| `replay_state` | one field on `assistantProvenance` | one field on `CompletionRequest` + one on `CompletionResponse` | Uva carries the state on the wire type, not on a separate `MessageSource` |

### Changed (R27 — `re_act.rs` split: 5896 → 2116 lines, no behaviour change)

- **The ReAct module is now a three-file unit with a real interface instead of one 5896-line file.** Pure code motion, verified by an unchanged test suite:
  - `agent/re_act.rs` — 2116 lines of production code (`ReActAgent`, `ReActLoop`, helpers) and the module map.
  - `agent/re_act/stream.rs` (402 lines) — **streaming chunk ingestion**: the buffers, the drain logic, the finalizer and their tests. Its interface to the loop is two items, `StreamSink` (typed event emitters) and `ChunkState` (the per-sampling-pass accumulator); everything else is `private`, which is what makes it a deep module: it is the only code that knows a provider's chunk shapes.
  - `agent/re_act/tests.rs` (3352 lines) — the test suite, moved out of the production file via the Rust 2018 child-module form (`mod tests;`), so the module path is unchanged and no test needed editing.
- Extraction was mechanical and compiler-verified: visibility on exactly five methods plus two types, imports resolved from the compiler's own diagnostics, and `cargo test -p uva-agent --lib` unchanged at 203 passing before and after.

### Added (R26 — `GroupJoin`: batched completion notification for background agents)

- **`uva_agent::GroupJoin`** — pi-subagents `group-join.ts` parity, and the last named reference gap. When an orchestrator fans out background subagents, notifying the main agent once per completion produces a burst of interruptions; `GroupJoin` holds those completions and emits **one** consolidated [`Delivery`] per group. Semantics: `Pass` for an ungrouped id (notify individually), `Held` while members are outstanding, an immediate `partial: false` delivery when the last member finishes, and on window expiry a `partial: true` delivery of the finished subset so a single slow straggler cannot stall the batch — the group then survives on a shorter straggler window (30 s / 15 s, pi's defaults), and a member is delivered **at most once** across partials.
- **Runtime-neutral by construction**: a pure state machine with no clock and no timer. The caller passes `now` (`chrono::DateTime<Utc>`) and arms its own timer from `GroupJoin::next_deadline()`, so it works on any executor and its tests are fully deterministic (no fake timers, unlike the reference implementation).
- Example `crates/uva-agent/examples/fan_out_with_group_join.rs` — a 3-member panel delivering one notification, plus the timeout/straggler path; asserts nobody is delivered twice.
- 10 tests covering the state machine: pass-through, single delivery on completion, deadline arming, deadline expiry (silent before, partial after), straggler re-batching on the shorter window, no double delivery, empty-window re-arm, `forget`, empty registration, re-registration replacing a group, and two independent groups. A test caught a real leak on the first run (a retired group left its members' id→group mappings behind, so `is_grouped` stayed `true`); `forget` now drops pending, finished **and** delivered ids.

### Added / Fixed (R25 — production boot E2E)

- **MCP transports carry a `label`** (`StdioConfig::label`) and `StdioTransport` bounds its auto-generated description to 60 chars. Previously `server=stdio: sh -c <script>` dumped an entire multi-line script into every log line and never named the configured server; the server now passes `McpServerConfig::name`, so logs read `server=boot-e2e`.
- **`crates/uva-app-server/tests/boot_wiring_test.rs`** — drives the real `AppState::new` boot path and then the real router, so the R21/R22 wiring is protected end-to-end: config → MCP spawn → tool publication → `GET /api/v1/tools` contains the remote tool, `AppState::retrieval` is populated, builtins survive, and a config-less deployment boots with no remote tools. Unit tests on the individual helpers could not catch a wiring regression (e.g. registering MCP tools after the registry is wrapped in its lock).

### Verified (R25 — real binary, not just tests)

- `uva-app-server --directory <ws> --port 18080` boots and logs `uva.mcp: spawned MCP server server=boot-e2e` → `registered MCP server tools tools=1` → `uva.rag: built retrieval index documents=2` (a `.png` in the indexed tree was correctly skipped).
- Live HTTP: `/livez` 200, `/readyz` 200, `/api/v1/agents` 200, `/api/v1/tools` 200 listing **7 tools** — the 6 builtins plus `e2e_remote_tool` served by the spawned MCP child (confirmed alive as a child of the server process).

### Changed (R24 — error-handling convention audit)

- **The last three hand-rolled error enums now use `thiserror`** (AGENTS.md §3.1: library crates handle errors with `thiserror`), which also deletes their hand-written `Display` boilerplate:
  - `uva_session::SessionError` — messages preserved verbatim (`"session is closed"`, `"{0}"` passthrough for the payload variants), `Display` + `Error` impls deleted.
  - `uva_steering::AdmissionError` — messages preserved verbatim, impls deleted.
  - `uva_session::FoldError` — **had no `Display` and no `Error` impl at all**, so it could not be erased into a boxed error or `?`-propagated. It now has both, with messages naming the offending event/range, pinned by a test.

### Verified (R24 — convention audit on the current tree)

- `SystemTime` appears exactly once (`bound_output.rs` mtime cutoff vs `metadata.modified()`) — the documented-correct use for filesystem timestamps; all wall-clock time is `chrono::DateTime<Utc>`.
- Error choice per crate: every library crate uses `thiserror` (or re-exports `uva_core::Error`, itself `thiserror`); only `uva-app-server` — the binary/app crate — uses `anyhow`. No crate uses both.
- Dependency versions are all `major.minor`; the only three-segment literal is `[workspace.package] version = "0.1.0"`, which is this workspace's own version, not a dependency.
- Public-API `tokio` leaks: **0** across the 12 library crates (visibility-aware audit over public fns, public struct fields, enum variants, type aliases). The 13th crate under `crates/` — `uva-app-server` — is the application crate and legitimately names `tokio` types (e.g. its broadcast `subscribe()`); the neutrality rule is a library guarantee, so it is stated for libraries only.

### Fixed (R23 — runtime-neutrality audit)

- **`uva-tool::start_cleanup_task` no longer returns `tokio::task::JoinHandle<()>`** — the last `tokio` type in any library crate's public signature, found by a visibility-aware audit over the 12 library crates (public fns, public struct fields, enum variants, type aliases). It now returns a runtime-neutral `CleanupTask` with `abort()` (also run on drop) and `detach()` (keep cleaning without the handle). R6 established the principle that lib crates expose only std / `futures` / workspace-own types in public signatures, with `tokio` as an implementation detail; this closes the one remaining violation. The audit now reports **0 leaks**; re-run it before claiming neutrality for a new crate.
- 2 behavioural tests: the cleanup loop really deletes stale managed files; dropping the handle aborts the loop; `detach` keeps it running.

### Verified (R23 — end-to-end audit on the current tree)

- **All 7 examples run green** (`assemble_from_scratch`, `assemble_with_builder`, `assemble_with_skills`, `assemble_with_provider_profile`, `assemble_with_tier_steering`, `register_remote_tools`, `grounded_agent`).
- **External-consumer proof re-run green**: `docs/examples/external-consumer` (independent crate, empty `[workspace]`, relative path deps only) assembles a descriptor + attachment round-trip + typed event sink + a full agent run and prints `CONSUMER-PROOF: OK`.
- `cargo test --workspace` → 1956 passed / 0 failed; fmt 0 diff; clippy 0 errors with `-D warnings`.

### Added / Changed (R22 — config-driven RAG grounding; one generic context-manager seam)

- **Retrieval grounding from config** (R22, completes the "every capability is reachable from deployment config" story alongside R16 compaction and R21 MCP): `ServerConfig.retrieval` (`enabled` / `paths` / `chunk_chars` / `top_k` / `char_budget`). At boot the server walks the configured paths, chunks every text-ish file (`.md`, `.txt`, `.rst`, `.toml`, `.json`, `.yaml`, `.yml`; `.git`/`target`/`node_modules` skipped), and builds a BM25 index — dependency-free, deterministic, **no network call**, so grounding turns on with config alone. Unreadable paths log `warn` and are skipped; an empty index is skipped rather than failing boot.
- **`AgentRunConfig.context_manager`, replacing R16's capability-specific `compaction` field.** The caller composes whatever manager it wants and hands it in; the loop factory just installs it (`None` keeps the loop default). One generic seam beats one field per capability — and it keeps `uva-agent` free of any dependency on the compaction or retrieval crates. The run factory got simpler: it no longer clones a provider handle for compaction.
- **`ground_with_retrieval(retriever, inner)`** — public composition helper with the ordering contract pinned by test: the grounder injects retrieved context and *then* delegates inward, so compaction/truncation still trims to the window. Grounding never escapes the budget. With both capabilities configured the server composes exactly this chain.
- **`RunDependencies::{retrieval, with_retrieval, with_optional_retrieval, compose_context_manager}`** + `AppState::retrieval`.
- 6 tests: retrieval config round-trip, boot index build (nested dirs walked, non-text extensions ignored, retrieved content ranks for its own query), opt-in semantics (no config / disabled / nothing-indexable → no index), grounding-before-inner ordering (spy inner manager observes the injected block), and composed-manager opt-in.

### Added (R21 — config-driven MCP servers at boot)

- **`mcp_servers` in the server config** (R21, completes R19's production story): `ServerConfig.mcp_servers: Vec<McpServerConfig>` (`name` / `command` / `args` / `env` / `enabled`) with `to_stdio_config()` bridging to `uva_mcp::StdioConfig`. At boot `register_configured_mcp_servers` spawns each enabled server, runs the MCP handshake, and publishes its tools into the local registry — so remote tools flow through the same guard pipeline, dispatch, metrics, and `output_definition` seam as builtins, exactly like `/api/v1/agents`-registered or builtin tools. Deployments add MCP capability with config alone.
- **Fail-soft boot**: a server that cannot spawn, handshake, or list is logged at `warn` and skipped; a broken optional integration never blocks startup (pinned by test — the builtin surface survives intact).
- **`AppState::mcp_clients`**: the live clients are retained for the process lifetime, because dropping a `StdioTransport` kills its child process (`kill_on_drop`). Consumers can address servers by name.
- 3 tests: config round-trip (incl. the MCP list), boot registration against a scripted `sh` MCP responder (tools visible in the registry + catalog, disabled servers skipped), and broken-server tolerance.

### Added (R20 — RAG: retrieval-augmented grounding)

- **New crate `uva-rag`** (R20, traitclaw `crates/traitclaw-rag` parity with documented improvements):
  - **`Retriever`** seam + [`Document`]/`ScoredDocument` (score separated from the document — a score belongs to a *(document, query)* pair). Deterministic ranking: descending score, **id tie-break**, so the grounding prompt is reproducible for provider-side prompt caching.
  - **Chunkers** — `FixedSizeChunker` (character-counted so multi-byte text never splits mid-codepoint, optional overlap), `SentenceChunker` (ASCII + CJK terminators), `RecursiveChunker` (paragraph → line → sentence → hard-cut hierarchy). All deterministic.
  - **`KeywordRetriever`** — **real BM25** (`k1 = 1.2`, `b = 0.75`, probabilistic IDF with `+0.5` smoothing, length normalisation) with dependency-free tokenisation (lowercase, punctuation split, per-character CJK). traitclaw's version is `Σ tf/(tf+1)` with no IDF; a rare term must outrank a common one (pinned by test).
  - **`EmbeddingRetriever`** + **`Embedder`** trait, with `ModelProviderEmbedder` adapting any `uva_provider::ModelProvider` (index-time batch embed, query-time single embed, cosine similarity). A retrieval failure logs at `warn` and degrades to "no grounding" — never breaks a turn, never silent.
  - **`HybridRetriever`** with two merge strategies: `CitationStrategy::ReciprocalRankFusion { k }` (**default**, `k = 60` — scale-robust: BM25 is unbounded, cosine is bounded) and `CitationStrategy::Weighted { keyword_weight }` (min-max normalised). Over-fetch factor `3`.
  - **`RagContextManager`** — implements `uva_context::ContextManager`, so grounding composes with compaction / tier steering for free. Retrieves against the newest user turn, renders a delimited `<retrieved_context>` block with numbered citations, **replaces** its own previous injection instead of stacking, skips retrieval for very short queries, and delegates to an inner manager so grounding still fits the window.
- **Example** `crates/uva-rag/examples/grounded_agent.rs` — chunk two sources → BM25 index → ranking → grounding prompt (printed) → RRF hybrid merge. No network, no embedder needed.
- 38 tests: chunker boundaries (incl. CJK + hard-cut fallback), BM25 ranking + IDF + CJK query, embedder contract violations, cosine edge cases, RRF/weighted merge, budget selection, grounding rendering, and the context-manager injection/replacement/trim behaviour.

### Added (R19 — MCP client: remote tools as local lego)

- **New crate `uva-mcp`** (R19, traitclaw `crates/traitclaw-mcp` parity, reshaped for uva's seams): a Model Context Protocol client.
  - **`McpTransport`** — the I/O seam (one JSON-RPC 2.0 request → one result). `StdioTransport` spawns a child process over newline-delimited JSON-RPC with a per-transport pair lock, stderr inherited, `kill_on_drop(true)`, and an id-matching read loop that skips server notifications. `InMemoryTransport` (public, not `#[cfg(test)]`) scripts responses so consumers test their own MCP wiring with **no process spawn**.
  - **`McpClient`** — `initialize` (protocolVersion `2025-06-18`, clientInfo, then the mandatory `notifications/initialized`), `tools_list`, `tools_call`.
  - **`McpTool`** — a remote tool adapted to `uva_tool::Tool` (name/description/`inputSchema` from the spec; `RenderKind::Json` + `mcp_server` presentation hint; `isError` → error output; transport failure → error output).
  - **`register_mcp_tools`** — publish every advertised tool into a `ToolRegistry`, after which they flow through the same guard pipeline, dispatch, metrics, and `output_definition` seam as builtins.
- **Example** `crates/uva-mcp/examples/register_remote_tools.rs` — spawns a POSIX `sh` responder over stdio, handshakes, registers `sum`, and calls it (success + `isError` paths). Runs with no network and no external MCP server.
- 16 tests: 10 protocol/adapter/registry integration (incl. a **real stdio round-trip** against a scripted `sh` child with 4 sequential id-matched calls) + 6 in-crate unit tests.

### Added (R18 — external-consumer proof, checked in)

- **`docs/examples/external-consumer/`** — a standalone crate that is deliberately NOT a workspace member (empty `[workspace]` table) and depends on the uva crates by relative path. It assembles a complete agent in one `AgentBuilder` chain (provider + tier steering + `.output_schema` auto-tool + `.compaction_settings` + typed sink), round-trips an image through the `AttachmentStore`, drives a turn with a std-only `AtomicCancelToken`, and asserts the result. Run: `cd docs/examples/external-consumer && cargo run` → `CONSUMER-PROOF: OK`. This is direct evidence for the goal's "use this repo as a lib from zero" claim: an out-of-tree consumer builds and runs against the published surfaces with no tokio type in any uva call. README gained the "Consume uva from your own crate" recipe.

### Added (R17 — ToolOutputDefinition made real)

- **`Tool::output_definition` is now an overridable trait method** (R17-1, fixes the R10 coherence trap): R10's `HasOutputDefinition` extension trait shipped an `impl<T: Tool + ?Sized> HasOutputDefinition for T` blanket impl, which made any concrete override a **coherence error** — so no tool could ever opt in and the seam was inert by construction. The method now lives on `Tool` itself as a defaulted method (object-safe; returns `ToolOutputDefinition::passthrough(name)`), and the extension trait is deleted.
- **Builtin render contracts** (R17-2, dsh result-view parity): `web_fetch` → `RenderKind::WebFetch` + truncation-marker hint; `read` → `Read` + `line_numbered`; `write` → `Write`; `shell` → `Shell` + `exit_marker`; `TodoWrite` → `Todo`. Every builtin now carries a display title, so the LLM-facing projection and the UI renderer share one contract per tool instead of hard-coding shapes.
- **`ToolRegistry::output_definition(name)`** (R17-3): the consumer seam — returns the tool's contract (hidden/unknown → `None`), letting the agent loop / UI pick a renderer by name. `GET /api/v1/tools/{name}` now includes it in `ToolDetail`.
- **`RenderKind` serialises in `snake_case`** (`"shell"` / `"web_fetch"` / `"todo"` / …), matching the rest of the API surface (`ContentPart`, `OperationState`). Previously PascalCase — fixed at birth (nothing consumed it before R17).

### Added (R16 — server-config compaction surface)

- **`agents.<name>.compaction` server config** (R16, completes the R13/R15 production story): `AgentConfig` gains an optional `compaction` table (`enabled` / `reserve_tokens` / `keep_recent_tokens` / `min_messages_between_compaction`) deserialised straight into `CompactionSettings`. `AppState::default_compaction` is resolved at boot via `load_default_compaction` and threaded through `RunDependencies.with_optional_compaction` → `AgentRunConfig.compaction` → the run factory, which installs the provider-backed `SummarizingContextManager` via the new public `uva_agent::context_manager_for_compaction(provider, settings)`. Deployments can now turn on LLM compaction with config alone. 3 tests (config-table parse with defaults filling the omitted throttle / loader read + unknown-agent + no-name → None / `AgentConfig` 8-field serde contract).
- **Fixed: flaky `mutation_queue::same_path_runs_sequentially`**: the test asserted the spawn order `[1, 2]`, but `tokio::spawn` gives no scheduling guarantee — the queue's real contract is mutual exclusion, not FIFO. Rewritten to observe the maximum number of mutations inside the critical section at once (`MUST be 1`) plus "both ran", which is deterministic under any scheduler interleaving.

### Added (R15 — CompactionSettings drives the SummarizingContextManager trigger)

- **`SummarizingContextManager::with_settings`** (R15-1, closes the R13-3 follow-on): installs a `CompactionSettings` policy that gates the batch pruning on `should_compact` (utilisation threshold + enabled switch + `min_messages_between_compaction` throttle via an interior `AtomicU32` counter reset on every successful splice). `None` (the default) keeps the legacy always-prune behaviour — all 6 pre-existing summarising tests pass unchanged. `AgentBuilder::compaction_settings` now threads the policy into the manager it constructs, so the R13 selection + this trigger form the complete pi-parity compaction story. 3 tests (under-threshold blocks / over-threshold prunes + throttle reset / disabled never prunes).
- **Fixed: `estimate_messages_token_count` dropped tool-result content** (found by the R15-1 gate tests): the public estimator matched only `ContentPart::Text` via `part.text()`, so tool-result text and tool-use JSON counted as **0 tokens** — tool-heavy conversations (exactly what compaction targets) estimated near-zero and could never cross the threshold. Now counts Text + Reasoning + ToolResult inner text + ToolUse name/input JSON through the shared `estimate_token_count` heuristic. Probe before: 4-message tool conversation → `est=0`; after: `est=393`.

 ### Added (R13 — attachment_ref resolve + output_schema injection + compaction wiring)

- **`attachment_ref` resolution at the chat boundary** (R13-1, closes the R12-2 follow-on): `build_parts` now takes `Option<&AttachmentStore>`; a `kind = "attachment_ref"` + `attachment_hash` attachment resolves through the R11 store and inlines the bytes as an `Image` part — the full hash→store→bytes→wire round trip, replay-safe via content addressing. Unknown hashes are skipped with a warn (the text part still ships; the turn isn't lost). Production call sites pass `Some(&state.attachment_store)`. 2 tests (round-trip + skip-on-unknown).
- **`AgentBuilder::output_schema` → auto `StructuredOutputTool` injection** (R13-2, closes the R12-3 loop-wiring deferral without re_act surgery): declaring a schema on the builder auto-registers `structured_output` (LIFO so it wins name lookups) into the tool registry at `build()` time AND threads the schema string onto `AgentDescriptor::output_schema` for the wire surface. The previously-inert descriptor field is now the source of the injection. 2 tests (inject + inert-by-default).
- **`AgentBuilder::compaction_settings` + `resolve_context_manager`** (R13-3, pi `harness/compaction` parity): when the policy is present + enabled + valid, `build()` upgrades the context manager from the truncating default to `SummarizingContextManager` whose summariser forks the agent's own provider (no second model wiring). Invalid settings warn + keep the default; disabled keeps the default. The pure decision core (`resolve_context_manager`) is directly tested across the full table (None/disabled/invalid/valid) including a behavioural prepare smoke on the replaced manager. 1 test.
 ### Added (R12 — AgentBuilder + schema validation + structured output + /status route)

- **`uva_agent::builder::AgentBuilder`** (R12-1, traitclaw `examples/24-agent-factory` parity): fluent factory hiding the orchestrator wiring. `AgentBuilder::new(provider)` + chained `.workspace() / .tool_registry() / .steering() / .context_manager() / .typed_event_sink() / .peer_registry() / .max_iterations() / .name() / .instructions() / .model_hint()` → `.build()` produces a fully wired `ReActAgent`. Defaults: `/tmp` workspace, empty `ToolRegistry`, `Steering::noop()`, `TruncatingContextManager`, descriptor named `"agent"`. `Clone` for cheap branching. Closes the goal's "乐高式从零组装" axis. 4 tests.
- **`uva_core::schema::{validate_against_schema, SchemaViolation}`** (R12-3a): minimal JSON-Schema-subset validator (type / required / properties / items; unknown keywords permissive per spec). Returns **all** violations at once with dotted paths (`"opts.depth"`, `"[2]"`) so the model can fix every field in one retry. 10 tests.
- **`uva_tool::structured_output::{StructuredOutputTool, structured_output_tool, STRUCTURED_OUTPUT_TOOL_NAME}`** (R12-3b, pi-subagents `src/structured-output.ts` parity): synthetic tool whose `parameters()` IS the caller's schema — the provider fills the fields as tool-call arguments, `call()` re-validates via `validate_against_schema`, and either captures the canonical JSON payload or returns an `is_error` result enumerating the violations. The structured-output capture pattern without response_format machinery. 5 tests.
- **`WireAttachment.attachment_hash`** (R12-2): new optional wire field on the chat request — `kind = "attachment_ref"` + `attachment_hash` references a previously-uploaded attachment (from R11's `/api/v1/attachments`) by content hash. `WireAttachment` now derives `Default` + `Serialize` so the hash round-trips through serde. 1 round-trip test. (The agent-runtime resolve-through-store step is the R13 follow-on.)
- **`GET /api/v1/sessions/{id}/status`** (R12-4, closes the R10-14 deferral): returns the latest `OperationSnapshot` from the controller's snapshot bus (typed `state` / `iteration` / `usage` / `last_error`), or 404 when the session is unknown or has never run. Complements the string-label `infer_status` on `GET /sessions/{id}`. 1 bus-latest test pinning the late-subscriber contract the route depends on.
### Added (R11 — multimodal attachments + router/delegation bridge + snapshot emit)

- **New crate `uva-attachment`** (R11, dsh `packages/attachment/attachment/src/index.ts` parity): content-addressed multimodal attachment store. `ImageAttachmentRef { hash, mime_type, byte_len, saved_at }` carries the SHA-256 identity through the LLM context instead of inline base64; `attachment_to_content_part()` renders the ref into `ContentPart::Image` for the provider wire format. `AttachmentStore` fronts a pluggable `AttachmentBackend` (`FilesystemBackend` sharded `root/{hash[0:2]}/{hash}.{ext}` with dedup-on-write, `InMemoryBackend` for tests) plus a bounded hot-read cache (256 entries) with `evict()`; `read_image` re-verifies the hash on every backend fetch and surfaces `AttachmentError::HashMismatch` on corruption. Validators: `SUPPORTED_IMAGE_MIME` (jpeg/png/gif/webp/bmp) + `DEFAULT_MAX_BYTES` (8 MiB). `read_base64_raw(hash)` serves hash-only lookups. 15 tests.
- **`POST/GET/DELETE /api/v1/attachments`** (R11): server surface for the attachment store. `AppState::attachment_store` (filesystem-backed at `<workspace>/.attachments`, in-memory fallback) is shared across handlers; the route validates MIME via a `validator` custom rule, decodes base64, saves through the store, and maps `AttachmentError` onto the unified envelope (`unsupported_mime` 400 / `attachment_too_large` 413 / `attachment_not_found` 404 / corruption 500). 6 route tests.
- **`mentions_to_task_specs`** (R11, closes the R10-4 loop-interception deferral): converts a `RoutingDecision` from `LeaderRouter::route(text)` into `Vec<TaskSpec>`s the existing delegation seam (`delegation::run_subagent`) understands — `@agent: prompt` text mentions become `task` tool calls without the model emitting a tool_use block. Re-exported from `uva_agent`. 2 tests.
- **`SessionController::subscribe_snapshots` + snapshot publishing** (R11, closes the R10-5 controller-emit deferral): the controller now publishes `OperationSnapshot`s on the shared `SnapshotBus` at every run-state transition — `Running` in `maybe_start_run` (with resolved agent name + iteration cap), `Completing` when the factory stream ends, `Cancelled { reason }` in the Cancel op handler. `subscribe_snapshots()` on the public handle returns the receiver plus the latest snapshot for late subscribers. 1 controller test pinning the Running→Completing sequence.
 
### Added (R10 — tier-aware steering + tool render contract + Router + snapshot bus)

- **`uva_provider::ModelTier` + `TierLimits`** (R10-6, traitclaw `types/model_info.rs` parity): 3-bucket coarse model-capacity enum (`Small` ≤32K, `Medium` 32K–100K, `Large` >100K context) with `Override(Box<ModelTier>)` for testing escape hatches. `from_model_config` heuristic + `effective()` flattener. `TierLimits::for_tier` exposes 5 per-tier knobs (`max_visible_tools`, `loop_detection_window`, `tool_budget`, `max_concurrency`, `context_budget_threshold`). `ModelProvider::tier()` returns the cheap sync tier; default derives from `model_config()`. 10 tests.
- **`uva_steering::tier::for_tier` + `tier::auto`** (R10-2, traitclaw `Steering::for_tier()` parity): one-line tier-tuned bundle factory. Per-tier preset scales guard budget (50/100/250), tracker concurrency cap (1/3/8), and context-budget hint threshold (0.50/0.75/0.80) in lock-step. `auto(provider.tier(), workspace_root)` is the recommended one-liner. 3 tests.
- **`uva_tool::AdaptiveRegistry`** (R10-3, traitclaw `AdaptiveRegistry` parity): tier-aware read-side wrapper over `Arc<ToolRegistry>`. Caps the LLM-visible tool list at `max_visible_tools` per tier; hidden tools filtered out by the underlying `snapshot()`. Cheap to clone; `with_tier()` swaps the tier without rebuilding the inner registry. 5 tests.
- **`uva_tool::{ToolOutputDefinition, RenderKind, HasOutputDefinition}`** (R10-1, dsh `tools/src/index.ts` ToolOutputDefinition parity): tool-owned canonical rendering contract. `present_call(args, ctx)` projects the tool call for the LLM-facing surface; `present_result(args, output, ctx)` projects the tool result for both LLM and UI surfaces. `RenderKind` enum (`Text` / `Shell` / `Read` / `Write` / `WebFetch` / `Todo` / `Json`) is a hint for UI renderers. `HasOutputDefinition` blanket impl gives every `Tool` a passthrough definition; builtins opt in by overriding. 8 tests.
- **`uva_agent::agent::router::{Router, LeaderRouter, PassThroughRouter, Mention, RoutingDecision}`** (R10-4, traitclaw `LeaderRouter` parity): text-routed delegation. `LeaderRouter::route(text)` parses `@<agent_name>: <prompt>` mentions and dispatches each to a known peer. Unknown peers stay in the remainder so the leader can rephrase. Custom trigger char via `with_trigger`. 11 tests. (Note: re_act loop interception is R11+; the trait is a usable building block today.)
- **`uva_session::{SnapshotBus, SnapshotReceiver, DEFAULT_BUS_CAPACITY}`** (R10-5, closes the R9-6 deferred wire-up): runtime-neutral subscribe / publish channel for `OperationSnapshot`. `subscribe()` returns a receiver plus the latest snapshot so late subscribers don't have to wait. `publish(snap)` returns the seq number and drops dead subscribers. Bounded by `DEFAULT_BUS_CAPACITY` (64). 4 tests.

 ### Changed (R6 — lego-composition round)

### Added (R6 — lego-composition round)

- **`uva_provider::BlockAssembler`** (R6-1, dsh `assembler.ts` parity): incremental chunk-to-message builder. `push(StreamChunk)` folds streamed text / reasoning / tool-call deltas with straggler-aware dedup; `finalize_assistant()` returns `(SamplingResult, Vec<ContentPart>, incomplete)`. Third-party providers only emit `Stream<Result<StreamChunk, Error>>` — the 200-line fold previously inlined in `ReActLoop::sample_once` is now a single testable unit. 12 unit tests.
- **`uva_session::typed_event_builders`** (R6-2): pure builder functions for every structural `SessionEvent` variant (`step_start/end`, `turn_start/end`, `iteration_start/end`, `subagent_enter/exit`, `request_header`, `usage`) + `round_trip` / `structural_kind` helpers. Every builder's JSON round-trips `SessionEvent::from_value` — the durability contract is pinned per-variant. 11 tests.
- **`uva_session::{TypedEventSink, TypedEventReceiver, TypedEventRecord, CompactionRecordView, compaction_emitter_bridge}`** (R6-3): runtime-neutral mpsc channel (futures, not tokio) carrying typed structural events from the agent loop to the controller; `compaction_emitter_bridge` adapts the R5-8 `SummarizingContextManager` emitter callback into a typed `SessionEvent::Compaction`.
- **`uva_steering::{HookMap, HookEvent, HookDecision, event_name}`** (R6-4, pi `HookMap` parity): name-keyed typed hook registry. `HookEvent` is a closed 8-variant enum; `HookMap::on(name, handler)` chains registrations; `dispatch` short-circuits on the first non-`Allow` decision and treats handler panics as `Allow` (fail-open, never poisons a run). 8 tests.
- **`uva_provider::{BilledClass, TokenUsage::bucket/for_each_bucket/bucket_total}`** (R6-5): 5-bucket billing semantics (Input / CacheRead / CacheWrite / Reasoning / Output — Output subtracts Reasoning). Cost modules fold a `TokenUsage` into per-class prices without hand-rolling a five-arm match. 5 tests.
- **`ContextManager::prepare_arc`** (R6-6): Arc-shared variant of `prepare`. Managers that need no change return the input `Arc` unchanged so `CachePolicyApplier`'s `Arc::ptr_eq` short-circuit actually hits (previously 0 % — every prepare allocated a fresh `Vec`). `Noop` / `Truncating` override the no-op fast path. 4 tests.
- **`uva_session::{KNOWN_SESSION_EVENT_TYPES, empty_event_of}`** (R6-7, dsh `known-event-types.ts` parity): the 16-entry wire-tag vocabulary + a tag → skeleton constructor. Frontend codegen's source of truth. 4 tests.
- **ReActLoop typed-event emission** (R6-A wiring): the loop now emits `request_header` (provider / model / tools_hash), `iteration_start/end`, `step_start/end` (with `StepAction::{ToolCall, FinalAnswer}`) through the typed sink at the natural drive-loop boundaries; `ReActAgent::with_typed_event_sink` / `AgentRunConfig.typed_event_sink` thread the sink from the controller.
- **`SessionController` typed-event persistence** (R6-B wiring): `maybe_start_run` creates the channel pair per run; after the agent stream ends the run task synchronously drains the receiver (`try_recv` loop — no spawned task, no channel-closure await) stamping `ts` and appending to the JSONL sink.

### Added (R7 — runtime neutrality + idiomatic Rust)

- **`uva_core::cancel::{CancelToken, AtomicCancelToken}`**: runtime-neutral cancellation vocabulary (`is_cancelled` / `cancel` / `cancelled()` boxed future — object-safe without async-trait macros). `AtomicCancelToken` is a std-only `AtomicBool` + hand-rolled waker-list future. The tokio-util bridge (`impl CancelToken for tokio_util::sync::CancellationToken`) lives behind `uva-core/tokio-util` so existing tokio callers coerce with zero adapters. 7 tests.
- **Runtime-neutral public APIs**: `Agent::run` and `ModelProvider::complete_with_stream` now take `Arc<dyn CancelToken>`; `steering::Gate::new` / `Gate::cancel_token` use the trait; `TypedEventSink` runs on `futures::channel::mpsc`. Lib consumers can drive uva from any async runtime; tokio remains an implementation detail.
- **Idiomatic-Rust cleanup**: stringly-typed `emit_iteration(kind: &str)` / `emit_step(kind: &str)` replaced by per-variant methods + `StepAction` enum; controller drain expressed as `while let Ok(Some(_))`; futures 0.3.34 `try_next` → `try_recv` rename adopted with `Closed → Ok(None)` mapping.

### Added (R8 — chrono uniformity + runnable assembly tutorial)

- **`examples/assemble_from_scratch.rs`** (`uva-agent`): the executable form of the composition tutorial. Wires the seven lego pieces — provider (`ModelProviderStub`), `build_default_tool_registry`, `Steering::default_policy` + `HookMap`, `TruncatingContextManager`, `TypedEventSink` (futures mpsc), `AtomicCancelToken` (std-only), `ReActAgent` — runs end-to-end with **no network and no API key**, prints the live SSE-style event stream, the typed structural events (`request_header` / `iteration` / `step`), the 16-entry event vocabulary, and a standalone `BlockAssembler` fold. `cargo run --example assemble_from_scratch -p uva-agent`.
- **README "Use as a Library (assemble an agent from scratch)"**: the assembly command, a code sketch of the same wiring, the runtime-neutrality guarantees (executor is the consumer's choice), and pointers to the per-crate docs + design records. The crate table also regained the missing `uva-steering` row.

### Changed (R8)

- **`uva_context::MemoryEntry::created_at` is now `chrono::DateTime<Utc>`** (was raw `u64` epoch seconds via `SystemTime`): chrono is the workspace-wide wall-clock library, so the entry carries a typed, RFC 3339-serialisable timestamp. File-system mtimes (`fs::Metadata::modified`) and monotonic `Instant` measurements deliberately stay on `std::time` — those are the correct std types.


### Verification (R6 + R7)

- `cargo check --workspace --all-features --all-targets`: 0 errors
- `cargo clippy --all-targets --all-features --tests --all -- -D warnings`: 0 warnings
- `cargo +nightly fmt --all -- --check`: 0 diff
- lib tests across 9 crates: **1594 passed, 0 failed** (core 74 · session 75 · context 61 · steering 50 · provider 599 · tool 178 · agent 171 · server 360 · telemetry 26); integration suites green.


- **`uva_context::CompactionSettings`** (R5-1, pi parity): typed policy struct with `enabled` / `reserve_tokens` / `keep_recent_tokens` / `min_messages_between_compaction`. `validate()` returns a typed `ValidationError` (`ReserveTokensZero` / `KeepRecentTokensZero` / `Overflow`); `sane_for_window()` surfaces a non-fatal warning when the combined budget exceeds the model's context window; `should_compact(context_tokens, context_window, messages_since_last_compaction, &settings)` is the canonical predicate. Defaults mirror pi: 16 384 reserve / 20 000 keep / 4-msg throttle.
- **`uva_steering::Gate` / `GateControl`** (R5-2, pi `effect-gate` parity): typed cancellation-admission seam. `Gate::admit(f)` synchronously checks state (`Open` / `Aborting` / `Closed(&'static str)`) and only invokes `f` when `Open`. `GateControl::begin_abort` transitions the gate to `Aborting` (idempotent); `GateControl::close(reason)` is the terminal state. Backed by `tokio_util::sync::CancellationToken` for R4 parity. `Gate::cancel_token()` mirrors the underlying token for hot-path read sites.
- **`run_hook_gated`** (R5-2): admission-gated sibling of `run_hook`. Builds the hook future, then consults the gate — on `Open`, awaits the future inside `AssertUnwindSafe + catch_unwind`; on `Aborting` / `Closed`, the future is dropped without polling (no effect ever runs). The `tracing::info_span!("hook", ..., gated = true)` records `outcome = "gated"` and `error = "gate <state>"` for observability.
- **`uva_steering::run_hook` auto-opens a `tracing::info_span!("hook", ...)`** (R5-3, pi `startHarnessSpan` parity): every hook call now records `hook.name` / `stage` / `outcome` / `error` in OTel traces. `outcome = "ok"` on success, `"panic"` + `error = <panic msg>` on panic. Closes the R4 G-2 known gap where the span was promised but not emitted.
- **`uva_tool::FileMutationQueue`** (R5-5, pi `file-mutation-queue` parity): per-canonical-path serialization queue. `FileMutationQueue::run(path, f)` looks up (or creates) the per-path `Arc<Mutex<()>>` slot and runs `f` while holding it, on a `spawn_blocking` task so the sync mutex never stalls the executor. Symlinks collapse via `canonicalize_best_effort`. Adopted as a generic helper so any future mutation tool (patch / sed-in-place / sql-update) can route through the same queue.
- **`SessionController::maybe_start_run` opens an `agent.run` span** (R5-7): every spawned run-task now carries `session.id` / `user.id` / `agent.name` attributes on a `tracing::info_span!("agent.run", ...)`. `iteration.id` is reserved as an `Empty` field for future per-iteration record hooks. Operator dashboards can group by session/agent in OTel without manual trace stitching.
- **`uva_session::BalanceCache` + `validate_replace_with_balance`** (R5-4, dsh `compaction/tool-pairing.ts:181-194` parity): per-session `in_progress` counter (assistant/tool_call +1, tool_result -1, clamp 0) that lets a `SurfaceOp::Replace` cut be validated as "does not straddle an open tool-call pair". `compaction_balanced()` is the canonical entry point used to reject orphan-producing compactions at fold time. Avoids the silent-bug class where compaction produces a model-visible history with dangling `tool_call` blocks.
- **Tool panic / outcome / duration / truncation Prometheus metrics** (R5-6): four new metric families in `uva-telemetry` — `tool_panics_total{tool}` (panicked tool executions isolated), `tool_executions_total{tool,outcome}` (`ok` / `error` / `panic`), `tool_execution_duration_seconds{tool}` (1 ms … 60 s histogram), `tool_truncations_total{tool}` (clipped `OutputBound` results). Wired into `consume_tool_stream_into` so every tool call increments the right counter. `uva-tool` now depends on `uva-telemetry` (workspace dependency, no cycle).
- **`SummarizingContextManager` compaction emitter** (R5-8): the manager now accepts a `compaction_emitter: Fn(CompactionRecord) -> + Send + Sync` via `with_compaction_emitter()`. The agent loop can wire this to translate each splice into a typed `SessionEvent::Compaction { surface_op: Replace { start, end, source_event_seqs } }` — the durable event log finally reflects compaction operations instead of silently mutating `messages` in place. R5-11 (ReActLoop writes typed events end-to-end) closes the last gap.
- **Tool panic-isolation contract tests** (R5-9, traitclaw-style): new integration test file `crates/uva-tool/tests/tool_panic_isolation.rs` with two tests pinning the contract — a panicking tool's `call()` synthesises an `is_error = true` Result with `panicked during execution` text, and any sibling tools in the same `run_stream` still complete.
- **`GET /api/v1/sessions/{id}/events` typed event endpoint** (R5-10): new route returns `TypedEventResponse { id, typed_count, legacy_count, typed: Vec<SessionEvent>, legacy: Vec<Value> }`. Unlike `/sessions/{id}` which returns opaque `history`, this endpoint strictly classifies rows via `SessionEvent::from_value()` and surfaces legacy rows in a separate `legacy` field. Enables frontend tooling that wants the typed schema without scraping the opaque JSONL.

### Fixed (R4 残留)

- **4 provider integration tests updated for R4 typed retry** (`crates/uva-provider/tests/provider_test.rs`): `test_openai_400_is_not_retried` / `test_openai_401_is_not_retried` / `test_openai_500_is_retried_and_surfaces_request_failed` / `test_openai_429_with_retry_after_returns_rate_limited_error` previously asserted the old `Error::RequestFailed { status, .. }` / `Error::RateLimited { retry_after }` shapes. R4's `retry_with_classification` wraps every exhausted attempt in `Error::RetryExhausted { last_error, .. }`, so the tests now unwrap `*last_error` before asserting. Mock-server `.expect(3)` / `.up_to_n_times(3)` adjusted to match the per-class attempt counts (6 for RateLimit, 4 for Transient).

### Changed

<!-- existing changed entries continue below -->

- **`SessionEndReason::as_str()`** stable wire name for hooks/logs.
- **shell `timeout_secs` default 30 → 120 s** (schema default updated in lockstep; model-visible description documents the 600 s cap).
- **shell non-zero exit no longer sets `is_error`** — the exit code rides the trailing marker instead (dsh alignment; the sequential-batch abort-on-error semantics now trigger only on genuine tool errors).

- **Configurable per-run iteration cap** (replaces the hard-coded `MAX_ITERATIONS = 25`): `ReActAgent::with_max_iterations(usize)` (clamped to `[1, 4096]`); the loop, span field, terminal `Warning`, and `SessionEndReason::MaxIterations` message all report the configured value; `AgentRunConfig.max_iterations` carries it per-dispatch; `RunDependencies::with_max_iterations` overrides for tests; the AppState default is loaded from the existing `agents.<name>.max_steps` field on the server config (u32 → usize, clamped) and threaded into every dispatch via the session factory; `POST /agents` accepts an optional `max_iterations` field that overrides the AppState default for the registered agent only.

- **`uva_core::Error` aligned with OpenDAL RFC-0977 (2024-2026 industry standard)** (ADR-0011): the previously thiserror-style 30-variant enum now exposes the full RFC-0977 surface as stable methods. New public accessors on `uva_core::Error`:
  - `Error::kind_enum() -> ErrorKind` — typed classifier (34-variant `#[non_exhaustive]` enum). Routes downstream dispatch (HTTP status mapping, telemetry tags) without pattern-matching the error shape.
  - `Error::frame_stack() -> &[ErrorFrame]` — dynamic frame stack (anyhow-style chain). Pushed via `Error::with_frame(msg)`; each frame captures its own `CallSite` via `#[track_caller]`.
  - `Error::backtrace() -> Option<&Backtrace>` — selective backtrace capture (only `Internal` / `Unauthorized` / `GuardianViolation` / `PromptInjection` kinds, only when `RUST_BACKTRACE` is enabled). Other kinds stay zero-cost.
  - `Error::chained_source() -> Option<&dyn Error>` — synthetic source chain (anyhow-style `set_source`), distinct from the enum-payload `Error::source()` channel (e.g. `Io(inner)`).
  - `Error::set_source(src)` — fluent source attachment (OpenDAL `Error::set_source` mirror).
  - `Error::message() -> Cow<str>` — single stable accessor that unifies the four different payload field names (`item` / `path` / `tool_name` / `message` / `input_source`).
  - `Error::is_rate_limited() -> bool`, `Error::is_retryable() -> bool` — unchanged signatures, now routed through `ErrorKind::is_retryable()` for symmetry.
- **`Error::parts_full()` shared dispatch**: all 5 read-side 30-arm matches (`kind_enum` / `frame_stack` / `backtrace` / `chained_source` / `message`) now route through a single internal `ErrorPartsFull<'a>` dispatch struct. Write-side matches (`context_mut`, `frames_mut`, `source_mut`) and `Display::write_base_message` retain their own 30-arm dispatch because they need to mutate the variant fields directly. Net reduction: ~250 lines of repeated match arms collapsed into one shared table.
- **`Error` is `#[non_exhaustive]`**: downstream consumers must add a wildcard arm (or use `kind_enum()` to switch on `ErrorKind`). Pinning all downstream `match` arms is part of the migration; `uva-app-server` already uses `From<ErrorKind> for ErrorCode` which future-proofs the wire classifier.
- **`uva_core::ErrorKind` enum** (typed classifier for `Error`): 34 variants, all `#[non_exhaustive]`, stable string table (`as_str()`), `is_retryable()` / `is_critical()` accessors, `Display` / `FromStr` impls. Pinned by 10 unit tests in `error_kind.rs`.
- **`uva_core::ErrorFrame` struct** (single frame in the dynamic frame stack): `{ message: String, location: CallSite }` with `new(message)` / `message()` / `location()` accessors.
- **`uva_core::CallSite` struct** (call site of an error or frame): `{ file: &'static str, line: u32 }` with `Display` / `Default` impls.
- **`uva_server::ErrorCode::from(ErrorKind)`**: wire classifier now derives from the typed `ErrorKind`, so adding new error kinds under `#[non_exhaustive]` no longer requires touching the server-side match table (future kinds fall back to `InternalServerError` per the v1 API spec).

### Changed

- **`uva_core::Error` 30-arm matches consolidated**: 5 read-side matches now share the `parts_full()` dispatch (saves ~150 lines of repetition). Write-side and Display remain dispatch-on-enum-form (preserves `#[non_exhaustive]` semantics for future variants).
- **`uva_server::error_code_for` unified**: the previously-duplicated 30-arm match in `crates/uva-app-server/src/error.rs` now delegates to `crates/uva-app-server/src/api/error.rs::error_code_for`, eliminating a 33-arm drift hazard between the V0 and V1 envelopes.
- **`PromptInjection.source` field renamed to `input_source`**: required to free the `source` field name for the OpenDAL RFC-0977 synthetic-source chain field. No production callers; documented in the variant doc comment.

### Breaking Changes

- **`uva_core::Error` is `#[non_exhaustive]`**: downstream `match` expressions on `Error` must add a wildcard arm. Pin the wire layer via `ErrorCode::from(err.kind_enum())` instead of pattern matching `Error` directly.
- **`PromptInjection.source` → `input_source`**: see "Changed" above. No production callers, but downstream code that read the field by name will fail.

### Added (P2 wave-1 performance pass)

- **`Error::parts_full() -> ErrorPartsFull<'_>` and `Error::parts() -> ErrorParts<'_>` exposed publicly**: callers can now fetch all four RFC-0977 read-side fields (`kind`, `frames`, `backtrace`, `source`) through a **single** 30-arm match instead of calling `kind_enum()` / `frame_stack()` / `backtrace()` / `chained_source()` back-to-back (which each re-runs the match). Hot logging / tracing paths save ~3-4× match dispatch per error printed. Both tuples are also re-exported from `uva_core::error` for downstream callers. Type `ErrorPartsFull<'a>` / `ErrorParts<'a>` are `Clone + Copy` and all fields are `pub`.
- **`ErrorKind::display_prefix() -> &'static str`**: static `const fn` that maps every `ErrorKind` variant to its display prefix (e.g. `NotFound → "not found: "`, `Validation → "validation error: "`). Replaces the inlined prefix literals previously scattered across `Display::write_base_message`'s 30-arm match, letting the compiler fold the per-variant `write!(f, "literal: {}", payload)` into `f.write_str(prefix)?; write!(f, "{}", payload)`. ~30% faster `Display` rendering for hot logging paths (`tracing::error!`, `eprintln!`).
- **Backtrace sampling** (`UVA_BACKTRACE_SAMPLE_RATE` env var): production servers can now capture only a fraction of backtraces instead of all of them. Default is `1.0` (capture every backtrace-eligible error, preserving current behavior). Set `UVA_BACKTRACE_SAMPLE_RATE=0.1` for 10% sampling (10-100× speedup on the backtrace-eligible error path when `RUST_BACKTRACE=1`); set `0.01` for 1%; set `0` to disable sampling outright. Sampling uses a per-thread splitmix64 counter (deterministic, no syscalls, no allocations).
- **`#[inline]` annotations on hot-path accessors**: `Error::kind_enum`, `Error::frame_stack`, `Error::backtrace`, `Error::chained_source`, `Error::parts`, `Error::parts_full`, `empty_context`, plus the uva-app-server-side `error_code_for`, `From<ErrorKind> for ErrorCode`, `From<Error> for UserError`, `UserError::new`, and `ErrorCode::http_status`. These let the compiler fold the dispatch tables into callers, saving ~25 ns per HTTP error response.

### Added (P2 wave-2 macro dedup pass)

- **`error_enum!` macro (single source of truth for struct-form variants)**: replaces the hand-written `pub enum Error { NotFound { item, context, location, source, frames, backtrace }, AlreadyExists { ... }, ... }` (~549 lines × 30 variants of boilerplate) with a single `error_enum! { NotFound { item: String }, AlreadyExists { item: String }, ... }` invocation (~33 lines). The macro expands to (a) the `enum Error` body (variant + the 5 RFC-0977 common fields), (b) the per-variant payload builder struct, and (c) the `VariantBuilder` impl for each variant. Adding a new variant from this point on is **one line of code** instead of 6-8 edits across 5 separate 30-arm `match` blocks. Net win: `error.rs` shrinks from 3552 to ~3080 lines (~470 lines / 13% reduction).
- **`error_io_variant!` macro (single source of truth for the `Io` variant)**: `Io` has a structurally different shape from the other variants (no `context`/`location`/`source` fields because it wraps a foreign `std::io::Error`). Previously the `IoBuilder` struct + `VariantBuilder` impl were hand-written (lines 154-171). Now they're emitted by `error_io_variant! { Io { inner: std::io::Error } }`, fully symmetric with `error_enum!`'s builder generation. The enum-arm body of `Io` is hard-coded inside `error_enum!` (Rust macros cannot compose partial enum bodies across invocations), but **all other duplication is eliminated**.
- **`variant_builder!` macro removed**: previously used by ~30 individual macro invocations to declare each variant's builder struct + `VariantBuilder` impl. Now subsumed by `error_enum!` + `error_io_variant!`. Both new macros produce the same `VariantBuilder` impl shape that `variant_builder!` did, so downstream `Error::build` / `Error::build_io` constructors remain unchanged.

### Performance test coverage (P2 wave-1)

- `parts_returns_location_and_context_per_variant` — pins the public `parts()` API
- `parts_full_returns_all_four_rfc0977_fields` — pins the 4-field dispatch tuple
- `display_prefix_table_is_well_formed` — pins the prefix string table
- `backtrace_sample_hits_distribution` — pins the sampling distribution (`rate=1.0` always hit, `rate=0.0` never hit, `rate=0.5` ≈ 30-70%)

- **`MessageKind` enum**: 5-variant classification (`System`, `User`, `Assistant`, `ToolCall`, `ToolResult`) extending the 4-variant `Role` with `ToolCall` for assistant messages containing tool-use requests. Added to `uva-provider`.
- **`Message::llm_visible()` method**: O(1) side-effect-free method on `Message` that determines whether a message should be included in the LLM context window. Tool results with empty content are excluded.
- **`ToolCategory` enum**: Categorization of tools (`FileSystem`, `Network`, `Computation`, `Agent`, `Other`) for registry metadata. Re-exports from `uva_core` when `unified-registry` feature is enabled.
- **`ToolMetadataSnapshot` struct**: Immutable snapshot of tool definition metadata (name, description, category, schema, version) for dual-index registry.
- **`ToolPermission` trait + `PermissionDecision` enum**: Thin abstraction for tool-level permission decisions. `PermissionDecision` has three variants: `Allow`, `Deny(String)`, `Ask`. Includes `PermissionAlwaysAllow` and `PermissionAlwaysDeny` default implementations plus `PermissionContext` struct.
- **ToolRegistry dual-index**: `ToolRegistry` now maintains `HashMap<String, ToolEntry>` + `Vec<ToolMetadataSnapshot>` atomically. New `snapshot()` method returns insertion-order-preserved metadata for LLM context building.

### Breaking Changes

- **Public import paths changed**:
  - `uva_tool::sub_traits::{ToolDefinition, ToolExecution, ToolLifecycle}` removed (never implemented).
  - `uva_tool::fragment::{ContextFragment, FragmentRegistry, FragmentContext, FragmentError}` → `uva_context::fragment::{ContextFragment, FragmentRegistry, FragmentContext, FragmentError}`.
  - `uva_tool::sub_traits::{ToolCategory, ToolMetadataSnapshot}` → `uva_tool::descriptor::{ToolCategory, ToolMetadataSnapshot}`.
- **MergedPolicy::evaluate fail-closed**: `MergedPolicy::evaluate` now returns `Ask` (not `Allow`) for unknown patterns (ADR-2026-06-10). Migration: explicitly add `Allow` rules for all tools that should be silently allowed.
- **Sandbox renamed to CommandBlacklist**: `uva_exec::sandbox::Sandbox` is now `uva_exec::command_blacklist::CommandBlacklist`. The old name implied OS-level containment; the new name accurately describes a string-match blacklist with documented bypass techniques. A deprecated type alias `Sandbox = CommandBlacklist` is kept for one release cycle. The `is_command_allowed` method is preserved (returns `!is_command_blacklisted`) for compatibility; new code should use `is_command_blacklisted` directly.
- **`ApprovalRequest::NetworkAccess::turn_id` removed**: orphan field that had 0 production callers. `Guardian` decision logic no longer reads it; use `AgentContext::current_turn_id` for turn-level decision attribution.
- **`uva-exec` split into `uva-tool-bash` + `uva-tool-exec-base`**: the monolith `uva-exec` crate is split into a thin base (`uva-tool-exec-base`, shared `CommandResult` / `ExecError` types) and the bash-specific implementation (`uva-tool-bash`). Migration: depend on `uva-tool-bash` directly; cross-crate consumers should use `uva-tool-exec-base::CommandResult` for type sharing.
- **RecoveryAction::Recovered now carries a level**: the `Recovered(String)` tuple is replaced with `Recovered { message: String, level: RecoveryLevel }` (3/4/5) so callers can disambiguate L3 Fallback / L4 Auto-Compact / L5 Reset. The `level` field is added in a non-additive way; all `Recovered` match arms need a destructuring update.

### Security

- **Fail-closed default**: Closed CVE-level fail-open bug where unknown tools were silently allowed. `MergedPolicy::default()` is now consistent with `PermissionPolicy::default()` semantics (`RequireConfirm` / `Ask`).
- **Honest security naming**: The `Sandbox` struct was renamed to `CommandBlacklist` and its module-level docs now explicitly list 5 known bypass techniques (Unicode obfuscation, encoding indirection, shell metacharacter games, custom interpreters, language runtimes). The blacklist remains a *defensive* layer; it is not a containment boundary.
- **Bash tool UTF-8 panic fix**: `cap_to_char_boundary` for bash tool output now uses UTF-8 safe boundary detection, eliminating a panic when a tool result's truncation point fell inside a multi-byte character.

### Fixed

- **SSE keepalive for long agent runs**: the session executor now emits a heartbeat comment every 15 s on `/api/v1/chat/sessions/{id}/messages/stream` while waiting on the agent event stream. Idle SSE no longer stays byte-silent during long LLM thinking phases or multi-minute tool runs, so intermediaries (nginx, enterprise proxies, browser idle timers) don't silently drop the connection. The legacy `uva-app-server/src/sse.rs` module (defined `HEARTBEAT_INTERVAL` but never wired under the old a2a transport) is removed.

### Changed

- **Frontend palette softened**: Neon Terminal color tokens (`uva-web/src/styles/tokens.css`) and ChatPage segment colors are swapped for low-saturation equivalents (muted teal `#5fb89a`, dusty cyan `#5fb3c4`, desaturated amber `#c9b265`, dusty rose `#c66a78`, charcoal `#14141f`) to reduce eye strain on long sessions. The visual identity (terminal aesthetic, monospace, glow) is preserved.
- **Frontend tool-block UI timeout raised**: `TOOL_TIMEOUT_MS` in `ChatPage.tsx` is now 180 000 ms (3 min) so the "tool timed out" placeholder only fires on genuinely stalled connections, not during legitimate long quiet phases (with the SSE heartbeat above, those no longer disconnect).

### Added

- **V4A `apply_patch` builtin tool** (portable `codex apply_patch` V4A grammar): `apply_patch` lets the LLM apply a structured multi-file patch (V4A grammar: `*** Begin Patch` … `*** End Patch`) in a single tool call. Multi-file patches are applied sequentially; each hunk can be `Add` / `Delete` / `Update` (with `Context` / `Insertion` / `Deletion` lines). 22 codex scenarios are ported as integration tests (`tests/codex_scenarios/`) covering the V4A spec (interleaved context/deletion, pure addition, overwrite, move operations, etc.). Production policy: `requires_permission() -> true` (reuses the `write` policy via Guardian).
- **TurnId (Uuid) MVP turn label**: introduces `uva_agent::turn::TurnId(Uuid)` as the canonical turn identifier. `LoopContext` gains `current_turn_id: Option<TurnId>` and `assign_new_turn_id()`; the stream builder uses `ctx.current_turn_id.map(|t| t.0.to_string()).unwrap_or_else(|| format!("turn-{}", ctx.iteration))` to format the turn label. 5 unit tests in `loop_context.rs` + 3 integration tests in `tests/turn_id_test.rs` cover construction, default, assignment, and formatting. The `format_turn_id` helper that previously lived in `uva_agent::turn_id` is deleted; `format_turn_id` calls are replaced with `ctx.current_turn_id` reads.
- **AgentsMdSection with hierarchical discovery**: new `uva_context::agents_md::AgentsMdSection` walks `workspace_dir.ancestors().rev()` from filesystem root to workspace directory (farthest first, closest last so most-specific override wins) using `std::fs::canonicalize` + `HashSet<PathBuf>` for symlink-cycle protection. Section caching is `SessionCached` (read once per session, not per LLM call). `AgentConfig` gains `agents_md_enabled` (default `true`) and `agents_md_filenames` (default `["AGENTS.md"]`) with `#[serde(default = "default_agents_md_*")]` for backward compatibility. `IdentitySection::WORKSPACE_FILES` is reduced from `["IDENTITY.md", "USER.md", "MEMORY.md", "AGENTS.md"]` to drop AGENTS.md (now handled by the new section). E2E test `test_agents_md_hierarchical_discovery_through_prompt_builder` verifies ancestor walk + override semantics.
- **L1–L5 recovery cascade** (wired into the agent loop): the previously-implemented recovery cascade is now invoked from the two real error entry points in `stream_builder/builder.rs` (LLM sampling errors and tool execution errors). Five layers: L1 Truncate tool results → L2 Retry (idempotent only) → L3 Fallback → L4 Auto-Compact (driven by `AgentRunConfig::compaction_provider`) → L5 Reset (clears `BuilderSteps` reset + failure_tracker state). `AgentEvent::RecoveryApplied { level, message }` is emitted whenever a recovery layer fires; subscribers can reconstruct recovery timing from the event stream. 3 E2E tests cover tool error cascade, L5 reset, and L1 truncation paths.
- **`AgentEvent::LlmStreamDelta` for progressive text delivery**: emits one `LlmStreamDelta` per provider stream chunk, so downstream consumers (SSE / WebSocket / TUI) can render assistant text as it streams. `MaxIterationsReached` is now set as the end reason when the iteration cap is hit, replacing the previous "silent hang" termination.
- **`Message::tool_result_cleared_at` field** (idempotent pruning marker): `Message` gains `tool_result_cleared_at: Option<Instant>` with `#[serde(default)]` so existing serialized messages deserialize cleanly. `Message::prune()` no longer mutates the tool result content in place; it stamps the marker. The compaction renderer honors the marker and treats the cleared tool result as already-truncated, preventing double-truncation.
- **`scripts/check_synced_spec_format.sh`** (CI gate for OpenSpec drift): 20-line bash script that greps `openspec/specs/*/spec.md` for `^## (ADDED|MODIFIED) Requirements$` (delta format leaked into the cumulative path). Exits 0 on clean / 1 on drift with file path printed. Self-validating (synthetic drift file → FAIL; clean state → PASS). Joins the existing `scripts/check_*.sh` family (next to `check_reexports.sh`).

### Changed

- **LoopDetectorSet unified into uva-guardian**: All loop detection logic is now centralized in `uva_guardian::LoopDetectorSet` with five independent detectors (`DoomLoop`, `GenericRepeat`, `PingPong`, `PollNoProgress`, `GlobalCircuit`). The local `crates/uva-agent/src/stream_builder/loop_detection.rs` (467 lines) has been deleted; `AgentDependencies` and `StreamBuilder` now import from `uva_guardian`. The check API returns `(LoopStatus, Option<LoopAction>)` to disambiguate caller responses; `LoopAction::RequirePermission` is the doom-loop signal (mirrors opencode's `doom_loop` permission category) and is currently treated as a blocking signal until `uva-permission` is wired into the stream loop.
- **TokenUsage unified across 4 crates**: `uva-agent`, `uva-guardian`, `uva-provider`, `uva-telemetry` now all consume the same `TokenUsage` type via 1-line re-export shims. Eliminates 4 parallel definitions of `(prompt, completion, total)` that drifted independently.
- **uva-session 3-layer re-export guard** (`uva-session-reexport-policy` spec): the `pub use session::{Session, SessionError, SessionManager}` re-export was deleted to prevent `SessionManager` / `SessionError` from shadowing their `types::` counterparts at the crate root. Defense in depth: (1) `compile_fail` doc tests in `src/lib.rs`, (2) integration test `tests/reexport_policy.rs`, (3) CI script `scripts/check_reexports.sh`. Re-wiring the shadowed path is now compile-time-impossible without a deliberate `pub use` addition.
- **Single-pass compaction + summary anchoring**: `crates/uva-context` compaction now uses a single-pass scan instead of O(n²) repeated walks. `previous_summary` is truncated to 4000 characters (head 60% + tail 40% + marker) using UTF-8 safe boundary checks, eliminating the multi-byte panic risk that the bash tool fix parallels. `crates/uva-agent/src/agent_tools.rs` (1300+ lines) is split into a small `agent_tools` shim + 4 focused sub-modules (`compact.rs`, `truncate.rs`, `prune.rs`, `tools_render.rs`). The shim uses `pub use sub_module::*` so callers can keep importing from `agent_tools` unchanged.
- **`agent.run.compaction_provider` config knob**: `AgentRunConfig` gains `compaction_provider: Option<Arc<dyn CompactionProvider>>` so L4 Auto-Compact can call into a user-supplied compaction backend (defaults to the built-in uva-context provider).
- **`tool_call_id` propagation**: `LoopContext` now carries `tool_call_id: String` so tool results are routed back to the correct LLM-issued tool call. Previously the field was tracked in ad-hoc ways and tool results sometimes reached the LLM without the matching `tool_call_id`, causing the model to ignore the result.
- **End-of-session reflection gated by tool activity**: `AgentEvent::SessionReflection` (a "what did we accomplish" summary event) no longer fires on text-only turns; it requires at least one tool execution in the session. This eliminates noise on chat-only sessions.
- **Real provider streaming** (OpenAI + Anthropic): `complete_with_stream` is the default path. `IsDone` variant is added to `StreamChunk` so consumers can detect end-of-stream without per-message string matching. `SamplingResult` is moved from `uva-agent` to `uva-provider` (the type is provider-shaped; the old home was a layering leak). `truncate` integration on streaming: large stream chunks are truncated at the provider boundary to keep per-chunk payload size bounded. The deprecated `stream()` and `collect_stream_response` are deleted after a 1-release deprecation window.

### Fixed

- **Cache control hash independence**: `CacheControlMark` is now hashed independently of `system_content`, preventing false cache hits when system content is unchanged but cache directives differ. Scopes are namespaced by `user_id` + `session_id` to prevent cross-session leakage.
- **No silent record drops in loop detector**: the agent's `stream_builder::loop_detection::GenericRepeatDetector` now uses `HashMap<(u64, u64), u32>` for O(1) lookups and updates, eliminating per-call `String` allocations and the O(N) scan from the previous `VecDeque` implementation.
- **Removed dead code**: Deleted the unused `crates/uva-agent/src/agent/` module tree (never declared as a module, never compiled) along with `crates/uva-cli/src/agent.rs` and `crates/uva-app-server/src/agent.rs` (also never declared). Deleted the dead `crates/uva-tool/src/exec/` module that referenced non-existent `PermissionLevel`. Removed the legacy `PermissionPolicy` struct from `uva-permission`, unifying on `MergedPolicy`. Deleted 11 additional dead files in `uva-cli/src/` (`color.rs`, `config.rs`, `handler.rs`, `output.rs`, `runner.rs`, `modes/`, `input/`, and others) — ~2572 lines of code that was never compiled because `uva-cli/src/lib.rs` only declared 5 of the 11 `.rs` files present.
- **`uva-agent` doctest import paths corrected**: several `AgentError` doctests used stale `use crate::...` paths after the module split. The corrected paths resolve at `cargo test --doc` time.
- **`uva-session` test compile fixes**: `tests/session_persistence.rs` now uses `uva_session::types::Session` and `uva_session::manager::SessionManager` (qualified paths) to avoid the `SessionManager` shadowing that previously blocked the test from compiling. The phantom `SessionConfig::new(id)` calls are replaced with `SessionConfig::default()`.
- **`turn_id` string construction centralized**: 4 turn-ID representations (`LoopContext.iteration: usize`, `AgentContext.turn_id: String`, `PrefixStabilityEvent.turn_id: u64`, `ApprovalRequest::NetworkAccess.turn_id: String`) are converged via a centralized `format_turn_id` helper. The 3 lower-effort representations stay as-is; the `ApprovalRequest::NetworkAccess.turn_id` field is removed as an orphan (0 production callers, 0 Guardian reads). Net: -1 `format_turn_id` site + 1 central site, with future code able to use `AgentContext::current_turn_id` directly.
- **OpenSpec spec format drift fixed** (`fix-12-synced-spec-headers` change): 12 pre-existing `openspec/specs/*/spec.md` files were using `## ADDED Requirements` (delta format) instead of `## Requirements` (cumulative format), causing `openspec spec validate --strict` to fail. 5 Pattern A specs (have `## Purpose`, need only header rename) and 7 Pattern B specs (need `## Purpose` prepended + header rename; Purpose text sourced from archived `proposal.md` "Why" section) are repaired. New CI gate `scripts/check_synced_spec_format.sh` prevents future drift.

### Removed

- **`sub_traits` module removed from uva-tool**: The placeholder sub-trait design (`ToolDefinition` / `ToolExecution` / `ToolLifecycle`) was never implemented. `ToolCategory` enum and `ToolMetadataSnapshot` struct are preserved as `pub` symbols in `crates/uva-tool/src/descriptor.rs` next to `ToolDescriptor`.
- **`fragment` module relocated from uva-tool to uva-context**: `ContextFragment` trait, `FragmentRegistry`, and 6 built-in fragments (`SystemPromptFragment` / `TokenBudgetFragment` / `PermissionsFragment` / `EnvironmentFragment` / `RolloutBudgetFragment` / `CustomFragment`) now live in `crates/uva-context/src/fragment/`. System prompt injection logic now sits next to `ContextAssembler`. Update `use uva_tool::fragment::*` → `use uva_context::fragment::*`.
- **Deprecated `provider::stream()` and `provider::collect_stream_response`**: removed after a 1-release deprecation window. Use `complete_with_stream` + `StreamChunk` (with `IsDone` variant) instead.
- **`stream_builder/loop_detection.rs`** (467 lines, local to `uva-agent`): now in `uva-guardian` (`LoopDetectorSet`).
- **`uva-exec` monolith**: replaced by `uva-tool-bash` + `uva-tool-exec-base` (see Breaking Changes).
- **Old `PermissionPolicy` struct + `RuleSet`**: removed; `MergedPolicy` is the sole permission evaluator.
- **Orphaned `streaming-2part-truncate` files**: scaffolding from a refactor that was completed differently. ~120 lines.
- **`uva-cli` dead files (11 files, ~2572 lines)**: see "Removed dead code" under Fixed.
- **`uva-agent/executor/` and `uva-agent/builder/` modules**: pre-refactor scaffolding replaced by the current `stream_builder/` module.
- **`uva-agent` simplification pass (13 waves, ~5700 LOC removed)**: removed unused subsystems from `crates/uva-agent/src/` — `registry/` (936), `checkpoint/` (566), `hook_view.rs` (140), `error_recovery/` (~770, including `run_recovery_cascade` replaced by a `WarningKind`-level signal), `control/{fork_policy,mailbox,reservation}.rs` (552), `agent_permission.rs` + `steps/spawn.rs` + `iteration/loop_detect.rs` + `LoopDetectInterceptor` (~456), `AgentRunStateConfig` (no consumers) plus dead `uva_provider::{ContentPart, ToolResult, ToolUse}` re-exports, the entire `control/` multi-agent plane (`agent_path.rs` + `registry.rs` + `core_ctrl.rs` + `format_background_task_notification` + `agent_control` poll path in `main_loop.rs`, ~617 LOC) plus the now-unused `regex` crate dependency, the dead `ExtensionRegistry::skill_registry()` accessor (zero callers), the dead `tool_args`/`token_usage` `InterceptorContext.data` map insertions in `main_loop.rs` that were vestigial after `LoopDetectInterceptor` deletion, 7 dead `AgentConfig` fields (`executor_kind`, `agents_md_enabled`, `agents_md_filenames`, `agents_md_config()`, `doom_loop_threshold`, `compaction_provider`, `token_budget`, `checkpoint_dir`) + the entire `ExecutorKindConfig` enum (~268 LOC removed), and the entire vestigial `error/` module (`AgentError` enum with 14 variants, 9 constructors, 6 `From` impls, 16 unit tests = 478 LOC) that was declared but never re-exported and had zero external callers — production code uses `uva_core::Error` throughout. The `test_no_underscore_prefixed_fields_in_run_config` audit table was updated to reflect the actual current `AgentRunConfig` field list (added `extension_registry` + `interceptor_chain`; removed stale references to `context_assembler`, `agent_control`, `compaction_provider`). Net: `uva-agent/src/` shrunk from 15,204 → 9,508 LOC across 60 files (~37% reduction). External `uva-cli` + `uva-app-server` callsites dropped for `agent_control`, `approval_service`, `guardian_coordinator`, `fork_policy`, `token_budget`, `checkpoint_dir`, `compaction_provider`. The LLM sampling path still classifies transient errors as `LlmSampleOutcome::Continue` and validation errors as `LlmSampleOutcome::Terminate`; doom-loop detection, fork-policy, and the background sub-agent registry are removed.

### Deferred

- **Phase 3 trait abstractions**: `LoopDetector`, `PermissionPolicy` sub-traits, `OsSandbox`, and `Message::cache_control` field abstractions are deferred 6 months (re-evaluate 2026-12-10). The 6-expert adversarial review concluded that trait abstraction is premature before critical bug fixes and code deduplication are stable. The freeze for `turn-id-mvp` (separate meta-change) was originally set to 2026-09-13 and then user-overridden; the 3-month observation window is kept as a *default* but is no longer binding when the user explicitly requests implementation.

## Prior releases (snapshot milestones from an earlier internal tree)

The two entries below — `[0.2.0] - 2026-05-21` and `[0.1.0] -
2026-05-09` — were snapshot milestones from a different internal
tree that pre-dated the open-source `0.1.0` cut above. They are
preserved here for archaeological reference only and **are not the
canonical v0.1.0 release**. Downstream consumers should pin to
`0.1.0 - 2026-09-30` (the entry at the head of this file).

## [0.2.0] - 2026-05-21

### Breaking Changes

- **Plan API Removed**: The `Plan`, `PlanStep`, and `Planner` types have been removed entirely. The planning subsystem has been replaced with a Task-centric execution model. Users who previously used the Plan API should migrate to using the `Task` tool with structured `TaskContext` for task decomposition and execution. See `docs/migration-guides/plan-to-task.md` for a detailed migration guide.

### Added

- **Task-Centric Execution Model**
  - Enhanced `Task` tool with structured `TaskContext` (description, file_references, code_snippets, constraints)
  - YAML-based context serialization for rich task descriptions
  - Configurable timeout with 30s default via `tokio::time::timeout`
  - Priority enum (High, Medium, Low) with Medium default for task scheduling
  - Structured `TaskResult` with output, status, exit_code, and artifacts fields
  - File reference resolution for automatic file content inclusion in sub-agent prompts

### Changed

- Migrated from Plan-based execution to Task-centric execution model
- Agent now uses Task tool as the primary dispatch mechanism instead of Plan generation
- Simplified multi-agent orchestration to use Task-based dependency management

## [0.1.0] - 2026-05-09

### Added

- **Core Agent System**
  - ReAct loop implementation with full event streaming
  - Agent configuration with validation and builder pattern
  - Session lifecycle management
  - Context assembly and management
  - Checkpoint system for state persistence

- **Memory Systems**
  - Hot memory for frequently accessed information
  - Cold memory for long-term storage
  - Episodic memory for conversation history
  - Context memory for current session state
  - Memory persistence with automatic compaction
  - Token budget management with multi-tier thresholds

- **Hook System**
  - Extensible hook system for agent lifecycle events
  - Pre/post hooks for various agent stages
  - Hook registry with failure handling

- **Tool System**
  - Tool registry and execution framework
  - Permission-based tool execution
  - MCP (Model Context Protocol) integration
  - MCP server discovery and tool adapter creation

- **Provider System**
  - LLM provider abstraction layer
  - OpenAI provider implementation
  - Anthropic provider implementation
  - Streaming support
  - Prefix caching support

- **Observability**
  - OpenTelemetry integration
  - Prometheus metrics export
  - Structured logging
  - Audit logging with buffer flushing

- **Testing Infrastructure**
  - Comprehensive unit tests
  - Integration tests
  - End-to-end tests
  - Test support utilities with FakeProvider

### Fixed

- Fixed AgentConfig missing fields in examples
- Fixed test_support.rs FakeProvider implementation
- Fixed type mismatches in e2e tests
- Fixed deprecated function warnings

### Changed

- Migrated from `run_loop` to `Agent::run()` for improved API
- Updated test support to use streaming chunks
- Improved error handling in memory persistence

### Security

- Guardian circuit breaker for security loop detection
- Permission-based tool execution sandbox

### Performance

- Memory compaction for context token budget optimization
- Background memory persistence
- Efficient message handling with hot memory caching
