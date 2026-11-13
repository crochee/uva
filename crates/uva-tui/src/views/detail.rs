//! The generic detail: one JSON object flattened into a key/value
//! table, plus the write-confirmation modal.
//!
//! Any domain's `get` answer (or a bare list row, for domains without
//! a `get`) renders here without a per-domain screen: paths are
//! `/`-joined, nested objects and arrays recurse three levels deep,
//! and a session's long `history` is bounded by a row cap with an
//! explicit ellipsis row instead of a truncated view.
//!
//! The confirmation modal is the terminal's answer to "one keystroke,
//! irreversible": delete / run / tick / resume / compact ask before
//! sending, and the modal names the operation, the wire method and
//! the target.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Wrap},
};
use serde_json::Value;

use crate::{domains::DomainSpec, render::centered};

/// The flattening depth cap: a session's `history` nests rows →
/// data → content → parts, and three levels show enough.
const MAX_DEPTH: usize = 3;

/// The row cap: past this the view says how many entries it omitted.
const MAX_ROWS: usize = 200;

/// One object's flattened view.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DetailView {
    /// The object's id (the row the detail was opened from).
    pub id: String,
    /// The flattened key paths and their texts.
    pub rows: Vec<(String, String)>,
    /// The first row the viewport shows.
    pub scroll: usize,
}

impl DetailView {
    /// Flatten one object into key/value rows.
    #[must_use]
    pub fn from_value(id: &str, value: &Value) -> Self {
        let mut rows = Vec::new();
        let mut omitted = 0usize;
        flatten(value, "", 0, &mut rows, &mut omitted);
        if omitted > 0 {
            rows.push((String::new(), format!("<还有 {omitted} 项省略>")));
        }
        Self {
            id: id.to_string(),
            rows,
            scroll: 0,
        }
    }
}

/// Walk one value, pushing scalar leaves as `(path, text)`.
fn flatten(
    value: &Value,
    path: &str,
    depth: usize,
    rows: &mut Vec<(String, String)>,
    omitted: &mut usize,
) {
    if rows.len() >= MAX_ROWS {
        *omitted += 1;
        return;
    }
    match value {
        Value::Object(map) => {
            if depth >= MAX_DEPTH {
                rows.push((path.to_string(), format!("{{{} 项}}", map.len())));
                return;
            }
            if map.is_empty() {
                rows.push((path.to_string(), "{}".to_string()));
                return;
            }
            for (key, child) in map {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}/{key}")
                };
                flatten(child, &child_path, depth + 1, rows, omitted);
            }
        }
        Value::Array(items) => {
            if depth >= MAX_DEPTH {
                rows.push((path.to_string(), format!("[{} 项]", items.len())));
                return;
            }
            if items.is_empty() {
                rows.push((path.to_string(), "[]".to_string()));
                return;
            }
            for (index, child) in items.iter().enumerate() {
                let child_path = if path.is_empty() {
                    index.to_string()
                } else {
                    format!("{path}/{index}")
                };
                flatten(child, &child_path, depth + 1, rows, omitted);
            }
        }
        scalar => rows.push((path.to_string(), scalar_text(scalar))),
    }
}

/// One scalar's text.
fn scalar_text(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Array(_) | Value::Object(_) => String::new(),
    }
}

/// Draw the flattened table, scrolled to [`DetailView::scroll`].
pub fn draw_detail(
    frame: &mut Frame,
    area: Rect,
    view: &DetailView,
    spec: &DomainSpec,
) {
    let lines: Vec<Line<'static>> = view
        .rows
        .iter()
        .skip(view.scroll)
        .map(|(path, text)| {
            Line::from(vec![
                Span::styled(
                    format!("{path}: "),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw(text.clone()),
            ])
        })
        .collect();
    let title = if view.id.is_empty() {
        format!(" {} ", spec.label)
    } else {
        format!(" {} · {} ", spec.label, view.id)
    };
    let paragraph = Paragraph::new(lines)
        .block(
            Block::bordered()
                .title(title)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

/// The bottom hint: the domain's operations and the leaving keys.
pub fn draw_ops_footer(frame: &mut Frame, area: Rect, spec: &DomainSpec) {
    let mut spans = vec![Span::styled(
        " 操作: ",
        Style::default().fg(Color::DarkGray),
    )];
    if spec.ops.is_empty() {
        spans
            .push(Span::styled("（无）", Style::default().fg(Color::DarkGray)));
    } else {
        for (index, op) in spec.ops.iter().enumerate() {
            if index > 0 {
                spans.push(Span::styled(
                    " · ",
                    Style::default().fg(Color::DarkGray),
                ));
            }
            spans.push(Span::styled(
                op.label,
                Style::default().fg(Color::Yellow),
            ));
        }
    }
    spans.push(Span::styled(
        "    : 命令面板 · Esc 返回",
        Style::default().fg(Color::DarkGray),
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// A write operation awaiting confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirm {
    /// The operation's label (`删除`, `运行`, `json`, …).
    pub op_label: &'static str,
    /// The wire method it will call.
    pub method: &'static str,
    /// The object it targets (or a params summary).
    pub target: String,
}

/// The centred modal: what will run, against what, and the two keys.
pub fn draw_confirm(frame: &mut Frame, area: Rect, confirm: &Confirm) {
    let area = centered(area, 60, 8);
    frame.render_widget(Clear, area);
    let lines = vec![
        Line::from(Span::styled(
            "确认执行",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(format!("操作: {}", confirm.op_label)),
        Line::from(format!("方法: {}", confirm.method)),
        Line::from(format!("对象: {}", confirm.target)),
        Line::from(""),
        Line::from(vec![
            Span::styled("[y] 执行", Style::default().fg(Color::Green)),
            Span::raw("    "),
            Span::styled("[Esc] 取消", Style::default().fg(Color::Red)),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::bordered()
                    .title(" Confirm ")
                    .border_style(Style::default().fg(Color::Yellow)),
            )
            .wrap(Wrap { trim: true }),
        area,
    );
}
