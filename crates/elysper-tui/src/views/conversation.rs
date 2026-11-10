//! The conversation flow: user messages as right-aligned bubbles,
//! the thinking block in its two states, the assistant's streamed
//! answer.
//!
//! The thinking block is the §6.1 contract: while the turn runs, its
//! reasoning renders expanded (a heading plus one dim line per
//! paragraph, so the operator can watch the model think); once the
//! turn ends it folds itself into the one-line summary `已思考 >`.
//! The projection keeps the reasoning text until the next turn
//! starts, which is what makes the folded state possible without a
//! durable row to read back.

use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, Wrap},
};

use crate::app::{App, Role, TranscriptLine, TurnState};

/// Draw the transcript, the thinking block and the live answer.
pub fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    for message in app.messages() {
        lines.push(transcript_line(message));
    }
    lines.extend(thinking_lines(app));
    if !app.live_text().is_empty() {
        lines.push(Line::from(vec![
            Span::styled(
                "assistant ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(app.live_text().to_string()),
        ]));
    }
    let paragraph = Paragraph::new(lines)
        .block(Block::bordered().title(" Conversation "))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

/// One durable line. The user's line is a right-aligned bubble (a
/// background block hugging the right edge); every other role keeps
/// its coloured label.
fn transcript_line(message: &TranscriptLine) -> Line<'static> {
    if message.role == Role::User {
        return Line::from(Span::styled(
            message.text.clone(),
            Style::default().fg(Color::White).bg(Color::Blue),
        ))
        .alignment(Alignment::Right);
    }
    let (label, color) = match message.role {
        Role::User => ("you", Color::Cyan),
        Role::Assistant => ("assistant", Color::Green),
        Role::Tool => ("tool", Color::Magenta),
        Role::System => ("system", Color::DarkGray),
    };
    Line::from(vec![
        Span::styled(
            format!("{label} "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::raw(message.text.clone()),
    ])
}

/// The thinking block's lines for the current state: expanded while
/// the turn runs, folded to one line afterwards, nothing when the
/// turn never thought.
fn thinking_lines(app: &App) -> Vec<Line<'static>> {
    if app.reasoning().is_empty() {
        return Vec::new();
    }
    let dim = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::ITALIC);
    if app.turn() == TurnState::Running {
        let mut lines = vec![Line::from(Span::styled("思考中", dim))];
        lines.extend(
            app.reasoning()
                .lines()
                .filter(|line| !line.is_empty())
                .map(|line| Line::from(Span::styled(format!("▎ {line}"), dim))),
        );
        lines
    } else {
        vec![Line::from(Span::styled("已思考 >", dim))]
    }
}
