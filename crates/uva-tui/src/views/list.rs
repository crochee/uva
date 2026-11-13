//! The generic list: a table driven by one domain's column table.
//!
//! [`ListView`] holds the rows a listing returned (raw JSON; the
//! columns extract at draw time), the focus cursor, the viewport
//! offset and the `/` filter. Its keys are D30's list motions — the
//! verbs that need context (`Enter`, `y`, `x`, `Esc`, `:`) belong to
//! [`crate::views::catalog`], which owns the list.
//!
//! Pagination is deliberately keyless: the cursor is an opaque
//! server token, so moving past the last loaded row asks the owner
//! for the next page rather than inventing a "page down" binding.

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Cell, Paragraph, Row, Table},
};
use serde_json::Value;

use crate::domains::{DomainSpec, ListPage, cell_text};

/// The listing's UI state.
#[derive(Debug, Default)]
pub struct ListView {
    /// The loaded rows, in server order (pages append).
    pub rows: Vec<Value>,
    /// The focused row's index into [`Self::rows`].
    pub cursor: usize,
    /// The first row the viewport shows.
    pub offset: usize,
    /// The active `/` query, if any.
    pub filter: Option<String>,
    /// The matching row indices while a filter is active.
    pub matches: Vec<usize>,
    /// The position inside [`Self::matches`] (`n` / `N` move it).
    pub match_at: usize,
    /// The next-page token, when the server sent one.
    pub next_cursor: Option<String>,
    /// How many rows the whole listing holds, when reported.
    pub total: Option<u64>,
    /// Whether a page request is in flight.
    pub loading: bool,
    /// The last failure, rendered on the status line.
    pub error: Option<String>,
    /// `g` waits for its partner.
    pending_g: bool,
    /// `d` waits for its partner.
    pending_d: bool,
    /// Set when `dd` completed; the owner takes it via
    /// [`Self::take_double_d`].
    double_d: bool,
    /// Whether the filter is still being typed.
    filter_typing: bool,
}

impl ListView {
    /// An empty list.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The visible row indices: every row without a filter, the
    /// matches with one.
    #[must_use]
    pub fn visible(&self) -> Vec<usize> {
        if self.filtered() {
            self.matches.clone()
        } else {
            (0..self.rows.len()).collect()
        }
    }

    /// Whether a non-empty filter is active.
    fn filtered(&self) -> bool {
        self.filter
            .as_deref()
            .is_some_and(|filter| !filter.is_empty())
    }

    /// Whether the filter is still being typed.
    #[must_use]
    pub fn filter_typing(&self) -> bool {
        self.filter_typing
    }

    /// Fold one page in. `append` extends the rows (a next-page
    /// answer); otherwise the page replaces the listing and the
    /// cursor returns to the top.
    pub fn set_page(&mut self, page: ListPage, append: bool) {
        if append {
            self.rows.extend(page.rows);
        } else {
            self.rows = page.rows;
            self.cursor = 0;
            self.offset = 0;
            self.filter = None;
            self.filter_typing = false;
            self.matches.clear();
            self.match_at = 0;
        }
        self.next_cursor = page.next_cursor;
        self.total = page.total;
        self.loading = false;
        self.error = None;
        self.recompute_matches();
        self.clamp_cursor();
    }

    /// Record a failed list request.
    pub fn set_error(&mut self, message: impl Into<String>) {
        self.loading = false;
        self.error = Some(message.into());
    }

    /// D30's list keys. Returns true when the key was consumed.
    ///
    /// `Enter` / `y` / `x` / `Esc` / `:` are never consumed here —
    /// they need the owner's context.
    pub fn key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        viewport: u16,
    ) -> bool {
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        if self.filter_typing {
            self.filter_key(code, ctrl);
            return true;
        }
        if self.pending_g {
            self.pending_g = false;
            if code == KeyCode::Char('g') {
                self.jump_to_edge(false);
                self.scroll_to_cursor(viewport);
            }
            return true;
        }
        if self.pending_d {
            self.pending_d = false;
            if code == KeyCode::Char('d') {
                self.double_d = true;
            }
            return true;
        }
        if ctrl {
            return match code {
                KeyCode::Char('d') => {
                    self.step(self.half_page(viewport));
                    self.scroll_to_cursor(viewport);
                    true
                }
                KeyCode::Char('u') => {
                    self.step_back(self.half_page(viewport));
                    self.scroll_to_cursor(viewport);
                    true
                }
                _ => false,
            };
        }
        match code {
            KeyCode::Char('j') => {
                self.step(1);
                self.scroll_to_cursor(viewport);
                true
            }
            KeyCode::Char('k') => {
                self.step_back(1);
                self.scroll_to_cursor(viewport);
                true
            }
            KeyCode::Char('g') => {
                self.pending_g = true;
                true
            }
            KeyCode::Char('G') => {
                self.jump_to_edge(true);
                self.scroll_to_cursor(viewport);
                true
            }
            KeyCode::Char('/') => {
                self.filter = Some(String::new());
                self.filter_typing = true;
                self.matches.clear();
                self.match_at = 0;
                true
            }
            KeyCode::Char('n') => {
                self.cycle_match(true);
                self.scroll_to_cursor(viewport);
                true
            }
            KeyCode::Char('N') => {
                self.cycle_match(false);
                self.scroll_to_cursor(viewport);
                true
            }
            KeyCode::Char('d') => {
                self.pending_d = true;
                true
            }
            _ => false,
        }
    }

    /// Take the "`dd` completed" signal.
    pub fn take_double_d(&mut self) -> bool {
        std::mem::take(&mut self.double_d)
    }

    /// Scroll the viewport so the cursor is inside it.
    pub fn scroll_to_cursor(&mut self, viewport: u16) {
        if viewport == 0 {
            return;
        }
        let viewport = usize::from(viewport);
        if self.cursor < self.offset {
            self.offset = self.cursor;
        } else if self.cursor >= self.offset + viewport {
            self.offset = self.cursor + 1 - viewport;
        }
        let max_offset = self.rows.len().saturating_sub(viewport);
        self.offset = self.offset.min(max_offset);
    }

    /// The focused row's id, per the spec's id pointer.
    #[must_use]
    pub fn focused_id(&self, spec: &DomainSpec) -> Option<String> {
        if spec.id_pointer.is_empty() {
            return None;
        }
        self.rows
            .get(self.cursor)
            .and_then(|row| row.pointer(spec.id_pointer))
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// The focused row itself.
    #[must_use]
    pub fn focused_row(&self) -> Option<&Value> {
        self.rows.get(self.cursor)
    }

    /// Whether the cursor sits on the last visible row and another
    /// page exists.
    #[must_use]
    pub fn at_last_row(&self) -> bool {
        let visible = self.visible();
        visible.last().is_some_and(|&last| self.cursor == last)
    }

    /// The filter's typing keys: characters edit, `Enter` accepts,
    /// `Esc` cancels.
    fn filter_key(&mut self, code: KeyCode, ctrl: bool) {
        match code {
            KeyCode::Esc => {
                self.filter = None;
                self.filter_typing = false;
                self.matches.clear();
                self.match_at = 0;
            }
            KeyCode::Enter => {
                self.filter_typing = false;
                self.match_at = 0;
                if let Some(&first) = self.matches.first() {
                    self.cursor = first;
                }
            }
            KeyCode::Backspace => {
                if let Some(filter) = self.filter.as_mut() {
                    filter.pop();
                }
                self.recompute_matches();
            }
            KeyCode::Char(character) if !ctrl => {
                if let Some(filter) = self.filter.as_mut() {
                    filter.push(character);
                }
                self.recompute_matches();
            }
            _ => {}
        }
    }

    /// Recompute [`Self::matches`] for the current filter.
    fn recompute_matches(&mut self) {
        let Some(filter) = self.filter.as_deref() else {
            self.matches.clear();
            return;
        };
        if filter.is_empty() {
            self.matches.clear();
            return;
        }
        let needle = filter.to_lowercase();
        self.matches = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.to_string().to_lowercase().contains(&needle))
            .map(|(index, _)| index)
            .collect();
        if self.matches.is_empty() {
            self.match_at = 0;
        } else {
            self.match_at = self.match_at.min(self.matches.len() - 1);
        }
    }

    /// Move the cursor to the next (or previous) match, wrapping.
    /// Without a filter the keys degrade to a plain step.
    fn cycle_match(&mut self, forward: bool) {
        if self.matches.is_empty() {
            if forward {
                self.step(1);
            } else {
                self.step_back(1);
            }
            return;
        }
        let len = self.matches.len();
        self.match_at = if forward {
            (self.match_at + 1) % len
        } else {
            (self.match_at + len - 1) % len
        };
        self.cursor = self.matches[self.match_at];
    }

    /// The visible neighbour `steps` rows down (or the last row).
    fn step(&mut self, steps: usize) {
        let visible = self.visible();
        if visible.is_empty() {
            self.cursor = 0;
            return;
        }
        let position = visible
            .iter()
            .position(|&row| row == self.cursor)
            .unwrap_or(0);
        let target = (position + steps).min(visible.len() - 1);
        self.cursor = visible[target];
    }

    /// The visible neighbour `steps` rows up (or the first row).
    fn step_back(&mut self, steps: usize) {
        let visible = self.visible();
        if visible.is_empty() {
            self.cursor = 0;
            return;
        }
        let position = visible
            .iter()
            .position(|&row| row == self.cursor)
            .unwrap_or(0);
        let target = position.saturating_sub(steps);
        self.cursor = visible[target];
    }

    /// Jump to the first (or last) visible row.
    fn jump_to_edge(&mut self, last: bool) {
        let visible = self.visible();
        if let Some(&row) = if last {
            visible.last()
        } else {
            visible.first()
        } {
            self.cursor = row;
        }
    }

    /// Half a viewport, at least one row.
    fn half_page(&self, viewport: u16) -> usize {
        usize::from(viewport).max(2) / 2
    }

    /// Keep the cursor inside the rows after a reload.
    fn clamp_cursor(&mut self) {
        let last = self.rows.len().saturating_sub(1);
        self.cursor = self.cursor.min(last);
        self.offset = self.offset.min(last);
    }
}

/// Draw the table: the spec's columns, the focus marker, a status
/// line. Column widths shrink from the right when they overflow.
pub fn draw_list(
    frame: &mut Frame,
    area: Rect,
    view: &ListView,
    spec: &DomainSpec,
) {
    let block = Block::bordered()
        .title(format!(" {} ", spec.label))
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows_area = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(inner);
    if rows_area[1].height == 0 {
        return;
    }

    let widths = column_widths(spec, inner.width);
    let header = Row::new(
        spec.columns
            .iter()
            .map(|column| {
                Cell::from(column.label.to_string()).style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
            })
            .collect::<Vec<_>>(),
    );
    let viewport = usize::from(rows_area[1].height);
    let rows: Vec<Row<'static>> = view
        .rows
        .iter()
        .enumerate()
        .skip(view.offset)
        .take(viewport)
        .map(|(index, row)| {
            let focused = index == view.cursor;
            let cells: Vec<Cell<'static>> = spec
                .columns
                .iter()
                .enumerate()
                .map(|(column, definition)| {
                    let text = cell_text(row, definition.pointer);
                    if column == 0 {
                        let marker = if focused { '>' } else { ' ' };
                        Cell::from(format!("{marker} {text}"))
                    } else {
                        Cell::from(text)
                    }
                })
                .collect();
            let style = if focused {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            Row::new(cells).style(style)
        })
        .collect();
    let table = Table::new(rows, widths).header(header).column_spacing(1);
    frame.render_widget(table, rows_area[1]);
    frame.render_widget(Paragraph::new(status_line(view)), rows_area[2]);
}

/// The bottom status line: position, filter, next-page hint.
fn status_line(view: &ListView) -> Line<'static> {
    let dim = Style::default().fg(Color::DarkGray);
    let position = if view.rows.is_empty() {
        " 0/0".to_string()
    } else {
        format!(" {}/{}", view.cursor + 1, view.rows.len())
    };
    let mut spans = vec![Span::styled(position, dim)];
    if let Some(total) = view.total {
        spans.push(Span::styled(format!(" (共 {total})"), dim));
    }
    if let Some(filter) = view.filter.as_deref() {
        let editing = if view.filter_typing { "…" } else { "" };
        spans.push(Span::styled(
            format!("  筛选: {filter}{editing} [{}]", view.matches.len()),
            Style::default().fg(Color::Yellow),
        ));
    }
    if view.loading {
        spans.push(Span::styled("  加载中…", dim));
    }
    if let Some(error) = view.error.as_deref() {
        spans.push(Span::styled(
            format!("  {error}"),
            Style::default().fg(Color::Red),
        ));
    } else if view.next_cursor.is_some() {
        spans.push(Span::styled("  j 到底继续加载", dim));
    }
    Line::from(spans)
}

/// Column constraints that fit `available` cells: widths shrink from
/// the rightmost column when they overflow (the suggested widths are
/// not hard).
fn column_widths(spec: &DomainSpec, available: u16) -> Vec<Constraint> {
    let mut widths: Vec<u16> =
        spec.columns.iter().map(|column| column.width).collect();
    let spacing =
        u32::try_from(spec.columns.len().saturating_sub(1)).unwrap_or(u32::MAX);
    let mut total: u32 =
        widths.iter().map(|&width| u32::from(width)).sum::<u32>() + spacing;
    let available = u32::from(available);
    while total > available {
        let Some(last) = widths.iter_mut().rev().find(|width| **width > 1)
        else {
            break;
        };
        *last -= 1;
        total -= 1;
    }
    widths.into_iter().map(Constraint::Length).collect()
}
