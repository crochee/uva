//! The §6.1 skeleton views: pure drawing functions, one file per
//! region of the screen.
//!
//! Every function here takes the area it owns and the [`App`] it
//! reads, holds no state and performs no I/O — the same properties
//! [`crate::render`] has always had, split by region so the skeleton
//! (sidebar · conversation · todo · composer · settings) can grow one
//! piece at a time. The shell layout lives in [`crate::render::draw`]:
//! a sidebar column beside a main column of status, conversation,
//! todos and composer.
//!
//! - [`sidebar`] — the workspace tree (foldable into an icon rail)
//!   or the settings navigation;
//! - [`list`] — the generic directory table (one domain's columns,
//!   D30's list motions, keyless pagination);
//! - [`detail`] — the generic key/value detail and the write
//!   confirmation modal;
//! - [`catalog`] — the directory browser's state machine (domain →
//!   list → detail), the command palette and the `:json` gate;
//! - [`conversation`] — right-aligned user bubbles, the thinking
//!   block's two states, the assistant stream;
//! - [`mod@todo`] — the todo progress block, absent when there are no
//!   todos;
//! - [`composer`] — the bash-prompted input with its footer hints;
//! - [`settings`] — the settings sections list the sidebar switches
//!   to;
//! - [`draw_status`] — the one-line header (connection, session,
//!   turn), unchanged in content.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::{App, ConnectionState, TurnState};

pub mod catalog;
pub mod composer;
pub mod conversation;
pub mod detail;
pub mod list;
pub mod settings;
pub mod sidebar;
pub mod todo;

#[cfg(test)]
mod tests;

/// The one-line header: connection, session and turn.
pub fn draw_status(frame: &mut Frame, area: Rect, app: &App) {
    let connection = match app.connection() {
        ConnectionState::Starting => "starting".to_string(),
        ConnectionState::Ready { .. } => "connected".to_string(),
        ConnectionState::Failed(reason) => format!("offline: {reason}"),
    };
    let session = app.session_id().unwrap_or("no session");
    let turn = match app.turn() {
        TurnState::Idle => "idle",
        TurnState::Running => "running",
        TurnState::AwaitingApproval => "awaiting approval",
    };
    let line = Line::from(vec![
        Span::styled(
            " elysper ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            " {connection}  session {session}  {turn}  ·  {}",
            app.status()
        )),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}
