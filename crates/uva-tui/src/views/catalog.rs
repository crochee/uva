//! The directory browser's state machine: domain → list → detail, the
//! command palette and the write-confirmation modal.
//!
//! [`Catalog`] owns no I/O: its keys produce [`Request`]s (drained by
//! `main`) and its responses are folded in by `main` **before** they
//! reach the reducer — a directory answer must never overwrite the
//! connection status line. The reducer's own requests keep their
//! kinds; only [`RequestKind::Catalog`](crate::transport::RequestKind)
//! responses are intercepted.
//!
//! Keys follow D30. The one collision with the approval prompt (`y`
//! means "allow" there, "copy id" here) is resolved by the dispatch
//! order in [`crate::app::App::handle_key`]: the approval branch runs
//! before the pane branches, so an open prompt always owns `y`/`n`.
//!
//! The palette (`:`, `Ctrl+k`) is the only path to multi-field writes:
//! `json <method> <params>` parses one JSON object, confirms it like
//! any other write, and sends it through
//! [`Request::catalog`](crate::transport::Request::catalog).

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
};
use serde_json::{Value, json};

use crate::{
    domains::{DOMAINS, DomainSpec, EnterOp, KNOWN_METHODS, Op, decode_list},
    render::centered,
    transport::{Request, RpcFailure},
    views::{
        detail::{Confirm, DetailView, draw_detail, draw_ops_footer},
        list::{ListView, draw_list},
    },
};

/// The state behind `:` / `Ctrl+k`: one editable line, executed on
/// `Enter`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Palette {
    /// The typed text.
    pub input: String,
    /// The byte index the next character goes at.
    pub cursor: usize,
}

/// What `sessions/resume` needs from the followed session's baseline.
///
/// The server's token is `{last_seq, pending, request_snapshot_hash}`;
/// all three come from the `sessions/follow` snapshot the reducer
/// keeps (`cursor`, `pending_interrupts[].call_id`,
/// `request_snapshot_hash`). Without a suspended turn the resume
/// operation is disabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeMaterial {
    /// The followed session (the only one whose baseline we hold).
    pub session_id: String,
    /// The baseline's last durable-row ordinal.
    pub last_seq: u64,
    /// The call ids still awaiting a decision.
    pub pending: Vec<String>,
    /// The fingerprint of the log's newest request envelope.
    pub request_snapshot_hash: String,
    /// The transcript's newest assistant label (`msg-<n>`), the
    /// `chat/feedback` target.
    pub last_message_id: Option<String>,
}

/// The directory browser.
#[derive(Debug, Default)]
pub struct Catalog {
    /// The opened domain; `None` before the first open (the palette
    /// then lists the conversation's chat operations).
    pub domain: Option<&'static DomainSpec>,
    /// The listing.
    pub list: ListView,
    /// The detail page, when one is open.
    pub detail: Option<DetailView>,
    /// The confirmation modal, when a write is waiting for `y`.
    pub confirm: Option<Confirm>,
    /// The query being typed for a `query_key` domain (search).
    pub query: Option<String>,
    /// The command palette, when open.
    pub palette: Option<Palette>,
    /// The one-line status shown above the footer.
    pub status: String,
    /// The params the pending confirmation will send.
    confirm_params: Value,
    /// Requests produced by response handling (a post-write
    /// refresh), drained by the caller.
    outbox: Vec<Request>,
    /// The cursor the in-flight next-page request used (its answer
    /// appends instead of replacing).
    last_cursor: Option<String>,
    /// Whether the last write wants the listing reloaded.
    needs_refresh: bool,
    /// Whether the query is still being typed.
    query_editing: bool,
    /// `Esc` at the list root asks to leave the directory.
    exit: bool,
    /// The row `Enter` asked to open as a conversation.
    follow: Option<String>,
    /// The id `y` asked to copy.
    copy: Option<String>,
    /// The target id of the in-flight `get`/read operation.
    read_target: Option<String>,
}

impl Catalog {
    /// A closed browser.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The palette, when open.
    #[must_use]
    pub fn palette_open(&self) -> bool {
        self.palette.is_some()
    }

    /// Open the palette (the real `:` / `Ctrl+k` panel).
    pub fn open_palette(&mut self) {
        self.palette = Some(Palette::default());
        self.status = "命令面板".to_string();
    }

    /// Take the "leave the directory" signal.
    pub fn take_exit(&mut self) -> bool {
        std::mem::take(&mut self.exit)
    }

    /// Take the "open this session" signal.
    pub fn take_follow(&mut self) -> Option<String> {
        self.follow.take()
    }

    /// Take the "copy this id" signal.
    pub fn take_copy(&mut self) -> Option<String> {
        self.copy.take()
    }

    /// Take the requests response handling produced.
    pub fn take_requests(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.outbox)
    }

    /// Open one domain. A `query_key` domain first asks for its query
    /// and sends nothing yet; every other domain asks for its first
    /// page.
    pub fn open(&mut self, spec: &'static DomainSpec) -> Vec<Request> {
        self.domain = Some(spec);
        self.list = ListView::new();
        self.detail = None;
        self.confirm = None;
        self.palette = None;
        self.query = None;
        self.query_editing = false;
        self.last_cursor = None;
        self.needs_refresh = false;
        self.status = format!("目录 · {}", spec.label);
        if spec.query_key.is_some() {
            self.query = Some(String::new());
            self.query_editing = true;
            return Vec::new();
        }
        self.list.loading = true;
        vec![Request::catalog(
            spec.list_method,
            spec.list_params(None, None),
        )]
    }

    /// Open the search domain's query prompt (the sidebar's `s`
    /// entry). Nothing is sent until the operator types the query and
    /// presses `Enter`.
    pub fn open_search(&mut self) -> Vec<Request> {
        match DomainSpec::by_list_method("search/all") {
            Some(spec) => self.open(spec),
            None => Vec::new(),
        }
    }

    /// Open the search domain with a query (the palette's
    /// `search <词>`).
    pub fn search(&mut self, query: &str) -> Vec<Request> {
        let Some(spec) = DomainSpec::by_list_method("search/all") else {
            return Vec::new();
        };
        let _ = self.open(spec);
        self.query = Some(query.to_string());
        self.query_editing = false;
        self.list.loading = true;
        vec![Request::catalog(
            spec.list_method,
            spec.list_params(None, Some(query)),
        )]
    }

    /// The listing's keys, then the domain verbs.
    pub fn key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        viewport: u16,
        resume: Option<&ResumeMaterial>,
    ) -> Vec<Request> {
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        if self.confirm.is_some() {
            return self.confirm_key(code);
        }
        if self.palette.is_some() {
            return self.palette_key(code, ctrl, resume);
        }
        if self.query_editing {
            return self.query_key(code, ctrl);
        }
        if self.detail.is_some() {
            return self.detail_key(code, ctrl);
        }
        if self.list.key(code, modifiers, viewport) {
            if self.list.take_double_d() {
                return self.invoke_delete();
            }
            if self.list.at_last_row()
                && self.list.next_cursor.is_some()
                && !self.list.loading
            {
                return self.next_page();
            }
            return Vec::new();
        }
        match code {
            KeyCode::Enter => self.enter(),
            KeyCode::Char('y') => self.copy_focused(),
            KeyCode::Char('x') => self.invoke_delete(),
            KeyCode::Esc => {
                self.exit = true;
                Vec::new()
            }
            KeyCode::Char(':') => {
                self.open_palette();
                Vec::new()
            }
            KeyCode::Tab => self.cycle_domain(),
            KeyCode::Char('r') => self.relist(),
            _ => Vec::new(),
        }
    }

    /// Fold one intercepted directory response.
    ///
    /// List answers replace or append the rows; `get` and read
    /// operations open the detail page; writes land on the status
    /// line and ask for a listing refresh (drained through
    /// [`Self::take_requests`]).
    pub fn apply_response(
        &mut self,
        method: &str,
        result: &Result<Value, RpcFailure>,
    ) {
        let Some(spec) = self.domain else {
            self.status = format!("{method}: 无打开的域");
            return;
        };
        if method == spec.list_method {
            match result {
                Ok(value) => {
                    let page = decode_list(value, spec);
                    let append = self.last_cursor.take().is_some();
                    self.list.set_page(page, append);
                    self.status =
                        format!("{} · {} 行", spec.label, self.list.rows.len());
                }
                Err(error) => {
                    self.last_cursor = None;
                    self.list.set_error(error.to_string());
                    self.status = format!("{} 失败: {error}", spec.label);
                }
            }
            return;
        }
        if Some(method) == spec.get_method {
            match result {
                Ok(value) => {
                    let id = self.read_target.take().unwrap_or_default();
                    self.detail = Some(DetailView::from_value(&id, value));
                    self.status = format!("{} · {id}", spec.label);
                }
                Err(error) => {
                    self.read_target = None;
                    self.status = format!("读取失败: {error}");
                }
            }
            return;
        }
        if let Some(op) = spec.ops.iter().find(|op| op.method == method) {
            match result {
                Ok(value) => {
                    if op.confirm {
                        self.status = format!("{} 完成", op.label);
                        self.needs_refresh = true;
                    } else {
                        let id = self.read_target.take().unwrap_or_default();
                        self.detail = Some(DetailView::from_value(&id, value));
                        self.status = format!("{} · {id}", op.label);
                    }
                }
                Err(error) => {
                    self.read_target = None;
                    self.status = format!("{} 失败: {error}", op.label);
                }
            }
            return;
        }
        // The `:json` gate (or a method this domain does not own):
        // show the answer, and reload the listing after it.
        match result {
            Ok(value) => {
                self.detail = Some(DetailView::from_value(method, value));
                self.status = format!("{method} 完成");
                self.needs_refresh = true;
            }
            Err(error) => self.status = format!("{method} 失败: {error}"),
        }
    }

    /// After a write, ask for the first page again. Returns nothing
    /// unless a refresh is pending.
    pub fn refresh(&mut self) -> Vec<Request> {
        if !self.needs_refresh {
            return Vec::new();
        }
        self.needs_refresh = false;
        let Some(spec) = self.domain else {
            return Vec::new();
        };
        self.list.loading = true;
        self.last_cursor = None;
        vec![Request::catalog(
            spec.list_method,
            spec.list_params(None, self.query.as_deref()),
        )]
    }

    /// Re-list the current domain's first page (the operator's `r`).
    fn relist(&mut self) -> Vec<Request> {
        self.needs_refresh = true;
        self.refresh()
    }

    /// A directory request could not be put on the wire (the gateway
    /// is still connecting): stop the loading state and say so. The
    /// operator retries with `r`.
    pub fn note_send_failure(&mut self, message: &str) {
        self.list.loading = false;
        self.status = format!("发送失败: {message}");
    }

    /// Ask for the next page.
    fn next_page(&mut self) -> Vec<Request> {
        let Some(spec) = self.domain else {
            return Vec::new();
        };
        let Some(cursor) = self.list.next_cursor.clone() else {
            return Vec::new();
        };
        self.list.loading = true;
        self.last_cursor = Some(cursor.clone());
        vec![Request::catalog(
            spec.list_method,
            spec.list_params(Some(&cursor), self.query.as_deref()),
        )]
    }

    /// `Enter`: open the session, the `get` page, or the row itself.
    fn enter(&mut self) -> Vec<Request> {
        let Some(spec) = self.domain else {
            return Vec::new();
        };
        let Some(id) = self.list.focused_id(spec) else {
            return Vec::new();
        };
        match spec.enter {
            EnterOp::Follow => {
                self.follow = Some(id);
                Vec::new()
            }
            EnterOp::Detail => {
                if let Some(get) = spec.get_method {
                    self.status = format!("读取 {id}");
                    self.read_target = Some(id.clone());
                    vec![Request::catalog(get, spec.get_params(&id))]
                } else {
                    let row =
                        self.list.focused_row().cloned().unwrap_or(Value::Null);
                    self.detail = Some(DetailView::from_value(&id, &row));
                    Vec::new()
                }
            }
        }
    }

    /// `y`: hand the focused id to the clipboard owner.
    fn copy_focused(&mut self) -> Vec<Request> {
        if let Some(spec) = self.domain
            && let Some(id) = self
                .detail
                .as_ref()
                .map(|detail| detail.id.clone())
                .or_else(|| self.list.focused_id(spec))
        {
            self.copy = Some(id);
        }
        Vec::new()
    }

    /// `x` / `dd`: the domain's delete operation, behind a confirm.
    fn invoke_delete(&mut self) -> Vec<Request> {
        let Some(spec) = self.domain else {
            return Vec::new();
        };
        let Some(op) = spec.op("删除") else {
            self.status = format!("{} 没有删除操作", spec.label);
            return Vec::new();
        };
        self.invoke_op(op, None)
    }

    /// Run one operation: a read sends immediately, a write asks
    /// first (with the params captured for the `y`).
    fn invoke_op(
        &mut self,
        op: &'static Op,
        resume: Option<&ResumeMaterial>,
    ) -> Vec<Request> {
        let Some(spec) = self.domain else {
            return Vec::new();
        };
        let target = self
            .detail
            .as_ref()
            .map(|detail| detail.id.clone())
            .or_else(|| self.list.focused_id(spec))
            .unwrap_or_default();
        let params = if op.id_key.is_empty() {
            json!({})
        } else {
            if target.is_empty() {
                self.status = "没有选中的行".to_string();
                return Vec::new();
            }
            if op.method == "sessions/resume" {
                match resume {
                    Some(material)
                        if material.session_id == target
                            && !material.pending.is_empty() =>
                    {
                        resume_params(&target, material)
                    }
                    _ => {
                        self.status =
                            "resume 不可用：没有挂起中断的会话基线".to_string();
                        return Vec::new();
                    }
                }
            } else {
                json!({ op.id_key: target })
            }
        };
        if op.confirm {
            self.confirm = Some(Confirm {
                op_label: op.label,
                method: op.method,
                target: describe_target(op, &target),
            });
            self.confirm_params = params;
            Vec::new()
        } else {
            self.read_target = Some(target.clone());
            self.status = format!("{} · {target}", op.label);
            vec![Request::catalog(op.method, params)]
        }
    }

    /// The confirmation modal's keys: `y`/`Enter` executes, `Esc`/`n`
    /// cancels. Nothing else is recognised while it is open.
    fn confirm_key(&mut self, code: KeyCode) -> Vec<Request> {
        let Some(confirm) = self.confirm.take() else {
            return Vec::new();
        };
        match code {
            KeyCode::Char('y') | KeyCode::Enter => {
                let params = std::mem::take(&mut self.confirm_params);
                self.status = format!("执行 {}", confirm.op_label);
                self.read_target = None;
                vec![Request::catalog(confirm.method, params)]
            }
            KeyCode::Esc | KeyCode::Char('n') => {
                self.confirm_params = Value::Null;
                self.status = "已取消".to_string();
                Vec::new()
            }
            _ => {
                self.confirm = Some(confirm);
                Vec::new()
            }
        }
    }

    /// The palette's keys: text editing, `Enter` executes.
    fn palette_key(
        &mut self,
        code: KeyCode,
        ctrl: bool,
        resume: Option<&ResumeMaterial>,
    ) -> Vec<Request> {
        let Some(palette) = self.palette.as_mut() else {
            return Vec::new();
        };
        match code {
            KeyCode::Esc => {
                self.palette = None;
                Vec::new()
            }
            KeyCode::Enter => self.run_palette(resume),
            KeyCode::Backspace => {
                if palette.cursor > 0 {
                    let previous =
                        prev_boundary(&palette.input, palette.cursor);
                    palette.input.replace_range(previous..palette.cursor, "");
                    palette.cursor = previous;
                }
                Vec::new()
            }
            KeyCode::Left => {
                palette.cursor = prev_boundary(&palette.input, palette.cursor);
                Vec::new()
            }
            KeyCode::Right => {
                palette.cursor = next_boundary(&palette.input, palette.cursor);
                Vec::new()
            }
            KeyCode::Char(character) if !ctrl => {
                palette.input.insert(palette.cursor, character);
                palette.cursor += character.len_utf8();
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Execute the palette line.
    fn run_palette(&mut self, resume: Option<&ResumeMaterial>) -> Vec<Request> {
        let input = self
            .palette
            .take()
            .map(|palette| palette.input)
            .unwrap_or_default();
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }
        if let Some(rest) = trimmed.strip_prefix("json") {
            return self.json_gate(rest.trim());
        }
        if let Some(rest) = trimmed.strip_prefix("search") {
            let query = rest.trim();
            if query.is_empty() {
                self.status = "用法: search <词>".to_string();
                return Vec::new();
            }
            return self.search(query);
        }
        if let Some(spec) = self.domain
            && let Some(op) = spec.op(trimmed)
            && let Some(op) = DomainSpec::by_list_method(spec.list_method)
                .and_then(|spec| spec.op(op.label))
        {
            return self.invoke_op(op, resume);
        }
        if self.domain.is_none() {
            return self.chat_op(trimmed, resume);
        }
        self.status = format!("未知命令: {trimmed}");
        Vec::new()
    }

    /// The conversation pane's operations (the palette lists these
    /// when no directory domain is open).
    fn chat_op(
        &mut self,
        command: &str,
        resume: Option<&ResumeMaterial>,
    ) -> Vec<Request> {
        let Some(material) = resume else {
            self.status = "没有打开的会话".to_string();
            return Vec::new();
        };
        match command {
            "regenerate" => {
                self.status = "regenerate".to_string();
                vec![Request::catalog(
                    "chat/regenerate",
                    json!({ "session_id": material.session_id }),
                )]
            }
            "feedback-up" | "feedback-down" => {
                let Some(message_id) = material.last_message_id.clone() else {
                    self.status = "没有可评价的助手消息".to_string();
                    return Vec::new();
                };
                let thumbs_up = command == "feedback-up";
                self.status = format!("{command} · {message_id}");
                vec![Request::catalog(
                    "chat/feedback",
                    json!({
                        "message_id": message_id,
                        "thumbs_up": thumbs_up,
                        "session_id": material.session_id,
                    }),
                )]
            }
            other => {
                self.status = format!("未知命令: {other}");
                Vec::new()
            }
        }
    }

    /// `json <method> <params>`: parse, confirm, then send.
    fn json_gate(&mut self, rest: &str) -> Vec<Request> {
        let (method_name, params_text) =
            match rest.split_once(char::is_whitespace) {
                Some((method, params)) => (method, params.trim()),
                None => (rest, "{}"),
            };
        if method_name.is_empty() {
            self.status = "用法: json <method> <params>".to_string();
            return Vec::new();
        }
        let params: Value = if params_text.is_empty() {
            json!({})
        } else {
            match serde_json::from_str(params_text) {
                Ok(value) => value,
                Err(error) => {
                    self.status = format!("参数不是 JSON: {error}");
                    return Vec::new();
                }
            }
        };
        let Some(method) = KNOWN_METHODS
            .iter()
            .copied()
            .find(|name| *name == method_name)
        else {
            self.status =
                format!("未知方法 {method_name}，未发送（注册表里没有）");
            return Vec::new();
        };
        let target = summarize(&params);
        self.confirm = Some(Confirm {
            op_label: "json",
            method,
            target,
        });
        self.confirm_params = params;
        Vec::new()
    }

    /// The query's typing keys: characters edit, `Enter` sends the
    /// listing, `Esc` abandons it.
    fn query_key(&mut self, code: KeyCode, ctrl: bool) -> Vec<Request> {
        match code {
            KeyCode::Esc => {
                self.query = None;
                self.query_editing = false;
                self.status = "已取消查询".to_string();
                Vec::new()
            }
            KeyCode::Enter => {
                let Some(spec) = self.domain else {
                    return Vec::new();
                };
                let query = self.query.clone().unwrap_or_default();
                self.query_editing = false;
                self.list.loading = true;
                self.last_cursor = None;
                vec![Request::catalog(
                    spec.list_method,
                    spec.list_params(None, Some(&query)),
                )]
            }
            KeyCode::Backspace => {
                if let Some(query) = self.query.as_mut() {
                    query.pop();
                }
                Vec::new()
            }
            KeyCode::Char(character) if !ctrl => {
                if let Some(query) = self.query.as_mut() {
                    query.push(character);
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// The detail page's keys: scroll, copy, palette, close.
    fn detail_key(&mut self, code: KeyCode, _ctrl: bool) -> Vec<Request> {
        match code {
            KeyCode::Esc => {
                self.detail = None;
                Vec::new()
            }
            KeyCode::Char('j') => {
                if let Some(detail) = self.detail.as_mut() {
                    detail.scroll = (detail.scroll + 1)
                        .min(detail.rows.len().saturating_sub(1));
                }
                Vec::new()
            }
            KeyCode::Char('k') => {
                if let Some(detail) = self.detail.as_mut() {
                    detail.scroll = detail.scroll.saturating_sub(1);
                }
                Vec::new()
            }
            KeyCode::Char('y') => self.copy_focused(),
            KeyCode::Char(':') => {
                self.open_palette();
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// `Tab`: open the next domain in sidebar order.
    fn cycle_domain(&mut self) -> Vec<Request> {
        let next = match self.domain {
            None => 0,
            Some(current) => {
                let position = DOMAINS
                    .iter()
                    .position(|spec| spec.list_method == current.list_method)
                    .unwrap_or(0);
                (position + 1) % DOMAINS.len()
            }
        };
        let Some(spec) = DOMAINS.get(next) else {
            return Vec::new();
        };
        self.open(spec)
    }

    /// The palette's candidate lines.
    #[must_use]
    pub fn palette_items(&self) -> Vec<String> {
        let mut items: Vec<String> = Vec::new();
        match self.domain {
            Some(spec) => {
                items.extend(spec.ops.iter().map(|op| op.label.to_string()));
            }
            None => {
                for command in ["regenerate", "feedback-up", "feedback-down"] {
                    items.push(command.to_string());
                }
            }
        }
        items.push("search <词>".to_string());
        if self.domain.is_some() || items.len() > 1 {
            items.push("json <method> <params>".to_string());
        }
        items
    }
}

/// The `sessions/resume` body: the token from the follow baseline and
/// one `resolved` decision per open interrupt.
fn resume_params(session_id: &str, material: &ResumeMaterial) -> Value {
    let interrupts: Vec<Value> = material
        .pending
        .iter()
        .map(|call_id| json!({ "id": call_id, "status": "resolved" }))
        .collect();
    json!({
        "session_id": session_id,
        "resume_token": {
            "last_seq": material.last_seq,
            "pending": material.pending,
            "request_snapshot_hash": material.request_snapshot_hash,
        },
        "interrupts": interrupts,
        "allow_drift": false,
    })
}

/// What the confirm modal names as the target.
fn describe_target(op: &Op, target: &str) -> String {
    if op.id_key.is_empty() {
        format!("{}（域级）", op.method)
    } else {
        target.to_string()
    }
}

/// A short one-line params summary for the confirm modal.
fn summarize(params: &Value) -> String {
    const LIMIT: usize = 48;
    let text = params.to_string();
    if text.chars().count() <= LIMIT {
        return text;
    }
    let truncated: String = text.chars().take(LIMIT).collect();
    format!("{truncated}…")
}

/// The previous char boundary before `index`.
fn prev_boundary(input: &str, index: usize) -> usize {
    input[..index]
        .char_indices()
        .next_back()
        .map(|(offset, _)| offset)
        .unwrap_or(0)
}

/// The next char boundary after `index`.
fn next_boundary(input: &str, index: usize) -> usize {
    input[index..]
        .char_indices()
        .nth(1)
        .map(|(offset, _)| index + offset)
        .unwrap_or(input.len())
}

/// Draw the browser's main pane: the listing, or the detail page with
/// its operations footer.
pub fn draw_catalog(frame: &mut Frame, area: Rect, catalog: &Catalog) {
    let Some(spec) = catalog.domain else {
        let hint = Paragraph::new("按 o 打开目录 · Tab 切换域").block(
            Block::bordered()
                .title(" 目录 ")
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        frame.render_widget(hint, area);
        return;
    };
    match &catalog.detail {
        Some(detail) => {
            let rows =
                Layout::vertical([Constraint::Min(1), Constraint::Length(1)])
                    .split(area);
            draw_detail(frame, rows[0], detail, spec);
            draw_ops_footer(frame, rows[1], spec);
        }
        None => draw_list(frame, area, &catalog.list, spec),
    }
}

/// Draw the command palette overlay.
pub fn draw_palette(frame: &mut Frame, area: Rect, catalog: &Catalog) {
    let Some(palette) = catalog.palette.as_ref() else {
        return;
    };
    let area = centered(area, 64, 10);
    frame.render_widget(Clear, area);
    let first = palette
        .input
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_lowercase();
    let mut lines = vec![Line::from(vec![
        Span::styled(": ", Style::default().fg(Color::Yellow)),
        Span::raw(palette.input.clone()),
        Span::styled("▏", Style::default().fg(Color::DarkGray)),
    ])];
    for item in catalog.palette_items() {
        if !first.is_empty() && !item.to_lowercase().starts_with(&first) {
            continue;
        }
        lines.push(Line::from(Span::styled(
            format!("  {item}"),
            Style::default().fg(Color::Cyan),
        )));
    }
    lines.push(Line::from(Span::styled(
        "Enter 执行 · Esc 关闭",
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .title(" 命令面板 ")
                .border_style(Style::default().fg(Color::Yellow)),
        ),
        area,
    );
}
