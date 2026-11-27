//! 归一 —— 在读边界把一行还原成当前行。
//!
//! 一行写进会话日志的时候，可能用的是当前行词汇，也可能用的是旧
//! 方言。归一把任意一行还原成**当前行**：它在当前词汇里的标签、它
//! 投影成的模型消息、它在表面里的位置。归一只发生一次，且发生在
//! 任何读者看到这一行之前——所以读者只见当前行，不见方言。
//!
//! 旧方言于是不再是运行期类别，而成了归一的**输入**：
//!
//! | 方言 | 还原成 |
//! |---|---|
//! | `UserInput` | `UserMessage` |
//! | `Model` + 文本 / 工具调用 / 推理 | `AssistantMessage` |
//! | `Model` + 工具结果 | `ToolResult` |
//! | `Message` + `role: user` / `system` | `UserMessage` |
//! | `Message` + `role: assistant` | `AssistantMessage` |
//! | `Message` + `role: tool` | `ToolResult` |
//! | `Agent` | 无——委派子会话现在有自己的日志 |
//!
//! 角色多态的方言由**载荷**判定。`Message` 与 `Model` 的对应物取
//! 决于载荷里的角色或 part 种类，不由方言本身决定——这是 omp 那个
//! 「标签说不是用户消息、载荷说是用户消息」的真缺陷所在。
//!
//! 类型化的行以**标签**为准：载荷的角色不推翻标签。两个折叠路径
//! （类型化的与原始的）因此对同一行给出同一个答案。
//!
//! 归一不判断日志读不读得动——那是 [`Rows::read`] 的事，也是唯一
//! 一处做那个判断的地方。

use serde_json::Value;
use uva_provider::{Content, ContentPart, Message, Role};

use super::{RowDialect, RowTag};
use crate::events::{SessionEvent, SurfaceOp};

#[derive(Debug, Clone, Copy)]
/// 一行，还原成当前行。
///
/// 一行要么是当前行（[`Self::tag`] 有值），要么说清自己为什么不是
/// （[`Self::defect`] 有值）。两者互斥。
pub struct CurrentRow<'a> {
    raw: &'a Value,
    tag: Option<RowTag>,
    dialect: Option<RowDialect>,
    defect: Option<RowDefect<'a>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// 一行为何不是当前行。
pub enum RowDefect<'a> {
    /// 这一行用的是旧方言，而当前词汇没有对应物——委派的子代理轨迹
    /// 属于这一类：子会话现在有自己的日志。
    NoCounterpart,
    /// 这一行的类型当前构建不认识（版本错位）。
    UnknownTag(&'a str),
    /// 这一行没有可用的类型——例如写入者的元数据头行。
    NoTag,
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("session log row {index} carries unknown type {tag:?}")]
/// 一份日志当前构建读不了。
///
/// 读不了的原因只有一种：日志里有一行，类型当前构建不认识，而那一行
/// 没有「可忽略」标记。静默跳过一条必需事件会重建出一个错误的会话，
/// 所以宁可拒读整个日志。
pub struct UnreadableLog {
    /// 出问题的那一行在日志里的位置。
    pub index: usize,
    /// 它带的类型名。
    pub tag: String,
}

#[must_use]
/// 把一行还原成当前行。
///
/// 全函数：无分配、永不失败。一个读点只关心「这一行是什么」时用它；
/// 需要整份日志读得动时用 [`Rows::read`]。
pub fn restore_row(row: &Value) -> CurrentRow<'_> {
    let Some(type_name) = row.get("type").and_then(Value::as_str) else {
        return CurrentRow {
            raw: row,
            tag: None,
            dialect: None,
            defect: Some(RowDefect::NoTag),
        };
    };
    if let Some(tag) = RowTag::from_wire(type_name) {
        return CurrentRow {
            raw: row,
            tag: Some(tag),
            dialect: None,
            defect: None,
        };
    }
    if let Some(dialect) = RowDialect::from_wire(type_name) {
        return match restore_dialect(row, dialect) {
            Some(tag) => CurrentRow {
                raw: row,
                tag: Some(tag),
                dialect: Some(dialect),
                defect: None,
            },
            None => CurrentRow {
                raw: row,
                tag: None,
                dialect: Some(dialect),
                defect: Some(RowDefect::NoCounterpart),
            },
        };
    }
    CurrentRow {
        raw: row,
        tag: None,
        dialect: None,
        defect: Some(RowDefect::UnknownTag(type_name)),
    }
}

/// 旧方言的对应物。`None` 表示当前词汇没有对应物。
fn restore_dialect(row: &Value, dialect: RowDialect) -> Option<RowTag> {
    match dialect {
        RowDialect::UserInput => {
            // 文本或附件引用，得有一样：只带附件的一轮（用户只发了
            // 文件）也是一轮，浏览器今天为它画一个带 chip 的气泡。
            // 对应物不取决于引用解不解得动——解不动只影响类型化视
            // 图，不许把整轮连同提示词一起拖下水。
            if user_input_text(row).is_none() && !row_has_attachment_refs(row) {
                return None;
            }
            Some(RowTag::UserMessage)
        }
        RowDialect::Model => match model_part(row)? {
            ContentPart::ToolResult(_) => Some(RowTag::ToolResult),
            ContentPart::Text(_)
            | ContentPart::ToolUse(_)
            | ContentPart::Reasoning(_) => Some(RowTag::AssistantMessage),
            // 图片、音频与资源没有当前对应物。
            _ => None,
        },
        RowDialect::Message => match dialect_message_of(row)?.role {
            Role::User | Role::System => Some(RowTag::UserMessage),
            Role::Assistant => Some(RowTag::AssistantMessage),
            Role::Tool => Some(RowTag::ToolResult),
        },
        RowDialect::Agent => None,
    }
}

#[derive(Debug, Clone, Copy)]
/// 一份日志，还原过。
///
/// 构造它就是那道 fail-closed 检查：日志里有一行类型当前构建不认识，
/// 而那一行没有「可忽略」标记，构造就失败。于是「日志读不了」这件事
/// 不可能被忘记——拿不到 [`Self`]，因为它没被造出来。
pub struct Rows<'a> {
    rows: &'a [Value],
}

impl<'a> Rows<'a> {
    /// 读一份日志。
    ///
    /// 失败当且仅当日志里有一行，类型当前构建不认识，而那一行没有
    /// 「可忽略」标记。没有类型的那一行（元数据头行）是个别缺陷，
    /// 不是拒读的理由。
    pub fn read(rows: &'a [Value]) -> Result<Self, UnreadableLog> {
        for (index, row) in rows.iter().enumerate() {
            let restored = restore_row(row);
            if let Some(RowDefect::UnknownTag(tag)) = restored.defect()
                && !is_ignorable(row)
            {
                return Err(UnreadableLog {
                    index,
                    tag: tag.to_string(),
                });
            }
        }
        Ok(Self { rows })
    }

    /// 每一行，按日志顺序。
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = CurrentRow<'a>> + 'a {
        self.rows.iter().map(restore_row)
    }

    /// 还原成 `tag` 的每一行，按日志顺序。
    ///
    /// 标签先比、载荷后解：被标签排除的行不解码。
    pub fn of(&self, tag: RowTag) -> impl Iterator<Item = CurrentRow<'a>> + 'a {
        self.iter().filter(move |row| row.tag() == Some(tag))
    }

    /// 还原成 `tag` 的最后一行——从尾往前扫，所以坏掉的尾行是个缺陷，
    /// 不是一个更新的答案。
    pub fn last_of(&self, tag: RowTag) -> Option<CurrentRow<'a>> {
        self.iter().rev().find(|row| row.tag() == Some(tag))
    }
}

/// 这一行带着「可忽略」标记吗。
///
/// 标记是行的顶层键（`type` 的兄弟），由写入者为**可忽略**的行盖上。
/// 不认识这一行的类型当前构建于是能安全跳过它，而不是拒读整份日志。
fn is_ignorable(row: &Value) -> bool {
    row.get("ignorable").and_then(Value::as_bool) == Some(true)
}

impl<'a> CurrentRow<'a> {
    #[must_use]
    /// 这一行写下时的原样。
    pub const fn raw(&self) -> &'a Value {
        self.raw
    }

    #[must_use]
    /// 还原后的标签。`None` 当且仅当 [`Self::defect`] 有值。
    pub const fn tag(&self) -> Option<RowTag> {
        self.tag
    }

    #[must_use]
    /// 这一行还原成 `tag` 吗——不论它当初写的是当前词汇还是旧方言。
    pub fn restores_to(&self, tag: RowTag) -> bool {
        self.tag == Some(tag)
    }

    #[must_use]
    /// 这一行为何不是当前行。`None` 表示它是当前行。
    pub const fn defect(&self) -> Option<RowDefect<'a>> {
        self.defect
    }

    #[must_use]
    /// 这一行当初写的是当前词汇吗。
    ///
    /// 当前词汇写的行原样就是当前形状；旧方言的行由归一改写。读点要的
    /// 是改写后的那一行——这个判据告诉它哪一行不用改。
    pub const fn is_current(&self) -> bool {
        self.dialect.is_none() && self.tag.is_some()
    }

    #[must_use]
    /// 这一行的序号——它在会话日志里的身份。
    ///
    /// 旧方言的行不带序号；那种行在日志里的序号由位置决定，只有
    /// [`Rows`] 知道。
    pub fn seq(&self) -> Option<u64> {
        self.raw.get("seq").and_then(Value::as_u64)
    }

    #[must_use]
    /// 这一行进模型可见的表面吗。
    ///
    /// 读的是标签声明的角色，不是猜的。
    pub fn is_surface_row(&self) -> bool {
        self.tag.is_some_and(|tag| tag.surface_role().is_surface())
    }

    #[must_use]
    /// 这一行还原成当前形状的事件。
    ///
    /// 类型化的行解出来就是它自己；旧方言的行被改写成它的对应物——
    /// 载荷改写成当前形状，标签取还原后的标签。载荷解不出来时是
    /// `None`：那是个缺陷，不是方言。
    pub fn event(&self) -> Option<SessionEvent> {
        match self.dialect {
            None => {
                #[cfg(test)]
                DECODE_ATTEMPTS.with(|count| count.set(count.get() + 1));
                SessionEvent::from_value(self.raw)
            }
            Some(dialect) => self.dialect_event(dialect),
        }
    }

    #[must_use]
    /// 这一行投影成的模型消息。
    ///
    /// 表面上的每一行都有一条：用户轮、助手轮、工具结果、压缩摘要。
    /// 其余的行不投影成消息。
    pub fn message(&self) -> Option<Message> {
        match self.dialect {
            Some(dialect) => self.dialect_message(dialect),
            None => match self.tag? {
                RowTag::UserMessage
                | RowTag::AssistantMessage
                | RowTag::ToolResult => self.decode_message(),
                RowTag::Compaction => {
                    self.raw.get("data").and_then(compaction_summary_message)
                }
                _ => None,
            },
        }
    }

    /// 旧方言的行投影成的模型消息。
    fn dialect_message(&self, dialect: RowDialect) -> Option<Message> {
        match dialect {
            RowDialect::UserInput => {
                // 对应物的判据与 restore_dialect 同一条：文本或附件
                // 引用得有一样。只带附件的一轮没有文本，但仍然是
                // 一轮。
                if user_input_text(self.raw).is_none()
                    && !row_has_attachment_refs(self.raw)
                {
                    return None;
                }
                let text = user_input_text(self.raw).unwrap_or_default();
                Some(Message::user(text))
            }
            RowDialect::Model => match model_part(self.raw)? {
                ContentPart::ToolResult(result) => {
                    let call_id = result.tool_use_id.clone();
                    Some(Message {
                        role: Role::Tool,
                        content: Content::Single(ContentPart::ToolResult(
                            result,
                        )),
                        tool_call_id: Some(call_id),
                        ..Message::default()
                    })
                }
                part @ (ContentPart::Text(_)
                | ContentPart::ToolUse(_)
                | ContentPart::Reasoning(_)) => Some(Message {
                    role: Role::Assistant,
                    content: Content::Single(part),
                    ..Message::default()
                }),
                _ => None,
            },
            RowDialect::Message => dialect_message_of(self.raw),
            RowDialect::Agent => None,
        }
    }

    /// 旧方言的行改写成当前形状的事件。
    ///
    /// 行的外壳——序号、时间戳、来源、附件——取**原始行**自己的，
    /// 不是造出来的：还原后的行就是那一行，只是写法变了。没有序号
    /// 的旧日志仍然没有（`seq` 落到 0 是类型要求的回退，发布一行时
    /// 走的是 [`super::publish`]，不经这里）。
    fn dialect_event(&self, dialect: RowDialect) -> Option<SessionEvent> {
        let message = self.dialect_message(dialect)?;
        let data = serde_json::to_value(&message).ok()?;
        let op = row_op(self.raw);
        let seq = self.seq().unwrap_or(0);
        let ts = row_ts(self.raw).unwrap_or_default().to_string();
        let source = self
            .raw
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_string);
        match self.tag? {
            RowTag::UserMessage => Some(SessionEvent::UserMessage {
                seq,
                ts,
                data,
                source,
                surface_op: Some(op),
                attachments: user_input_attachments(self.raw),
            }),
            RowTag::AssistantMessage => Some(SessionEvent::AssistantMessage {
                seq,
                ts,
                data,
                surface_op: Some(op),
            }),
            RowTag::ToolResult => Some(SessionEvent::ToolResult {
                seq,
                ts,
                data,
                is_error: tool_result_failed(&message),
                tool_call_id: message.tool_call_id.clone(),
                changed: row_changed(self.raw),
                surface_op: Some(op),
            }),
            _ => None,
        }
    }

    /// 这一行的载荷解成的模型消息。
    ///
    /// 接受消息的完整线上形状，也接受手写的 `{"role": …, "content":
    /// […]}` 形状；两者都不是时，载荷里带文本就当一条用户轮，带调用
    /// id 就当一条工具结果。
    fn decode_message(&self) -> Option<Message> {
        decode_message(self.raw.get("data")?)
    }
}

/// 压缩行载荷里的摘要解成的助手轮。
///
/// `data.summary` 只有这一处读它：压缩行的载荷形状归行词汇管，
/// 读它的人（行投影、历史投影）都问这里。
pub(crate) fn compaction_summary_message(data: &Value) -> Option<Message> {
    data.get("summary")
        .and_then(Value::as_str)
        .map(Message::assistant)
}

/// 一条**折叠后的表面载荷**解成的模型消息。
///
/// 折叠后的载荷只有两种：消息行带的消息本身，或压缩行带的
/// `{"summary": …}`——模型把它看成替换掉那一批的助手轮。两者都
/// 不是时，这里给错误，而不是猜：历史投影是严格的，一条无法证明
/// 归属的载荷必须报错，不能被兑底成一条用户轮。
///
/// 这是折叠载荷的**唯一**解码入口：行词汇与
/// [`SessionEvent::message`](crate::events::SessionEvent::message)
/// 共用 [`decode_message`]，历史投影（[`crate::history`]）共用这里。
pub(crate) fn decode_folded_payload(
    data: &Value,
) -> Result<Message, serde_json::Error> {
    match serde_json::from_value::<Message>(data.clone()) {
        Ok(message) => Ok(message),
        Err(error) => match compaction_summary_message(data) {
            Some(message) => Ok(message),
            None => Err(error),
        },
    }
}

/// 一行的载荷解成的模型消息。
///
/// 接受消息的完整线上形状，也接受手写的 `{"role": …, "content":
/// […]}` 形状；两者都不是时，载荷里带文本就当一条用户轮，带调用
/// id 就当一条工具结果。
///
/// 行词汇与 [`SessionEvent::message`](crate::events::SessionEvent::message)
/// 共用这一套解码——一行带的是什么东西，只有这一处说了算。
pub(crate) fn decode_message(data: &Value) -> Option<Message> {
    if let Ok(message) = serde_json::from_value::<Message>(data.clone()) {
        return Some(message);
    }
    if let Some(message) = decode_loose_message(data) {
        return Some(message);
    }
    decode_fallback_message(data)
}

/// 手写的 `{"role": …, "content": […]}` 形状。
fn decode_loose_message(data: &Value) -> Option<Message> {
    let role = role_of(data.get("role").and_then(Value::as_str)?)?;
    let parts: Vec<ContentPart> = data
        .get("content")?
        .as_array()?
        .iter()
        .filter_map(|part| {
            serde_json::from_value::<ContentPart>(part.clone()).ok()
        })
        .collect();
    if parts.is_empty() {
        return None;
    }
    let mut key = data
        .get("tool_call_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if key.is_none() && role == Role::Tool {
        key = parts.iter().find_map(|part| match part {
            ContentPart::ToolResult(result) => Some(result.tool_use_id.clone()),
            _ => None,
        });
    }
    Some(Message {
        role,
        content: Content::parts(parts),
        tool_call_id: key,
        ..Message::default()
    })
}

/// 最后的归一：载荷不是消息，但还带着文本或调用 id。
fn decode_fallback_message(data: &Value) -> Option<Message> {
    if let Some(text) = data.get("text").and_then(Value::as_str) {
        return Some(Message::user(text));
    }
    let key = data
        .get("call_id")
        .or_else(|| data.get("tool_call_id"))
        .and_then(Value::as_str)?;
    Some(Message::tool(Content::text(data.to_string()), key))
}

/// 线上标签名说的角色。
fn role_of(role: &str) -> Option<Role> {
    match role {
        "user" => Some(Role::User),
        "assistant" => Some(Role::Assistant),
        "tool" => Some(Role::Tool),
        "system" => Some(Role::System),
        _ => None,
    }
}

/// `Model` 方言的载荷：一个 part。
fn model_part(row: &Value) -> Option<ContentPart> {
    let data = row.get("data")?;
    serde_json::from_value::<ContentPart>(data.clone()).ok()
}

/// `Message` 方言的载荷：整条消息，它的角色决定对应物。
///
/// 先按 provider 自己的编码解，解不动再按手写的 `{"role": …,
/// "content": […]}` 形状解——与当前行的解码同一套宽严：词汇对旧
/// 方言的载荷不能比对当前行的载荷更严，旧的写法只会更松。带文本
/// 的兜底（`decode_fallback_message`）**不**用：那会把一条没有角色
/// 的行变成用户轮，是今天两边都不渲染的内容。
fn dialect_message_of(row: &Value) -> Option<Message> {
    let data = row.get("data")?;
    if let Ok(message) = serde_json::from_value::<Message>(data.clone()) {
        return Some(message);
    }
    decode_loose_message(data)
}

/// `UserInput` 方言的载荷：这一轮的文字。
fn user_input_text(row: &Value) -> Option<&str> {
    row.get("data")
        .and_then(|data| data.get("text"))
        .and_then(Value::as_str)
}

/// 旧方言的载荷里放这一轮附件引用的位置。
///
/// 方言的提示行把引用放在文本旁边；当前的 `user_message` 行把它们
/// 放在行自己的顶层。旧引用的形状（`kind` / `name` / `mime_type` /
/// `byte_len`）就是可选成员全缺省的 `AttachmentRefRow`，所以逐条
/// `from_value` 解码对两个写入者产出的每一种引用都无损。这一路径
/// 只在这一处声明：类型化视图在这里读它，发布器（`super::publish`）
/// 从这里搬走它。
pub(crate) const DIALECT_ATTACHMENT_PATH: &str = "/data/attachments";

/// `UserInput` 方言的载荷里带的附件引用，逐条尽力解。
///
/// 一条解不动的引用只丢它自己，不牵连其余。
fn user_input_attachments(row: &Value) -> Vec<crate::events::AttachmentRefRow> {
    let Some(list) = row
        .pointer(DIALECT_ATTACHMENT_PATH)
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|entry| serde_json::from_value(entry.clone()).ok())
        .collect()
}

/// 这一行的旧载荷带没带附件引用。
///
/// 只看数组**在不在、非不非空**，不看解不解得动：对应物不许被一条
/// 古怪的引用拖垮。
fn row_has_attachment_refs(row: &Value) -> bool {
    row.pointer(DIALECT_ATTACHMENT_PATH)
        .and_then(Value::as_array)
        .is_some_and(|list| !list.is_empty())
}

/// 这条消息的工具结果说自己失败了吗。
///
/// 失败标志在 part 自己身上；行级 `is_error` 是它在外壳上的投影。
pub(crate) fn tool_result_failed(message: &Message) -> bool {
    matches!(
        &message.content,
        Content::Single(ContentPart::ToolResult(result))
            if result.is_error == Some(true)
    )
}

/// 这一行的时间戳。
fn row_ts(row: &Value) -> Option<&str> {
    row.get("ts").and_then(Value::as_str)
}

/// 这一行带的表面操作。
/// 这一行带的「这次调用改了哪些文件」。
fn row_changed(row: &Value) -> Option<Vec<String>> {
    let paths = row.get("changed")?.as_array()?;
    let paths: Vec<String> = paths
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    (!paths.is_empty()).then_some(paths)
}

fn row_op(row: &Value) -> SurfaceOp {
    row.get("surface_op")
        .and_then(|value| {
            serde_json::from_value::<SurfaceOp>(value.clone()).ok()
        })
        .unwrap_or_else(SurfaceOp::append)
}

#[cfg(test)]
thread_local! {
    /// How many rows this test thread handed to the decoder — the
    /// proof behind «compare the tag before decoding».
    ///
    /// Thread-local, not a process counter: the harness runs each
    /// test on its own thread, and every test that decodes would
    /// otherwise inflate the count the one measuring test reads.
    pub(crate) static DECODE_ATTEMPTS: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
}

#[cfg(test)]
mod tests;
