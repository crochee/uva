//! The terminal's renderers for the deployment's UI tree.
//!
//! The kind registry mirrors the browser's `kinds.tsx` in *content*
//! only (D18: each host owns its presentation; the wire is what they
//! share). An unknown kind renders one `? <kind>` placeholder line
//! rather than dropping the component — the same stance as the
//! browser's `UnknownKind`.
//!
//! Slots are placed by [`crate::render::draw`]: `chat.sidebar` becomes
//! a column beside the conversation, `session.detail.extensions` a
//! block under the transcript. Every other slot a deployment declares
//! lands in a one-line summary inside that block.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, Wrap},
};

use crate::{
    app::App,
    ui_schema::{SLOT_CHAT_SIDEBAR, SLOT_SESSION_EXTENSIONS, UiComponent},
};

/// Read an optional string prop.
fn text<'a>(props: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    props.get(key).and_then(serde_json::Value::as_str)
}

/// The string items of a `props.items` array.
fn items(props: &serde_json::Value) -> Vec<&str> {
    props
        .get("items")
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect()
        })
        .unwrap_or_default()
}

/// One line per component, in registration order.
pub(crate) fn component_lines(component: &UiComponent) -> Vec<Line<'static>> {
    match component.kind.as_str() {
        "panel" => {
            let mut lines = Vec::new();
            if let Some(title) = text(&component.props, "title") {
                lines.push(Line::from(Span::styled(
                    title.to_string(),
                    Style::default().add_modifier(Modifier::BOLD),
                )));
            }
            if let Some(body) = text(&component.props, "body") {
                lines.push(Line::from(body.to_string()));
            }
            for item in items(&component.props) {
                lines.push(Line::from(format!("• {item}")));
            }
            lines
        }
        "list" => {
            let mut lines = Vec::new();
            if let Some(title) = text(&component.props, "title") {
                lines.push(Line::from(Span::styled(
                    title.to_string(),
                    Style::default().add_modifier(Modifier::BOLD),
                )));
            }
            for item in items(&component.props) {
                lines.push(Line::from(format!("• {item}")));
            }
            lines
        }
        "key-value" => {
            let mut lines = Vec::new();
            if let Some(title) = text(&component.props, "title") {
                lines.push(Line::from(Span::styled(
                    title.to_string(),
                    Style::default().add_modifier(Modifier::BOLD),
                )));
            }
            if let Some(entries) = component
                .props
                .get("entries")
                .and_then(serde_json::Value::as_object)
            {
                for (key, value) in entries {
                    let value = match value.as_str() {
                        Some(text) => text.to_owned(),
                        None => value.to_string(),
                    };
                    lines.push(Line::from(format!("{key}: {value}")));
                }
            }
            lines
        }
        other => vec![Line::from(Span::styled(
            format!("? {other}"),
            Style::default().fg(Color::DarkGray),
        ))],
    }
}

/// Every component's lines, stacked.
fn stack(components: &[&UiComponent]) -> Vec<Line<'static>> {
    components
        .iter()
        .flat_map(|component| component_lines(component))
        .collect()
}

/// The `chat.sidebar` column: a bordered block titled by the slot id.
pub(crate) fn draw_sidebar(
    frame: &mut Frame,
    area: Rect,
    components: &[&UiComponent],
) {
    let paragraph = Paragraph::new(stack(components))
        .block(Block::bordered().title(format!(" {SLOT_CHAT_SIDEBAR} ")))
        .wrap(Wrap { trim: true });
    frame.render_widget(paragraph, area);
}

/// The extensions block's height: the components' lines plus the
/// borders and the one-line unplaced summary, capped at eight lines.
pub(crate) fn extensions_height(components: &[&UiComponent]) -> u16 {
    let lines: usize = components
        .iter()
        .map(|component| component_lines(component).len())
        .sum();
    u16::try_from(lines.saturating_add(3).min(8)).unwrap_or(8)
}

/// The `session.detail.extensions` block plus the unplaced summary.
pub(crate) fn draw_extensions(frame: &mut Frame, area: Rect, app: &App) {
    let components = app.ui().components_in(SLOT_SESSION_EXTENSIONS);
    let mut lines = stack(&components);
    let unplaced = app.ui().unplaced_slots();
    if !unplaced.is_empty() {
        // Declared somewhere this build does not render: say so in one
        // line instead of hiding the components entirely.
        lines.push(Line::from(Span::styled(
            format!("unplaced: {}", unplaced.join(", ")),
            Style::default().fg(Color::DarkGray),
        )));
    }
    let paragraph = Paragraph::new(lines)
        .block(Block::bordered().title(format!(" {SLOT_SESSION_EXTENSIONS} ")))
        .wrap(Wrap { trim: true });
    frame.render_widget(paragraph, area);
}
