# 会话写入者的类型化事实不再经 JSON 键绕一圈

ADR-0009 把「一行携带什么」的**读法**收进行词汇，消费者改问它。写入侧还在绕：
`LogWriter` 收到类型化的 `SessionEvent`，**先序列化成 `Value`，再按 JSON 键把类型化的事实读回来**。

- `Plugin` 变体有类型化的 `kind: String` / `payload: Value` / `original_bytes: Option<u64>`
  （`events/mod.rs:872-892`），而 `bound_plugin_row` 在序列化行上
  `remove("payload")` / `remove("kind")` / `insert("original_bytes")`（`:1038-1050`）。
- `ToolOutput` 变体有类型化的 `call_id: Option<String>`（`:410-424`），而
  `tool_output_call` 读 `.get("call_id")`（`writer/mod.rs:832`）。

一个字段改名，事实搬家了，按键读的那位留在原地——正是读侧已经消掉的那类漂移。

同时有三样**不是**泄漏，明确不动：

- `stamp()` 按 `seq`/`ts`/`ignorable` 键盖信封。信封是**写入者自己拥有的知识**：
  序号是它向 sink 预订的，时间戳是它的钟；`ignorable` 的决定**已经问词汇**
  （`restore_row(&value).tag().is_some_and(|t| t.ignorable())`）。构造器以
  `seq: 0, ts: ""` 占位、由 `stamp` 覆盖，是这条分工的自然结果。
- `merge_tool_output_row` 在批次缓冲里读/写 `data.output` / `data.original_bytes`。
  `ToolOutput.data` 是个 `Value` 袋——`output` 这个键**没有**类型化字段可问；
  给袋里的自由格式发明结构，与 ADR-0009「自由格式的载荷保留手读」是同一条判断。
- `append_copied` 的 `remap_citations` 按 `surface_op` 键改抄来的行——它的输入本来就是
  原始行。

## 决定

- **bound 搬到类型化事件上，序列化之前做。** `append_event` / `append_event_nowait`
  在序列化前 bound 类型化的 `SessionEvent`；Value 级的 `bound_plugin_row` /
  `bound_tool_output_row` 删除。变体字段再公开也逃不过——序列化前必经这条入口。
- **`append_copied` 的行不再重复 bound。** 它们来自本 crate 写的子日志，写入那一刻
  已经 bound 过；幂等注释自己写着「a row this has already bounded is left alone」，
  重复跑本来就是空操作，删掉重复什么都没损失。
- **`tool_output_call` 删掉**：合并的判定直接问类型化事件的 `call_id` 字段。
- **`append_copied` 的重映射改问词汇**：`row.get("seq")` → `CurrentRow::seq()`。
- **`stamp` / `merge_tool_output_row` / 批次缓冲不动**——它们是写入者的内部事务
  与它自己的信封。

## 参考

- [ADR-0009](0009-row-vocabulary-owns-the-payload.md) 把载荷的**读法**收进行词汇；
  本次是同一把刀落在**写侧**：类型化的事实不该经序列化绕一圈再按键读回。
- `bound_plugin_row` 的文档写明它跑在每一条入口上，「so the cap holds for a row built
  through the variant literal as well」——变体字段是公开的，只有构造器强制的大小上限
  等于没有上限。类型化 bound 仍然跑在每一条入口上，这个保证不降级。
- ADR-0009 的「自由格式的载荷保留手读」：`ToolOutput.data` 的 `output` 键是袋里的
  自由格式，没有类型化字段可问。

## Considered Options

- **给 `SessionEvent` 加 `set_seq`/`set_ts`/surface_op 的 setter，把 stamp 也类型化**：
  被否。信封是写入者自己拥有的知识，不是词汇的；加 setter 是公开面增长，
  换不来任何漂移的消除——`stamp` 的按键插入没有第二份事实可漂移。
- **批次缓冲改成类型化事件，合并也类型化**：被否。`data.output` 没有类型化字段；
  重写写入者的内部缓冲是负收益。
- **bound 双份保留（类型化 + Value 级兜底）**：被否。兜底是给「绕过类型化构造的
  调用方」的，但序列化前必经类型化入口；留两份正是这一刀要消的东西。
- **`append_copied` 也保留 Value 级 bound**：被否。幂等性证明重复跑是空操作。

## Consequences

- **冻结面不变**：`bound_plugin_row` / `bound_tool_output_row` 是 `pub(crate)`，
  不是 crate 根的名字；类型化 bound 也不新增公开项。
- **`append_copied` 少跑一次 bound**：行为不变（幂等），批次少两次逐行扫描。
- **验收是一条 grep**：`crates/uva-session/src/writer/mod.rs` 里不再出现
  `"payload"` / `"original_bytes"` / `"call_id"` 的键读写；`events/mod.rs` 里
  bound_* 删除后不再有 `remove("payload")` / `remove("kind")`。
- **现有 bound 测试迁到类型化入口**：幂等与上限的不变量守的是同一批事实，
  入口换了不变量不换。
- **切面**：一刀。Rust 侧、零线上变更、无新依赖。
