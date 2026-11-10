//! The todo progress block: `n/m` plus one line per item.
//!
//! The block exists only while the turn holds todos — [`height`]
//! answers zero otherwise, so the layout above the composer reserves
//! no row for it.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::{App, TodoStatus};

/// The block's height: the summary line plus one line per item, or
/// zero when there are no todos.
#[must_use]
pub fn height(app: &App) -> u16 {
    if app.todo().is_empty() {
        return 0;
    }
    u16::try_from(app.todo().total() + 1).unwrap_or(u16::MAX)
}

/// Draw the todo block — only ever called on a non-zero area.
pub fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let todo = app.todo();
    let mut lines = vec![Line::from(Span::styled(
        format!("待办 {}/{}", todo.done(), todo.total()),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    ))];
    for item in todo.items() {
        let marker = match item.status {
            TodoStatus::Done => "✔",
            TodoStatus::Active => "▸",
            TodoStatus::Pending => "·",
        };
        lines.push(Line::from(format!("  {marker} {}", item.content)));
    }
    frame.render_widget(Paragraph::new(lines), area);
}
