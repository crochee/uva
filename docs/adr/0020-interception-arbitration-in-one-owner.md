# 拦截裁决归一个主人：一条规则，三个 adapter

「一个注册在挂载点上的决定意味着什么」这条规则，在拦截面上被写了**三遍**，三遍的答案还
不一样：

| 规则 | `uva-hook` 注册表 | host 链上 adapter | host 残差表 |
|---|---|---|---|
| 门答不上来 | `gate_failure()`：BeforeTool→Block，能终止运行的点→Terminate，llm/*→报告 | `blocked()`：永远 Block，再被 `accept()` 降级——四个点变成 **Allow** | 同一 `accept()` |
| 注册 panic | 跳过本次、**注册保留** | 撤注册 + `Disabled` 诊断 | 撤注册 + `Disabled`；而 hint 类只跳过不撤（host 内部的第三种） |
| 决定点不接受 | 静默忽略（debug 日志） | 降级 Allow + 环里一条诊断 | 同一 `accept()` |
| 报告通道 | `RegistryPass.failures` → harness 落 warning 行 | `DiagnosticKind` 环——**运行期无人读，什么都不落** | 同左 |

这不是 cosmetic：同一个答不上来的门，在注册表路径上**终止运行**，在 host 路径上**放行**；
而 host 自己的注释写着「the pass fails closed」。ADR-0004 早就裁定「**仲裁一张表**：同一
条 waterfall、同一张 `HookEvent::accepts` 合法性表服务内外两类注册」——折叠（`absorb`）
确实一张表，但裁决的其余半边是三张。

## 决定

- **一个 owner：`uva-hook::arbitration`。** 它拥有：
  - **折叠**：`Arbitration::absorb`（从 registry.rs 搬来，拒绝单调，dsh E004/E048）；
  - **合法性**：`rule(event, decision, source)`——点的表（`HookEvent::accepts`）说了算；
    点不接受的裁决是 no-op，但**以 Fact 报告**，不再静默；
  - **fail-closed 合成**：`refusal(event, source, why)`——点能拒绝就拒绝（Block 优先于
    Terminate，模型读得到拒绝），点接受不了任何拒绝（`llm/request`、`llm/result`）就报告；
  - **panic 策略**：`PanicPolicy::{Keep, Withdraw}`——两条，写死在一处。外部插件 `Keep`
    （跳过本次、保活，一次 panic 不永久失能），进程内注册 `Withdraw`（撤除 + 报告）。
    host 内部 hint 的第三种（只跳过不撤）随之收掉；
  - **事实词汇**：`Fact { kind: GateFailed | Unsupported | Panicked, source, detail }`
    ——一个形状，两个面都报告它。
- **三个 adapter，不再各自裁决**：注册表的 `dispatch`、host 的链上 adapter
  （hook/gate/request-transformer）、host 的残差 `dispatch_hooks` 都改问 owner；残差路径
  自己那份「首个拒绝即赢」的折叠也改走 `absorb`。
- **一条 durable 通道**：注册表路径的 `RegistryPass.failures`（字符串）换成
  `facts: Vec<Fact>`；链上 adapter 跑在拿不到 sink 的内核链里，事实随载荷走
  （`HookPayload::record_fact` / `take_facts`），seam 层在 sink 所在处落成
  `WarningKind::Hook` warning 行。残差路径有 sink，直接落。**host 路径的门失败从
  「什么都不落」变成运维读得到的 warning 行。**
- **宿主的诊断环只留宿主自己的事实**（工具名冲突、注册表绑定、transformer 失败、
  `Disabled`）；裁决类诊断（`UnsupportedDecision`、`GateFailed`）从 `DiagnosticKind`
  删除——它们的形状现在是 owner 的 `Fact`。无 sink 的路径（hint、result transformer）
  的 panic 仍记 `Disabled`（那里没有 durable 通道）。

## 参考

- [ADR-0004](0004-hook-contract-sinks-below-harness.md)：裁定契约下沉成 `uva-hook`、
  「仲裁一张表」。本次是那条裁定的**补齐**——折叠早已一张表，裁决的其余半边这次归一。
  「`Seams`/`Host` 自身不下沉」「行词汇归 harness」两条原样成立：harness 仍是分发器，
  `hook_block` 行仍由它写。
- [ADR-0005](0005-hook-rewrite-surface.md)：`ReplaceParams` 只在 `BeforeRequest` 被接受
  ——本次不动合法性表本身（它在 `uva-steering::HookEvent::accepts`，冻结面未动）。

## Considered Options

- **owner 住 `uva-harness`**：被否。注册表的合成必须搬出注册表（`HookRegistry::dispatch`
  的文档语义随之变化），且与 ADR-0004「契约下沉到 harness 之下」相反。
- **owner 住 `uva-steering`（冻结面）**：被否。要同步冻结基线与 `docs/api-frozen.md`，
  且这条规则讲的是「注册」，不是词汇。`uva-hook` 不在冻结面（7 个冻结 crate 里没有它），
  公开面可增。
- **统一 panic 策略为单一一条**：被否。外部插件与进程内注册的失败语义确实不同（插件可能
  瞬时失败且不知道自己被拉黑；进程内注册崩了就是坏了）——两条策略显式命名、写在一处，
  比强行一条更诚实。
- **不接受的裁决继续静默（保留注册表的旧行为）**：被否。一个注册说出了点不接受的话，
  正是运维需要看见的配置错误；「以 Fact 报告」是这次行为变更的一部分。
- **连 `hook_block` 行的三个写者一起收**：被否（留作独立一步）。veto 行写什么、谁写是另一
  条规则；本次的 `Decided` 值与 `veto_row` 不动。

## Consequences

- **行为变更三条**（CHANGELOG 随记）：
  1. host 路径的门答不上来，在能终止运行的四个点（before_run / before_step / after_tool /
     after_step）从 **Allow（继续跑）** 变为 **Terminate（终止运行）**——与注册表路径一致，
     fail-closed 成立；两条回归测试（改动前失败、改动后通过）钉住。
  2. host 路径的裁决类事实（门失败于 llm/*、点不接受的裁决、注册 panic）从「环里一条无人
     读的诊断」变为 **durable 的 `hook` warning 行**。
  3. hint 类注册 panic 后**被撤除**（原先只跳过、每次都再试）。
- **公开面变化**：`uva-hook` 新增 `arbitration` 模块（`Arbitration` 从 `registry.rs` 搬来
  并经原路径 re-export）；`RegistryPass.failures: Vec<String>` → `facts: Vec<Fact>`；
  `HookPayload` 新增 `record_fact`/`take_facts`。`uva-harness` 删除 `Host::accept` 与
  `DiagnosticKind::{UnsupportedDecision, GateFailed}`——冻结基线按名字比较（441 个名字），
  没有名字移动，无需同步。
- **测试面**：uva-hook 的 owner 模块自带规则测试；注册表测试换 `facts` 断言；host 的三条
  诊断断言改为 warning 行断言；seams 新增两条「fail-closed」与「两面一致」回归测试。
- **词汇表**：GLOSSARY 的挂载点/裁决词条不动——语义没变，变的是规则有了唯一的主人。
