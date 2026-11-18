//! 发布 —— 把一行按当前词汇的形状交出去。
//!
//! 归一（[`super::restore`]）回答「这一行**是**什么」；发布回答「读者
//! **见到**什么」。两者是同一件事的两面：归一发生在读边界，发布发
//! 生在上线边界，而读者只在线上边界之外存在——浏览器、TUI、任何持
//! 有 `sessions/get` 的 `history` 或 `follow` 的行的人。
//!
//! 发布的行**永远**以当前词汇拼写：`type` 是 33 个标签之一，载荷是
//! 当前形状。于是读者不需要认识方言——浏览器的那张方言分派表随这一
//! 刀退场，下一次新增方言式的形状变化也只改词汇，不改任何读者。
//!
//! # 规则：原始行，只在词汇说它不对的地方改
//!
//! 写入者把 `SessionEvent` 序列化后才落盘，所以**当前行本身就是日志
//! 行**：当前行原样发布，一个字节不动、一个字节不拷贝。旧方言的行
//! 不是当前形状，发布的行是它**改写后的对应物**——改 `type` 与
//! `data`，抬附件引用，补工具结果的失败标志与调用 id；其余成员
//! （`seq`、`ts`、`source`、`call_id`、`ignorable`……）连同它们的
//! **缺席**一起，都是日志行自己的。不重序列化事件：`SessionEvent`
//! 的 `seq` 是必填的 `u64`，重序列化表达不了「这一行没有序号」，只
//! 会捏造一个 0。
//!
//! # 发布集
//!
//! 发布的行恰是词汇**叫得出名字**的行（[`CurrentRow::tag`] 有值）。
//! 没有当前形式的行不发布：没有对应物的方言（`Agent`——委派子会话
//! 现在有自己的日志）、本构建不认识的标签、没有可用类型的行。它们
//! 不是被藏起来——`sessions/events` 的 `legacy` / `unknown` 桶继续
//! 原样报告它们；只是它们没有当前形状可上线路。
//!
//! 拒读（[`super::restore::Rows::read`]）不属于这里：那是「从日志重
//! 建会话」的读者的检查，转录与报告路由有意不做它。

use serde_json::Value;

use super::{
    RowTag,
    restore::{DIALECT_ATTACHMENT_PATH, restore_row, tool_result_failed},
};

#[must_use]
/// 一行的当前形状，或什么都不发布。
/// 当前行原样发布（行被移动，不拷贝）；旧方言的行按本模块文档
/// 的规则改写；没有当前形式的行返回 `None`。
/// 载荷解不出的**当前行**仍然发布：它的名字是当前的，只是载荷坏了
/// ——丢掉它才是丢转录内容。
pub fn publish_row(row: Value) -> Option<Value> {
    let restored = restore_row(&row);
    let tag = restored.tag()?;
    let is_current = restored.is_current();
    if is_current {
        return Some(row);
    }
    publish_restored(row, tag)
}

#[must_use]
/// 一份日志的行，按当前形状、按日志顺序发布。
///
/// 没有当前形式的行被略去；其余的行被移动，不拷贝。
pub fn publish_rows(rows: Vec<Value>) -> Vec<Value> {
    rows.into_iter().filter_map(publish_row).collect()
}

/// 一个旧方言行的当前形状：原始行，改词汇说它不对的地方。
fn publish_restored(row: Value, tag: RowTag) -> Option<Value> {
    let mut row = row;
    let message = restore_row(&row).message()?;
    let payload = serde_json::to_value(&message).ok()?;
    // 附件引用住在旧载荷里，得先搬出来——旧 `data` 马上要被替换。
    let refs = row.pointer_mut(DIALECT_ATTACHMENT_PATH).map(std::mem::take);
    let object = row.as_object_mut()?;
    object.insert("type".into(), Value::String(tag.as_wire().to_string()));
    object.insert("data".into(), payload);
    if let Some(refs) = refs
        && refs.as_array().is_some_and(|list| !list.is_empty())
    {
        // 日志自己的引用，逐字节原样：线上的 `attachments` 因此
        // 是新旧两种形状的并集，浏览器那个宽容的读法两边都认。
        object.insert("attachments".into(), refs);
    }
    if tag == RowTag::ToolResult {
        // 失败标志与调用 id 在当前形状里住在行顶层（part 自己的副
        // 本仍在载荷里）。失败只在为真时写——当前行的写法如此。
        if tool_result_failed(&message) {
            object.insert("is_error".into(), Value::Bool(true));
        }
        if let Some(call_id) = &message.tool_call_id {
            object
                .insert("tool_call_id".into(), Value::String(call_id.clone()));
        }
    }
    Some(row)
}

#[cfg(test)]
mod tests;
