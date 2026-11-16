# 挂载点的改写面：工具入参不可改写，出参可改写

> 依据：设计稿 [`2026-11-15-plugin-crate-splits-design.md`](../design/2026-11-15-plugin-crate-splits-design.md)
> §6.2.2；调研 [`…plugin-crate-splits-dsh-omp.md`](../research/2026-11-15-plugin-crate-splits-dsh-omp.md)
> 与 [`…subagent-live-visibility.md`](../research/2026-11-15-subagent-live-visibility.md)。
> 证据 id（形如 `[E025]`）均出自前者。

两个参考实现在这一处意见相反：omp 的 `tool_call` 前置事件允许 handler 替换工具执行所用
的入参，并按改写后的入参**重算审批** `[E087]`；dsh 明令禁止，理由是「历史、审计、UI、执行
必须一致」`[E025][E058]`。这不是移植题：uva 的 `HookDecision` 五档里没有工具入参改写档
（`ReplaceParams` 只被 `llm/request` 的 `BeforeRequest` 接受，改写的是 provider 请求参数，
不是工具入参 `[E074]`），选哪边都是新裁定。而 uva 的工具入参被三处各自定格成 durable
事实：`tool_call` 行随 assistant 消息在挂载点之前落盘（界面与重载看到「模型要了
什么」）；`tool_dispatch` 意图行在执行前写入 `arguments_hash`；`ApprovalRequest` 的
身份就含 `arguments_hash`，审批 grant 按它绑定调用。挂载点若能再改一层入参，三处至少
分叉一处：展示行要么离执行越来越远、要么被改写成不再说模型说过的话；而 resume 用
重放时的守卫结果对账 `tool_dispatch` 哈希——对不上即拒绝续跑（"the dispatch for
{call_id} does not match the guarded call"）——外插的改写层无法在重放时确定性重现。

**入参侧采 dsh：`tool/pre-execute` 的裁决词汇不给工具入参改写档**（`BeforeTool` 只接受
`Allow`/`Block`/`Ask`/`Terminate`，现状即如此，本 ADR 把「不补这一档」定为裁定）。
**出参改写是公开能力并保持**：`ResultTransformer` 在提交前原位改写聚合输出，随 `uva-hook`
契约下沉，成为外部插件的脱敏/重排出口；溯源由现有 `tool_result_transformed` 行承载——
`transformers` 改写者名单 + `before`/`after` 全量 sha256，语义是 provenance（谁在提交前
改了模型将读到的结果），不是 reconciliation（不回滚、不并存储两份）。

## Considered Options

- **采 omp（允许改写 + 改写后重算审批）**：被否。omp 的自洽建立在审批于改写**之后**解析；
  uva 的 durable 行先于或旁路于挂载点落盘：`tool_call` 行要么展示改写前（UI 与执行分叉）、
  要么被改写（历史不再说模型说过的话）；`tool_dispatch` 与审批哈希要推迟到改写后重算；
  resume 对账要认得改写。三处同改才不破「同哈希即同调用」。将来真要开，须另开 ADR 并同改
  三处。
- **两侧都禁（出参也不许改写）**：被否。出参没有身份问题：改写发生在提交之前，
  `tool_result` 行与历史持有改写后文本——日志说的就是模型读到的；原始输出不是任何
  grant/对账的凭据。禁掉它只损失能力（脱敏是 `uva-hook-plugins` 首批模块），换不回一致性。
- **新增 `tool-args transformer` trait 而非 `HookDecision` 新档**：被否（也作为将来若开放
  的形状备注）——换词汇不换问题，三处定格照样分叉。

## Consequences

- 三处分叉论证的落点：入参的三处定格是**身份**（审批 grant 与 resume 靠 `arguments_hash`
  认调用），出参只有**顺序**（先改后提交）。两者不可比，「为对称而对称」的两个方向都被否。
- `HookDecision::ReplaceParams` 语义保持窄义：只在 `BeforeRequest` 被接受（`accepts` 五点
  表），改写 provider 请求参数（model、temperature、max tokens、tool choice、tool
  schemas），消息列表不在改写面；`llm/request` 的 effective 载荷写回链路不变。本 ADR
  不动它。
- `tool/pre-execute` 链上的 `SeamPass::rewrite` 是 waterfall 的通用机制（`llm/request` 在
  用），但 `tool_pre` 分发从不把改写后事件写回执行——下沉后这一语义固化：即使 middleware
  改写了 `BeforeTool` 载荷，后续注册者读到改写版，执行入参不变。
- 内建 steering guard 的 `Sanitize` 不在裁定范围：它不是挂载点，跑在审批哈希与 dispatch
  哈希之前，三处定格用的都是改写后的 effective call（"the approved arguments are the
  executed arguments"）；`tool_call` 行保留模型原始请求，正是「行说模型的话、哈希认执行
  的调用」的既有分工。
