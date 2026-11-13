//! The sidebar: the workspace tree, foldable into an icon rail
//! (`Ctrl+b`), or the settings navigation when the settings tab is
//! open.
//!
//! One gateway serves one workspace, so the tree is one group per
//! [`WorkspaceNode`](crate::app::WorkspaceNode) — the connection's
//! own workspace listing — with its sessions under it. The followed
//! session is always visible: when the listing has not caught up
//! with a freshly created session, the view appends it.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};

use crate::app::{App, Pane, SidebarTab};

/// The expanded sidebar's width.
pub const WIDTH: u16 = 24;

/// The folded rail's width.
pub const RAIL: u16 = 3;

/// Draw the sidebar into `area` — the whole column when expanded, a
/// three-cell icon rail when folded.
pub fn draw(frame: &mut Frame, area: Rect, app: &App) {
    if app.sidebar().collapsed {
        draw_rail(frame, area, app);
        return;
    }
    if app.pane() == Pane::Catalog {
        draw_domains(frame, area, app);
        return;
    }
    match app.sidebar().tab {
        SidebarTab::Sessions => draw_tree(frame, area, app),
        SidebarTab::Settings => super::settings::draw(frame, area, app),
    }
}

/// The workspace tree: a group heading per workspace, its sessions
/// indented under it, the settings entry pinned at the bottom.
fn draw_tree(frame: &mut Frame, area: Rect, app: &App) {
    let selected = app.sidebar().cursor;
    let current = app.session_id();
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut row = 0usize;
    let mut listed_current = false;
    for node in app.workspaces() {
        lines.push(Line::from(Span::styled(
            format!("▾ {} ({}) 🔍", node.name, node.sessions.len()),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        for entry in &node.sessions {
            if Some(entry.id.as_str()) == current {
                listed_current = true;
            }
            lines.push(session_line(
                entry,
                Some(row) == Some(selected),
                current,
            ));
            row += 1;
        }
    }
    if let Some(current) = current
        && !listed_current
    {
        // A freshly created session the listing has not returned yet.
        lines.push(Line::from(Span::styled(
            format!("  ● {current}"),
            Style::default().fg(Color::Green),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "▾ 目录 (o)",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    for spec in crate::domains::DOMAINS {
        lines.push(Line::from(Span::styled(
            format!("  ▸ {}", spec.label),
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "⚙ 设置 (Tab)",
        Style::default().fg(Color::DarkGray),
    )));
    let paragraph = Paragraph::new(lines).block(
        Block::bordered()
            .title(" Workspaces ")
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(paragraph, area);
}

/// One session row: the current session is `●`, others `▸`; the
/// selected row renders reversed.
fn session_line(
    entry: &crate::app::SessionEntry,
    is_selected: bool,
    current: Option<&str>,
) -> Line<'static> {
    let is_current = Some(entry.id.as_str()) == current;
    let marker = if is_current { "●" } else { "▸" };
    let mut style = if is_current {
        Style::default().fg(Color::Green)
    } else {
        Style::default()
    };
    if is_selected {
        style = style.add_modifier(Modifier::REVERSED);
    }
    let text = if entry.status.is_empty() {
        format!("  {marker} {}", entry.id)
    } else {
        format!("  {marker} {} · {}", entry.id, entry.status)
    };
    Line::from(Span::styled(text, style))
}

/// The folded rail: the brand, one glyph per workspace, the settings
/// glyph — nothing else fits, by design.
fn draw_rail(frame: &mut Frame, area: Rect, app: &App) {
    let selected = app.sidebar().cursor;
    let mut lines: Vec<Line<'static>> = vec![Line::from(Span::styled(
        " E",
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ))];
    let mut row = 0usize;
    for node in app.workspaces() {
        let glyph = node.name.chars().next().unwrap_or('·').to_uppercase();
        let mut style = Style::default().fg(Color::Cyan);
        if Some(row) == Some(selected) {
            style = style.add_modifier(Modifier::REVERSED);
        }
        lines.push(Line::from(Span::styled(format!(" {glyph}"), style)));
        row += node.sessions.len().max(1);
    }
    if let Some(glyph) = app.session_id().and_then(|id| id.chars().next()) {
        lines.push(Line::from(Span::styled(
            format!(" {glyph}"),
            Style::default().fg(Color::Green),
        )));
    }
    lines.push(Line::from(Span::styled(
        " ⚙",
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(Paragraph::new(lines), area);
}

/// The directory list: every connected domain, the open one marked.
/// The workspace tree's "目录" group is the same list in miniature —
/// this is the focused view you get after `o`.
fn draw_domains(frame: &mut Frame, area: Rect, app: &App) {
    let current = app.catalog().domain.map(|spec| spec.list_method);
    let mut lines: Vec<Line<'static>> = vec![Line::from(Span::styled(
        "▾ 目录 (Tab 切换)",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ))];
    for spec in crate::domains::DOMAINS {
        let active = Some(spec.list_method) == current;
        let marker = if active { '●' } else { '▸' };
        let style = if active {
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(
            format!("  {marker} {}", spec.label),
            style,
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Esc 返回对话",
        Style::default().fg(Color::DarkGray),
    )));
    let paragraph = Paragraph::new(lines).block(
        Block::bordered()
            .title(" 目录 ")
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(paragraph, area);
}
