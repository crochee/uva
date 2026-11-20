# 一步的回合归一次折叠：流处理器只解码

每次模型调用（一步）走流式，同一个回合现在有三份维护者：Anthropic 与 OpenAI
两个 protocol processor 各自累计 `text` / `reasoning` / `tool_calls` /
`stop_reason` / `usage`，`BlockAssembler` 再从 `StreamChunk` 折一遍；`IsDone`
让每个 decoder 直接构造一个完整 `SamplingResult`，adapter 再把它转成
`CompletionResponse`。折叠的答案已经在一处（`uva-provider/src/assembler/mod.rs`），
decoder 的那一份是它的第二个副本。

代价已经付了三笔。

**流式 Anthropic 的 usage 恒为零。** 处理器的 `usage` 槽从未被赋值
（`streaming/anthropic/processor/mod.rs:45` 初始化、`:347` 读取，中间无人写），
而 `AnthropicStreamEvent` 根本不建模 usage（`streaming/anthropic/events.rs:9-20`），
于是 wire 上的 `message_start.message.usage` 与 `message_delta.usage` 被 serde
静默丢弃。零随 usage 行进入 `TokenMeter`——它既当总量，也当压力锚点
（`uva-session/src/token_meter/mod.rs:481-504`），再经
`/sessions/{id}/usage`、上下文投影、Anthropic 兼容响应的 `message_delta.usage`
与浏览器用量条露出去。没有任何补偿路径：`complete_with_stream` 是 policy-free
的，预算插件只记录非流式调用。

**OpenAI 丢晚到的 usage。** `process_line` 在首个 `IsDone` 之后丢弃一切
（`openai_streaming/processor/mod.rs:87`）；provider 把 usage 放在
`finish_reason` 之后时，那个数字永远不会被折进回合。

**think-extractor 的 carry 被丢。** 它只塞进 `IsDone.text`，而
`fold_is_done` 在已有流式文本时忽略 `result.text`
（`assembler/mod.rs:397-404`），所以通常情形下这段文本既不进活动投影，也不进
回合。此外 `SamplingResult::into_completion_response` 只为流式路径存在，却把
`reasoning` 与签名剥掉再交给调用方。

## 决定

- **`BlockAssembler` 是唯一折叠。** decoder 只解码：发 `Content`、`Stop`、
  `Usage` 与签名；不再构造 `SamplingResult`，也不再发 `IsDone`。
- **`Stop` 是终止标记。** `push` 的 `Stop` 臂同时置 `is_done`——今天只有
  `IsDone` 臂置它（`assembler/mod.rs:229`），否则 `finalize_assistant` 的
  `incomplete = !is_done`（`:377`）会把每个回合标成不完整。
- **两个事实需要一个载体。** 词汇新增 `StreamChunk::ReasoningSignature(String)`
  与 `StreamChunk::ToolCallName { id, name }`：前者因为签名此前只能藏在终态结果
  里，后者因为 OpenAI 兼容网关会把工具名放在后续分片里，而折叠只从起块拿到名字
  ——不补这条通道，后到的名字只会更新 decoder 里一个没人再读的字段。usage 与
  stop 复用已有变体，缺的只是发送方。
- **`IsDone` 留在词汇里，只服务「已经组装好一次回合」的 provider**：stub、
  replay helper、harness 的测试 fake。`fold_is_done` 的规则不变。
- **`complete_with_stream` 返回折叠的产物 `SamplingResult`**，不是
  `CompletionResponse`；`SamplingResult::into_completion_response` 删除，
  `CompletionResponse` 只属于 `complete`。adapter 仍折一次（它必须知道 usage、
  stop，以及这一步是否产出了内容），但折的是同一个模块，不是第二份逻辑。
- **Anthropic 的 usage 与非流式共用一份映射**：`prompt = input_tokens`、
  `completion = output_tokens`、`total = input + output`、
  `cached_prompt` 与 `cache_read = cache_read_input_tokens`、
  `cache_write = cache_creation_input_tokens`、`reasoning = None`
  （`anthropic/provider/parse/mod.rs:107-135`）。provider 报告为零的桶保留为
  `Some(0)`，与 `complete` 的形状一致。
- **OpenAI 的 decoder 在 `finish_reason` 之后继续消费到 `[DONE]`/EOF**，晚到的
  usage 照常折进；「首个 `IsDone` 之后丢弃一切」的守卫删除。
- **decoder 只保留解码草稿**：wire index → tool buffer，与 `ThinkExtractor`。
  `text` / `reasoning` / `tool_calls` / `stop_reason` / `usage` 从 decoder 消失；
  think carry 以 `Content` 发出，因此进入活动投影与回合。唯一的例外是 Anthropic
  的用量合并草稿：wire 把一次报告拆在开头帧与终止帧里，而折叠是「末次为准」，
  所以 decoder 把两帧按字段合成一份再发块——它合成的是 wire 的碎片，不是回合的
  事实，并且是这条规则的唯一豁免。
- **块是累计快照，事件是一步一次。** `Usage` 块携带「到目前为止」的累计值，折叠
  末次为准；把块翻译成事件的消费者只在折完之后发布一次。思维链曾经逐块发布，
  在 Anthropic 的两帧上会把用量加两次——这次一并改正。
- **截断的流不是回合。** 折叠在结束前从未见到终止事实时，adapter 报流错误
  （「在 provider 的终止事件之前结束」），而不是交回半个回合：折叠会排干未闭合的
  工具缓冲，半个工具调用带半截 JSON 参数被派发出去，比一次失败更糟。
- **不在本次范围**：cache breakpoint——`cache_control` 在全仓没有任何 producer，
  `fold_is_done` 硬编码 `None`，那是缺特性，不是折叠的副本。历史零 usage 行也
  不补写。

## Considered Options

- **adapter 折一次再转 `CompletionResponse`**：被否。为流式路径保留一个有损再
  投影，而且把流式调用的返回值说成 `CompletionResponse` 是撒谎——它从来没有
  经过非流式的解析路径。
- **保留 `IsDone` 并让 decoder 用它承载 usage**：被否。那正是「decoder 构造整个
  回合」，第二轮也只是把副本换个位置。
- **为 usage 或 stop 新增变体**：被否。词汇里已有 `Usage` 与 `Stop`，缺的是
  发送方。
- **`Stop` 不做终止标记、由 adapter 决定终止**：被否。`incomplete` 判据会整体
  反转，而「流在什么时候算结束」是词汇的事实，不是调用方的猜测。
- **OpenAI 的晚到 usage 留到下一票**：被否。同一模块、同一缺陷类，留着等于立刻
  再开一票。
- **补写历史 usage 行**：被否。行是日志的事实、`seq` 是消息身份；重写历史等于
  伪造身份（`make check-single-writer` 的追加语义也建立在这一点上）。

## Consequences

- **形状变化，名字不动**：`ModelProvider::complete_with_stream` 的返回类型、
  `StreamChunk` 的两个新变体、`SamplingResult` 失去一个方法。冻结基线按 crate
  根名字比较（`scripts/check-frozen-api.sh`），故基线不改；按
  `docs/api-frozen.md` 的规则记进 "Recorded shape changes" 与 changelog。
- **行为变更五条**：流式 Anthropic 步的 usage 从零变为真实值；`UsageBuckets` 的
  `cache_read_tokens` / `cache_write_tokens` 在 provider 报告零时从缺席变为
  `Some(0)`（与非流式一致，读者经 `provider_usage()` 两种形状都认）；think
  carry 进入活动投影与回合；usage 晚于 `finish_reason` 不再丢失；在终止事件之前
  结束的流重新是错误——错误类型从「空响应」改为点名的流协议错误，而思维链在
  Anthropic 上从「少记一次」变成「只按末次合并值记一次」。
- **历史行不补写**：changelog 明记「在此之前，流式 Anthropic 步的 usage 读作
  零」。
- **测试面**：processor 的累加器测试（约 30 条）删除；每个协议留一个 decoder 的
  chunk 序列断言；完成的回合与 usage 由折叠的测试断言；唯一一条 SSE → 折叠的
  集成测试（`crates/uva-provider/tests/provider_test.rs:2367`）承担跨接缝验收，
  Anthropic 流式 usage 的断言第一次存在（今天 `streaming/anthropic/tests.rs:375`
  只写 `let _ = result.usage`）。
- **切面**：① 词汇与折叠——`Stop` 置终止、签名变体、`IsDone` 收窄为「已组装的
  回合」，树绿且不改变任何 decoder；② 两个 decoder 退回解码，adapter 改返回
  类型；③ 测试与文档。②依赖①，③随②。

## 参考

- [ADR-0028](0028-one-interception-table.md)：同一个「一个主人」的判据；本次把
  它从拦截表移到一步的回合。
- [ADR-0021](0021-row-payload-shape-in-one-owner.md)：usage 行仍归行词汇；本次
  只填它已经有的槽，不改行形状的所有权。
- `docs/api-frozen.md`：名字与形状的分界，以及 shape change 的记账位置。
