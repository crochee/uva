//! Drawing only.
//!
//! [`draw`] reads [`App`] and paints it; it holds no state and performs
//! no I/O, which is what lets `ratatui::backend::TestBackend` assert
//! the screen without a tty. The region painters live in
//! [`crate::views`]; what stays here is the shell — the sidebar
//! column beside the main column — and the modal overlays:
//!
//! ```text//!
//! │ Workspaces │ uva  connected  session s1  idle
//! │ ▾ work (2) │ ┌ Conversation ─────────────────────────┐
//! │   ● s1     │ │            ◄ what is the answer?     │
//! │   ▸ s2     │ │ assistant Sunny with a light breeze. │
//! │ ⚙ 设置     │ └──────────────────────────────────────┘
//! │            │ ❯ /work
//! │            │ + / 工作区 / 模型 / 发送
//! ```
//!
//! An approval prompt is an overlay: the transcript stays readable
//! underneath, and the prompt names the tool, the risk and the
//! correlation id the answer must carry.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Wrap},
};

use crate::{
    app::{App, ApprovalPrompt, Overlay, Pane},
    ui_schema::{SLOT_CHAT_SIDEBAR, SLOT_SESSION_EXTENSIONS},
    views,
};

mod ui_slots;

/// Draw the whole projection.
pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let sidebar_width = if app.sidebar().collapsed {
        views::sidebar::RAIL
    } else {
        views::sidebar::WIDTH.min(area.width / 2)
    };
    let columns = Layout::horizontal([
        Constraint::Length(sidebar_width),
        Constraint::Min(0),
    ])
    .split(area);
    views::sidebar::draw(frame, columns[0], app);

    // The `session.detail.extensions` slot reserves rows only when it
    // (or the unplaced bucket) carries something: an empty tree keeps
    // the layout byte-identical to the one before the slot tree.
    let extensions = app.ui().components_in(SLOT_SESSION_EXTENSIONS);
    let extensions_visible =
        !extensions.is_empty() || !app.ui().unplaced_slots().is_empty();
    let rows = if extensions_visible {
        Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(ui_slots::extensions_height(&extensions)),
            Constraint::Length(views::todo::height(app)),
            Constraint::Length(views::composer::HEIGHT),
        ])
        .split(columns[1])
    } else {
        Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(views::todo::height(app)),
            Constraint::Length(views::composer::HEIGHT),
        ])
        .split(columns[1])
    };
    views::draw_status(frame, rows[0], app);
    if app.pane() == Pane::Catalog {
        views::catalog::draw_catalog(frame, rows[1], app.catalog());
    } else {
        // The `chat.sidebar` slot takes a right column of the
        // conversation row, and only when it carries components.
        let slot_components = app.ui().components_in(SLOT_CHAT_SIDEBAR);
        if slot_components.is_empty() {
            views::conversation::draw(frame, rows[1], app);
        } else {
            let middle = Layout::horizontal([
                Constraint::Min(40),
                Constraint::Length(34),
            ])
            .split(rows[1]);
            views::conversation::draw(frame, middle[0], app);
            ui_slots::draw_sidebar(frame, middle[1], &slot_components);
        }
        let (todo_area, composer_area) = if extensions_visible {
            ui_slots::draw_extensions(frame, rows[2], app);
            (rows[3], rows[4])
        } else {
            (rows[2], rows[3])
        };
        views::todo::draw(frame, todo_area, app);
        views::composer::draw(frame, composer_area, app);
    }

    match app.overlay() {
        Some(Overlay::Help) => draw_help(frame),
        Some(Overlay::CommandPalette) => draw_palette(frame),
        None => {}
    }

    if app.catalog().palette_open() {
        views::catalog::draw_palette(frame, frame.area(), app.catalog());
    }
    if let Some(confirm) = app.catalog().confirm.as_ref() {
        views::detail::draw_confirm(frame, frame.area(), confirm);
    }

    if let Some(prompt) = app.approval() {
        draw_approval(frame, prompt, app.approval_answered());
    }
}

/// The approval overlay.
fn draw_approval(frame: &mut Frame, prompt: &ApprovalPrompt, answered: bool) {
    let area = centered(frame.area(), 64, 9);
    frame.render_widget(Clear, area);
    let mut lines = vec![
        Line::from(Span::styled(
            "Approval required",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(format!("tool: {}", prompt.tool)),
        Line::from(format!("risk: {}", prompt.risk)),
        Line::from(format!("approval_id: {}", prompt.approval_id)),
        Line::from(""),
    ];
    if answered {
        lines.push(Line::from(Span::styled(
            "decision sent — waiting for the core",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        lines.push(Line::from(vec![
            Span::styled("[y] allow", Style::default().fg(Color::Green)),
            Span::raw("    "),
            Span::styled("[n] deny", Style::default().fg(Color::Red)),
        ]));
    }
    let paragraph = Paragraph::new(lines)
        .block(
            Block::bordered()
                .title(" Approval ")
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(paragraph, area);
}

/// A centred rectangle, clamped to the terminal.
pub(crate) fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width, height)
}

/// The keybinding help overlay (`?`).
fn draw_help(frame: &mut Frame) {
    let area = centered(frame.area(), 56, 11);
    frame.render_widget(Clear, area);
    let dim = Style::default().fg(Color::DarkGray);
    let lines = vec![
        Line::from(vec![
            Span::styled("列表  ", Style::default().fg(Color::Cyan)),
            Span::raw("j/k 下一个/上一个 · gg/G 首尾 · Ctrl+d/u 半页"),
        ]),
        Line::from(vec![
            Span::styled("搜索  ", Style::default().fg(Color::Cyan)),
            Span::raw("/ 搜索 · n/N 下一个/上一个 · Esc 取消"),
        ]),
        Line::from(vec![
            Span::styled("会话  ", Style::default().fg(Color::Cyan)),
            Span::raw("Enter 打开 · y 复制 id · x/dd 删除 · r 刷新"),
        ]),
        Line::from(vec![
            Span::styled("全局  ", Style::default().fg(Color::Cyan)),
            Span::raw("Ctrl+b 折叠侧栏 · Tab 设置 · Ctrl+c 退出"),
        ]),
        Line::from(vec![
            Span::styled("面板  ", Style::default().fg(Color::Cyan)),
            Span::raw(": / Ctrl+k 命令面板 · ? 帮助"),
        ]),
        Line::from(vec![
            Span::styled("输入  ", Style::default().fg(Color::Cyan)),
            Span::raw("i 聚焦输入框 · Enter 发送 · Esc 回列表"),
        ]),
        Line::from(""),
        Line::from(Span::styled("Esc 关闭", dim)),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" 键位 ")),
        area,
    );
}

/// The command palette placeholder (`:` / `Ctrl+k`).
fn draw_palette(frame: &mut Frame) {
    let area = centered(frame.area(), 44, 5);
    frame.render_widget(Clear, area);
    let lines = vec![
        Line::from(vec![
            Span::styled(": ", Style::default().fg(Color::Yellow)),
            Span::styled(
                "命令面板（占位）",
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Esc 关闭",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" 命令面板 ")),
        area,
    );
}

#[cfg(test)]
mod tests;
