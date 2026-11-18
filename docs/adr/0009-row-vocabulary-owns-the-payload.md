# 行词汇也管一行携带什么：消费者改问词汇，载荷的形状进线上契约

`GLOSSARY.md` 的「行词汇」早就写着「载荷带哪些字段」由它说了算——**代码没兑现**。
TUI 的 `app.rs` 里有 14 处手写载荷读取，浏览器近 20 处，`uva-session` 内部 2 处；
每个消费者各自重新实现一遍 provider 消息的形状。

更糟的是其中一处**已经与生产形状失同步**。`crates/uva-session/src/repair/mod.rs:183`
把助手的 `data["content"]` 当**裸数组**读，而 harness 写的是
`{"role":"assistant","content":{"Single":{…}}}`（`Transcript::assistant_message` →
`Content::parts` → 单个 part 是 `Single`）。于是「模型发了 `tool_use`、崩溃发生在 dispatch
行之前」这一种崩溃——没有 dispatch 行可依赖——**拿不到那条合成 closer**，下一次请求就带一个
没人答的 `tool_use`。仓库的测试夹具（`repair/tests.rs:110`）注释写着「The shape the
harness actually writes」，写的却正是 harness 不写的那种形状，所以
`dangling_read_only_call_without_a_dispatch_is_closed` 是绿的，生产是坏的。

同一形状的泄漏还有一层：`Content` 是 serde 的**默认外部标签**（`{"Single":{…}}` /
`{"Multi":[…]}`），而 `Single`/`Multi` 的分家**是为了避免堆分配**
（`#[allow(clippy::large_enum_variant)]`，注释写明「单个 part 是 `Single`」）。于是 provider
的**内存布局**成了线上契约的一部分，两个客户端各写一遍读法。

我们把「这一行带了什么」的**读法**收进行词汇，并让载荷的形状进线上契约。

## 决定

- **消费者改问词汇。** TUI 的 14 处手写读取改问 `CurrentRow::message()` 与
  `CurrentRow::event()` + `SessionEvent` 的访问器；`search/text.rs` 的手写信封解析换成
  类型化解码。**自由格式的载荷保留手读**——警告行的 `message`/`kind`、hook 行的
  `hook`/`reason`/`tool`、插件行的 `kind`/`payload` 没有类型化字段，
  `data` 袋里是什么就是什么，强行类型化只会把自由格式硬塞进一个类型里。
  公开面**零增长**（`CurrentRow` 的按标签窄访问器被否）。
- **`SessionEvent` 新增一个 `message()` 访问器**，与它已有的十五个派生访问器同族
  （`boundary_kind()`、`tool_call_id()`、`delegation_*()`…）。它与
  `CurrentRow::message()` **共用同一套解码**（`decode_message`：provider 的完整编码、
  手写的 `{"role":…,"content":[…]}` 形状、带文本或调用 id 的兜底）——**一份解码实现**，
  所以读类型化事件的人与读原始行的人不会对「这一行带了什么」有分歧。
  `interrupted_turn_closers` 的**签名不动**（它在冻结面里）；`&[SessionEvent]` 已经解好了，
  再解一次是白花。
- **缺陷按「问词汇」修**，不是按「把夹具改对」修：repair 的助手行读取改问
  `SessionEvent::message()`，夹具改成 harness 真正写的形状（`Content::parts` / `Message`）。
- **写入者只写一个键。** 三个挂起行的 `data.phase` 改成 `data.kind`——写入者是唯一知道
  「这个键该叫什么」的地方；旧日志的 `phase` 由还原的宽松回退继续读。
  `kind`/`phase` 今天三处优先级不同（Rust：类型化字段 → `data.kind` → `data.phase`；
  TUI：`data.kind` → `data.phase`；Web：`data.phase` → `data.kind`），值不冲突所以**今天
  不会分歧**，是潜伏的——写入者只写一个键，漂移的可能就没了。
- **片二：`SessionTurn` 的 schema 收成按标签的判别联合，只窄化载荷**就是** provider
  `Message` 的那两行（`user_message` / `assistant_message`）。`tool_result` **不能**窄化：
  运行自己的行装的是工具消息，崩溃修复 closer 装的是自己的标记袋——一个标签两种载荷，
  说了哪种都是撒谎；`tool_output` 装的是一个流式分片，不是消息。其余行种仍是 `unknown`
  （多数行种的载荷是自由格式的，窄化不了）。**分支从词汇自己的标签联合里读出来**——
  词汇加一个标签，联合跟着多一个分支，没有第二份清单要维持。provider 的
  `Message` / `Content` / `ContentPart` 进契约目录，生成的 TS 拿到具名类型。
  **线上字节不变**——`SessionTurn` 是 schema 视图（运行时写的是 `Value`），
  变的是**生成的 TS 类型**，浏览器因此同刀迁移。
- **`Content` 的 `Single`/`Multi` 继续在线上**——它下线是 provider 冻结面上的形状变更，
  不在这一刀；但从「两个客户端各写一遍读法」变成「契约里声明的一个类型」。

## 参考

- [ADR-0006](0006-row-vocabulary-owns-the-row.md) 把「一行是什么」收成行词汇模块；
  [ADR-0007](0007-row-vocabulary-restores-at-the-read-boundary.md) 把归一移到读边界。
  本次是同一条纪律的第三步：**载荷的读法也归它**。
- `crates/uva-session/src/log_surface.rs:108-133` 的表面折叠**早就是按词汇问的**
  （`restore_row` → `current.tag()` / `current.message()`，一个载荷键都没拼）——
  它是仓里已有的反例，说明这个模式可行。
- `crates/uva-session/src/events/mod.rs:1155` 的 `normalize()` 已经把「类型化字段是读者的
  事实、`data` 袋只是同一条事实的第二份拼写」写成仓里的规矩；本次是把它落到消费者身上。
- `crates/uva-provider/src/types/content/mod.rs:11-22` 的注释写明 `Single`/`Multi` 的分家
  是为了避免堆分配——**布局不是契约**。

## Considered Options

- **给 `CurrentRow` 加按标签的窄访问器**（`tool_calls()`、`boundary_kind()`…）：被否。
  `CurrentRow::event()` 已经是那扇门——消费者要什么就问什么；窄访问器是同一批事实的
  第二份公开面。
- **把 repair 的输入从 `&[SessionEvent]` 改成 `&Rows`**：被否（虽然「行是门」更纯粹）。
  那一行要解两次（事件一次、消息一次），而 `&[SessionEvent]` 已经解好了。
- **只把夹具改成真实形状**：被否。那会让缺陷以测试失败暴露出来，但没修。
- **把 provider 的 `Content` 的线上形状扁平化**（去掉 `Single`/`Multi`）：不在这一刀。
  它是 provider 冻结面上的形状变更；这一刀先把形状**声明**出来。
- **33 个行种全切判别联合**：被否。多数行种的载荷是自由格式的，窄化不了，只造出一个
  大部分成员还是 `unknown` 的巨联合。
- **只发布 provider 的类型、不切判别联合**：被否（它是切联合的便宜版）。`data` 仍是
  `unknown`，浏览器还是得自己 cast 一次。
- **把 `repair::parked_prompts` 也收进这一刀**（它仍收 `&[Value]`）：不在这一刀。
  改成 `&Rows` 会让那条路径也带上 fail-closed 读日志的语义——那是行为变更，不是本次的题。

## Consequences

- **冻结面不变**：冻结面记的是 crate 根的 `pub use` 名字，
  `SessionEvent::message()` 是既有类型上的新方法，不是新名字——
  `scripts/frozen-api/baseline` 与 `docs/api-frozen.md` 都不用动。
  `interrupted_turn_closers` 的签名也不变；`CurrentRow` / `Rows` / `RowTag` 不动。
- **一个真缺陷修掉**：崩溃在模型 `tool_use` 与 dispatch 行之间的会话，从此拿到那条
  合成 closer。夹具改成 harness 真正写的形状，于是测试第一次覆盖生产形状。
- **夹具的形状就是读法的形状。** 三个夹具（session crate 的 repair、harness 的
  lifecycle、TUI 的 reducer）此前都在**猜**载荷的形状——猜出来的形状让读法与写法脱节：
  repair 的那条读取在生产里是死的，而它的测试是绿的。夹具改成从真实的
  `uva_provider::Message` 造出来，于是「读法与写法一致」这句话有了地方可查。
- **TUI 多了一条依赖边**：`crates/uva-tui/Cargo.toml` 加 `uva-provider`（工作区的
  `default-features = false` ✓，不带 HTTP ✓，也不新增编译单元 ✓）——它要读消息的
  part，就得能说出 `ContentPart` 这个类型。
- **harness 的一条断言从空转变成有效**：它断言「挂起不写普通的 `turn_end`」，而它的
  过滤条件匹配的是旧的两键写法——于是它什么也没断言。写入者统一到一个键之后，
  断言按它的本意重写：挂起标记之后的 `kind: end` 行必须自己说明是 `suspended`。
- **TUI 的手写载荷读取 14 → 4**：剩下的是四处**自由格式**的袋——警告行的
  `message`/`kind`、hook 行的 `hook`/`reason`/`tool`、崩溃修复 closer 自己的
  `tool_name`/`error`/`interrupted`、以及边界行的 `reason`。它们没有类型化字段可问
  （`data` 袋里是什么就是什么），硬塞进一个类型只会撒谎。
  界面输出除旧方言行外不变——发布的行本就是当前形状。
- **`data.phase` → `data.kind`**：新写的行换键；旧日志继续读 ✓。断言 `data.phase` 的
  测试（app-server 的 follow 测试、session crate 的 repair 与 events 测试、harness 的
  lifecycle 测试）跟着改。
- **provider 的消息类型可派生 schema**：`uva-provider` 加 `schemars`（冻结面上不是
  新名字 ✓），`Message` / `Content` / `ContentPart` / `Role` / `ToolUse` / `ToolResult`
  与它们的叶子类型派生 `JsonSchema`——契约从此发布 provider 自己的类型，而不是一份
  手写的镜像。判别联合的**分支从 `RowTag` 自己的标签联合读出来**，契约测试把联合钉在
  词汇上：分支 = 词汇的标签，一个不多一个不少；两个消息行引用 `Message`，其余不引用。
- **生成的 TS 变**：`SessionTurn` 成为判别联合（33 个分支），`Message` / `Content` /
  `ContentPart` 进契约。浏览器同刀迁移——联合按标签窄化 `data`，消息行不再 cast。
- **未做**：`Content` 的线上形状扁平化；`repair::parked_prompts` 的门仍收
  `&[Value]`（内部问词汇并解构类型化事件——repair 读的是日志文件，原始行
  就是它的输入）；`SessionTurn` 其余行种的 `data` 仍是 `unknown`。浏览器那
  3 处 `kind`/`phase` 的优先级已随切面 ② 收进投影模块的一份 `boundaryKind`
  （`kind` 优先，`phase` 容忍旧日志）。
- **切面**（见 `GLOSSARY.md` 的「切面」）：两刀。①Rust 侧：消费者改问词汇 + 修掉
  repair 的缺陷 + 写入者的键统一 + `search/text.rs`；②跨语言：`SessionTurn` 的判别联合
  + provider 类型进契约 + 浏览器迁移。②依赖①。
