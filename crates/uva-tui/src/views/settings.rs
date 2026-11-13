//! The settings navigation: the section list the sidebar switches to.
//!
//! The sections themselves are the reducer's data
//! ([`SETTINGS_SECTIONS`] — §6.1's
//! settings table, one row per section); this view only renders them,
//! with the selected row reversed. The section pages are the SPA's
//! to build; the terminal skeleton stops at the navigation.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};

use crate::app::{App, SETTINGS_SECTIONS};

/// Draw the settings navigation list.
pub fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let selected = app.sidebar().cursor;
    let lines: Vec<Line<'static>> = SETTINGS_SECTIONS
        .iter()
        .enumerate()
        .map(|(index, section)| {
            let mut style = Style::default().fg(Color::Cyan);
            if index == selected {
                style = style.add_modifier(Modifier::REVERSED);
            }
            Line::from(Span::styled(format!("  {section}"), style))
        })
        .collect();
    let paragraph = Paragraph::new(lines).block(
        Block::bordered()
            .title(" Settings ")
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(paragraph, area);
}
