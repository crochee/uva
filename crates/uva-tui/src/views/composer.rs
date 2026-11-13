//! The composer: a bash-styled prompt (`❯ <workspace> `) with the
//! input on one row and the footer hints on the next.
//!
//! The prompt decoration is §6.1's contract — the workspace path
//! grounds the operator in which tree the conversation edits. The
//! footer names the affordances the skeleton ships: `+` / 工作区 /
//! 模型 / 发送.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};

use crate::app::App;

/// The composer's height: two bordered rows, the input and the
/// footer.
pub const HEIGHT: u16 = 4;

/// Draw the composer.
pub fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::bordered().title(" Message ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)])
        .split(inner);

    let mut prompt =
        vec![Span::styled("❯ ", Style::default().fg(Color::Yellow))];
    if let Some(root) = app.workspace_root() {
        prompt.push(Span::styled(
            format!("{root} "),
            Style::default().fg(Color::Cyan),
        ));
    }
    if app.composer().is_empty() {
        prompt.push(Span::styled(
            "type a message",
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        prompt.push(Span::raw(app.composer().to_string()));
    }
    frame.render_widget(Paragraph::new(Line::from(prompt)), rows[0]);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "+ / 工作区 / 模型 / 发送",
            Style::default().fg(Color::DarkGray),
        ))),
        rows[1],
    );
}
