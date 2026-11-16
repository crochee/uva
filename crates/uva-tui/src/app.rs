//! The pure projection: transport envelopes in, UI state out.
//!
//! Nothing here performs I/O. [`App::apply`] folds one
//! [`TransportEvent`] into the state and returns the requests the
//! state wants sent ([`Action`]); the composer turns key input into
//! the same [`Action`] values. `main` owns the loop that hands those
//! actions to [`crate::transport::Transport`].
//!
//! # What the reducer knows
//!
//! The core's wire vocabulary only — durable `session/event` rows, the
//! `live` projection, the `interrupts` set and the `approval/request`
//! notification. It does not know what a provider is, and it never
//! re-derives the log: a `sessions/follow` baseline replaces the
//! transcript, and a later row appends to it.
//!
//! # Idle is derived, not assumed
//!
//! A turn is [`TurnState::Running`] while the `live` projection says
//! so or between a send and its terminal `turn` row; the terminal row
//! (`{"kind":"end","reason":"completed"}`) is what returns the
//! projection to [`TurnState::Idle`]. A crash drops the live
//! projection but keeps the transcript, so the operator can see what
//! was lost.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;

use crate::{
    domains,
    transport::{Request, RequestKind, RpcFailure, TransportEvent},
    views::catalog::{Catalog, ResumeMaterial},
};

/// How far `Ctrl+d` / `Ctrl+u` move the cursor. The reducer never
/// sees the layout, so half a page is a fixed step rather than a
/// measured one.
const HALF_PAGE: usize = 10;

/// The name a call shows when the model named no tool. The provider
/// gives such a call the same placeholder, so the transcript and the
/// wire agree on what the call is called.
const UNNAMED_TOOL: &str = "unknown";

/// The name a tool result shows when the transcript never saw the
/// call it answers: the outcome is known, the tool is not.
const UNKNOWN_TOOL: &str = "unknown tool";

/// How much of a tool's arguments or output one transcript line
/// carries: the TUI's own one-line-entry limit, the same 48
/// characters `views::catalog`'s confirm-modal summary uses. The cut
/// is made on a character boundary by
/// [`uva_core::text::truncate_chars`] — the workspace's one
/// truncator — and marked with the `…` ellipsis the TUI's own
/// truncation points use (`views::catalog`'s summary appends the
/// same marker). Other in-repo call sites mark their cuts
/// differently, so the marker is this crate's convention, not the
/// workspace's.
const ONE_LINE_LIMIT: usize = 48;

/// Who authored a transcript line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The operator's prompt.
    User,
    /// The model's answer.
    Assistant,
    /// A tool result.
    Tool,
    /// A projection-level notice.
    System,
}

/// One rendered transcript line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptLine {
    /// Who authored the text.
    pub role: Role,
    /// The text itself.
    pub text: String,
    /// The row's ordinal in the session log — that row's identity
    /// there, and the only thing a `chat/feedback` target can name.
    ///
    /// `None` for a line that did not come from a stamped row: an
    /// old log's rows carry no ordinal, so the line has none to
    /// offer.
    pub seq: Option<u64>,
}

/// The transport's lifecycle as the projection sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    /// No generation has announced `ready` yet.
    Starting,
    /// The current generation is serving.
    Ready {
        /// The core's version string.
        version: String,
        /// The workspace root the core resolved.
        workspace_root: String,
    },
    /// The generation died or could not be spawned; a restart is
    /// pending.
    Failed(String),
}

/// The turn's lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnState {
    /// No turn is in flight.
    Idle,
    /// A turn is queued or running.
    Running,
    /// The run suspended on a held tool call.
    AwaitingApproval,
}

/// A held call waiting for the operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalPrompt {
    /// The session the suspension belongs to.
    pub session_id: String,
    /// The UI correlation id the answer must carry.
    pub approval_id: String,
    /// The provider's call id.
    pub call_id: String,
    /// The tool the call addresses.
    pub tool: String,
    /// The risk the tool declared.
    pub risk: String,
    /// The agent the call belongs to.
    pub agent: String,
}

/// The operator's answer to an [`ApprovalPrompt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Authorize the call.
    Allow,
    /// Refuse the call; the model sees a tool error.
    Deny,
}

impl Decision {
    /// The wire spelling (`approval/respond.decision`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }
}

/// Which pane plain keys belong to: the composer eats characters, the
/// sidebar list eats Vim motions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pane {
    /// Typing goes to the composer (the today-default single-session
    /// view).
    #[default]
    Composer,
    /// Vim keys drive the sidebar's list.
    Sidebar,
    /// The directory browser owns the keys (list / detail / palette /
    /// confirm); `Esc` at the list root returns to the composer.
    Catalog,
}

/// What the sidebar lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SidebarTab {
    /// The workspace tree: workspaces and their sessions.
    #[default]
    Sessions,
    /// The settings navigation sections.
    Settings,
}

/// The sidebar's own UI state.
#[derive(Debug, Default)]
pub struct SidebarState {
    /// Whether the sidebar is folded down to its icon rail.
    pub collapsed: bool,
    /// Which list the sidebar shows.
    pub tab: SidebarTab,
    /// The selected row, as an index into the flattened selectable
    /// rows (sessions in the sessions tab, sections in the settings
    /// tab).
    pub cursor: usize,
    /// The active search query, if `/` opened one. It stays set
    /// after `Enter` accepts it (so `n`/`N` keep cycling) and clears
    /// on `Esc`.
    pub search: Option<String>,
    /// Whether the query is still being typed (printable keys go to
    /// the query, not the list).
    pub search_typing: bool,
    /// A one-key operator waiting for its second key (`g` for `gg`,
    /// `d` for `dd`).
    pub pending: Option<char>,
}

/// A modal overlay above the panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    /// The keybinding help (`?`).
    Help,
    /// The command palette (`:` / `Ctrl+k`) — a placeholder surface
    /// in the skeleton.
    CommandPalette,
}

/// One session under a workspace in the sidebar tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEntry {
    /// The session id (also the `y` copy target).
    pub id: String,
    /// The listing's status label, when it carried one.
    pub status: String,
}

/// One workspace group in the sidebar tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceNode {
    /// The workspace's display name.
    pub name: String,
    /// The sessions listed under it.
    pub sessions: Vec<SessionEntry>,
}

/// The settings navigation sections, in §6.1's order. One gateway
/// serves one workspace, so the sections — not workspaces — are the
/// sidebar's second tab. The plugin section is gone with the dynamic
/// plugin channel; third-party tools arrive over MCP.
pub const SETTINGS_SECTIONS: [&str; 8] = [
    "账户",
    "通用",
    "模型",
    "智能体",
    "技能与命令",
    "MCP",
    "Hooks",
    "自动化",
];

/// One todo item's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoStatus {
    /// Finished — rendered `✔`.
    Done,
    /// In progress — rendered `▸`.
    Active,
    /// Not started — rendered `·`.
    Pending,
}

/// One todo item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoItem {
    /// What the item says.
    pub content: String,
    /// Where the item stands.
    pub status: TodoStatus,
}

/// The turn's todo list. Empty means no todos — the view reserves no
/// row for it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TodoState {
    items: Vec<TodoItem>,
}

impl TodoState {
    /// A todo list from its items.
    #[must_use]
    pub fn new(items: Vec<TodoItem>) -> Self {
        Self { items }
    }

    /// The items, in order.
    #[must_use]
    pub fn items(&self) -> &[TodoItem] {
        &self.items
    }

    /// How many items there are.
    #[must_use]
    pub fn total(&self) -> usize {
        self.items.len()
    }

    /// How many items are finished.
    #[must_use]
    pub fn done(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.status == TodoStatus::Done)
            .count()
    }

    /// Whether there is nothing to show.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// A request the projection wants the transport to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// `chat/session/create`.
    CreateSession,
    /// `sessions/follow`.
    Follow {
        /// The session to subscribe to.
        session_id: String,
    },
    /// `chat/message/send`.
    SendMessage {
        /// The session the message goes to.
        session_id: String,
        /// The prompt text.
        text: String,
    },
    /// `approval/respond`.
    RespondApproval {
        /// The session the suspension belongs to.
        session_id: String,
        /// The correlation id the prompt carried.
        approval_id: String,
        /// The operator's answer.
        decision: Decision,
    },
    /// `chat/cancel`.
    Cancel {
        /// The session to cancel.
        session_id: String,
    },
    /// `session/unfollow`.
    Unfollow {
        /// The session to stop following.
        session_id: String,
    },
    /// `sessions/events`.
    LoadEvents {
        /// The session whose durable rows should be read.
        session_id: String,
    },
    /// `sessions/list` — refresh the sidebar's workspace tree.
    ListSessions,
    /// Copy one id to the terminal's clipboard (OSC 52). Local only —
    /// the loop that owns stdout performs it.
    CopyId {
        /// The id to put on the clipboard.
        id: String,
    },
    /// `sessions/delete`.
    DeleteSession {
        /// The session whose durable transcript should be removed.
        id: String,
    },
    /// `ui/schema` — the deployment's slot tree.
    FetchUiSchema,
}

impl Action {
    /// The wire request this action serializes to, when it has one —
    /// local-only actions answer `None` and the caller handles them
    /// itself.
    #[must_use]
    pub fn request(&self) -> Option<Request> {
        match self {
            Self::CreateSession => Some(Request::create_session()),
            Self::Follow { session_id } => Some(Request::follow(session_id)),
            Self::SendMessage { session_id, text } => {
                Some(Request::send_message(session_id, text))
            }
            Self::RespondApproval {
                session_id,
                approval_id,
                decision,
            } => Some(Request::respond_approval(
                session_id,
                approval_id,
                decision.as_str(),
            )),
            Self::Cancel { session_id } => Some(Request::cancel(session_id)),
            Self::Unfollow { session_id } => {
                Some(Request::unfollow(session_id))
            }
            Self::LoadEvents { session_id } => {
                Some(Request::events(session_id))
            }
            Self::ListSessions => Some(Request::list_sessions()),
            Self::CopyId { .. } => None,
            Self::DeleteSession { id } => Some(Request::delete_session(id)),
            Self::FetchUiSchema => Some(Request::ui_schema()),
        }
    }
}

/// The whole projection.
#[derive(Debug)]
pub struct App {
    connection: ConnectionState,
    session_id: Option<String>,
    agent_name: Option<String>,
    messages: Vec<TranscriptLine>,
    live_text: String,
    reasoning: String,
    composer: String,
    turn: TurnState,
    approval: Option<ApprovalPrompt>,
    approval_answered: bool,
    status: String,
    pending_prompt: Option<String>,
    creating: bool,
    quit: bool,
    pane: Pane,
    sidebar: SidebarState,
    workspaces: Vec<WorkspaceNode>,
    todo: TodoState,
    sessions_listed: bool,
    deleting: Option<String>,
    overlay: Option<Overlay>,
    /// The directory browser's state.
    catalog: Catalog,
    /// Directory requests produced by keys or responses, drained by
    /// the loop that owns the transport.
    catalog_requests: Vec<Request>,
    /// How many rows the focused list pane can show (the half-page
    /// step and the viewport scroll read it).
    viewport: u16,
    /// `sessions/follow` baseline: the last durable-row ordinal.
    follow_cursor: u64,
    /// `sessions/follow` baseline: the call ids still suspended.
    follow_pending: Vec<String>,
    /// `sessions/follow` baseline: the request-envelope fingerprint.
    follow_request_hash: String,
    /// The deployment's UI tree (empty until the first `ui/schema`
    /// answers).
    ui: crate::ui_schema::UiSchemaResponse,
    /// The tool each call id names: the assistant row's `tool_use`
    /// part fills it, the `tool_result` row reads it back by the id
    /// the result answers.
    tool_names: HashMap<String, String>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            connection: ConnectionState::Starting,
            session_id: None,
            agent_name: None,
            messages: Vec::new(),
            live_text: String::new(),
            reasoning: String::new(),
            composer: String::new(),
            turn: TurnState::Idle,
            approval: None,
            approval_answered: false,
            status: "starting the core".to_string(),
            pending_prompt: None,
            creating: false,
            quit: false,
            pane: Pane::Composer,
            sidebar: SidebarState::default(),
            workspaces: Vec::new(),
            todo: TodoState::default(),
            sessions_listed: false,
            deleting: None,
            overlay: None,
            catalog: Catalog::new(),
            catalog_requests: Vec::new(),
            viewport: 24,
            follow_cursor: 0,
            follow_pending: Vec::new(),
            follow_request_hash: String::new(),
            ui: crate::ui_schema::UiSchemaResponse::default(),
            tool_names: HashMap::new(),
        }
    }
}

impl App {
    /// A projection in its initial state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The current transport lifecycle.
    #[must_use]
    pub fn connection(&self) -> &ConnectionState {
        &self.connection
    }

    /// The followed session, once one is known.
    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// The agent the followed session runs as.
    #[must_use]
    pub fn agent_name(&self) -> Option<&str> {
        self.agent_name.as_deref()
    }

    /// The terminal host's UI tree (empty until the first `ui/schema`
    /// answers).
    #[must_use]
    pub fn ui(&self) -> &crate::ui_schema::UiSchemaResponse {
        &self.ui
    }

    /// The transcript, oldest first.
    #[must_use]
    pub fn messages(&self) -> &[TranscriptLine] {
        &self.messages
    }

    /// The in-progress turn's streamed text.
    #[must_use]
    pub fn live_text(&self) -> &str {
        &self.live_text
    }

    /// The composer's current text.
    #[must_use]
    pub fn composer(&self) -> &str {
        &self.composer
    }

    /// The turn lifecycle.
    #[must_use]
    pub fn turn(&self) -> TurnState {
        self.turn
    }

    /// The held call waiting for a decision, if any.
    #[must_use]
    pub fn approval(&self) -> Option<&ApprovalPrompt> {
        self.approval.as_ref()
    }

    /// Whether the current prompt has already been answered (the
    /// decision is in flight, or the core has not resumed yet).
    #[must_use]
    pub fn approval_answered(&self) -> bool {
        self.approval_answered
    }

    /// The one-line status shown in the header.
    #[must_use]
    pub fn status(&self) -> &str {
        &self.status
    }

    /// The in-progress turn's streamed reasoning (the thinking
    /// block). Kept after the turn ends so the view can fold it into
    /// its one-line summary.
    #[must_use]
    pub fn reasoning(&self) -> &str {
        &self.reasoning
    }

    /// The workspace root the connection resolved, once known — the
    /// composer's bash prompt and the sidebar's group name read it.
    #[must_use]
    pub fn workspace_root(&self) -> Option<&str> {
        match &self.connection {
            ConnectionState::Ready { workspace_root, .. } => {
                Some(workspace_root)
            }
            _ => None,
        }
    }

    /// Which pane plain keys belong to.
    #[must_use]
    pub fn pane(&self) -> Pane {
        self.pane
    }

    /// The sidebar's UI state.
    #[must_use]
    pub fn sidebar(&self) -> &SidebarState {
        &self.sidebar
    }

    /// The sidebar's workspace tree.
    #[must_use]
    pub fn workspaces(&self) -> &[WorkspaceNode] {
        &self.workspaces
    }

    /// The current turn's todos.
    #[must_use]
    pub fn todo(&self) -> &TodoState {
        &self.todo
    }

    /// How many selectable rows the sidebar's current tab holds.
    #[must_use]
    pub fn sidebar_len(&self) -> usize {
        match self.sidebar.tab {
            SidebarTab::Sessions => {
                self.workspaces.iter().map(|node| node.sessions.len()).sum()
            }
            SidebarTab::Settings => SETTINGS_SECTIONS.len(),
        }
    }

    /// Replace the sidebar's workspace tree (the `sessions/list`
    /// response folds here), clamping the cursor into range.
    pub fn set_workspaces(&mut self, workspaces: Vec<WorkspaceNode>) {
        self.workspaces = workspaces;
        self.clamp_sidebar_cursor();
    }

    /// Replace the current todos.
    pub fn set_todo(&mut self, todo: TodoState) {
        self.todo = todo;
    }

    /// Fold or unfold the sidebar's icon rail.
    pub fn toggle_sidebar(&mut self) {
        self.sidebar.collapsed = !self.sidebar.collapsed;
    }

    /// Switch the sidebar's list.
    pub fn set_sidebar_tab(&mut self, tab: SidebarTab) {
        self.sidebar.tab = tab;
        self.sidebar.cursor = 0;
    }

    /// Ask for the sidebar's tree to be re-listed.
    #[must_use]
    pub fn refresh_sidebar(&mut self) -> Vec<Action> {
        self.sessions_listed = true;
        vec![Action::ListSessions]
    }

    /// `o` on the sidebar: open the directory browser on the sessions
    /// domain — the workspace tree's own data, browsable by columns,
    /// with the domain's operations on the palette.
    fn open_catalog(&mut self) -> Vec<Action> {
        self.pane = Pane::Catalog;
        let Some(spec) = domains::DOMAINS.first() else {
            return Vec::new();
        };
        let requests = self.catalog.open(spec);
        self.catalog_requests.extend(requests);
        Vec::new()
    }

    /// `s` on the sidebar: the workspace group's 🔍 entry — open the
    /// search domain's query prompt.
    fn open_search_catalog(&mut self) -> Vec<Action> {
        self.pane = Pane::Catalog;
        let requests = self.catalog.open_search();
        self.catalog_requests.extend(requests);
        Vec::new()
    }

    fn clamp_sidebar_cursor(&mut self) {
        let last = self.sidebar_len().saturating_sub(1);
        self.sidebar.cursor = self.sidebar.cursor.min(last);
    }

    /// The modal overlay above the panes, if one is open.
    #[must_use]
    pub fn overlay(&self) -> Option<Overlay> {
        self.overlay
    }

    /// The directory browser.
    #[must_use]
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// The directory browser, mutably — the loop that intercepts
    /// directory responses folds them in here.
    #[must_use]
    pub fn catalog_mut(&mut self) -> &mut Catalog {
        &mut self.catalog
    }

    /// How many rows the focused list can show. Set once per frame by
    /// the loop that owns the terminal; the catalog's half-page steps
    /// and the list's viewport scroll read it.
    pub fn set_viewport(&mut self, viewport: u16) {
        self.viewport = viewport;
    }

    /// Take the directory requests the state wants sent.
    pub fn take_catalog_requests(&mut self) -> Vec<Request> {
        let mut requests = std::mem::take(&mut self.catalog_requests);
        requests.extend(self.catalog.take_requests());
        requests
    }

    /// Queue directory requests the loop should send (a post-write
    /// refresh produced while folding a response).
    pub fn enqueue_catalog_requests(&mut self, requests: Vec<Request>) {
        self.catalog_requests.extend(requests);
    }

    /// Fold one key press into the projection.
    ///
    /// The dispatch order is the contract: `Ctrl+c` quits from
    /// anywhere; `Ctrl+b` (the rail) and `Ctrl+k` (the palette) work
    /// in both panes; an open overlay swallows everything but its
    /// close keys; an open approval owns `y`/`n` (answered presses
    /// are swallowed, not recycled into copy-id); and only then does
    /// the focused pane read the key — the composer eats characters,
    /// the sidebar eats Vim motions. `Esc` steps back one level:
    /// overlay first, then the search, then the composer's focus.
    pub fn handle_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let code = key.code;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && code == KeyCode::Char('c') {
            self.quit();
            return Vec::new();
        }
        if ctrl && code == KeyCode::Char('b') {
            self.toggle_sidebar();
            return Vec::new();
        }
        if ctrl && code == KeyCode::Char('k') {
            self.overlay = Some(Overlay::CommandPalette);
            return Vec::new();
        }
        if self.overlay.is_some() {
            if matches!(code, KeyCode::Esc | KeyCode::Enter) {
                self.overlay = None;
            }
            return Vec::new();
        }
        if self.approval.is_some() {
            match code {
                KeyCode::Char('y') => {
                    return self
                        .answer_approval(Decision::Allow)
                        .into_iter()
                        .collect();
                }
                KeyCode::Char('n') => {
                    return self
                        .answer_approval(Decision::Deny)
                        .into_iter()
                        .collect();
                }
                _ => {}
            }
        }
        match self.pane {
            Pane::Composer => self.handle_composer_key(code, ctrl),
            Pane::Sidebar => self.handle_sidebar_key(code, ctrl),
            Pane::Catalog => self.handle_catalog_key(code, key.modifiers),
        }
    }

    /// The directory browser's keys. The approval branch (and every
    /// global key) already ran in [`Self::handle_key`], so this is
    /// only reached when the prompt is closed — D30's `y` is the
    /// copy verb here, never the approval answer.
    fn handle_catalog_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Vec<Action> {
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        if code == KeyCode::Char('?') && !ctrl && !self.catalog.palette_open() {
            self.overlay = Some(Overlay::Help);
            return Vec::new();
        }
        let resume = self.resume_material();
        let requests =
            self.catalog
                .key(code, modifiers, self.viewport, resume.as_ref());
        self.fold_catalog(requests)
    }

    /// Route one key to the open command palette even when the
    /// conversation pane owns the keyboard: `Ctrl+k` raises the real
    /// palette above any pane.
    pub fn handle_palette_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let resume = self.resume_material();
        let requests = self.catalog.key(
            key.code,
            key.modifiers,
            self.viewport,
            resume.as_ref(),
        );
        self.fold_catalog(requests)
    }

    /// Fold the catalog's side effects (requests, follow/copy/exit
    /// signals) into the projection's outputs.
    fn fold_catalog(&mut self, requests: Vec<Request>) -> Vec<Action> {
        self.catalog_requests.extend(requests);
        let mut actions = Vec::new();
        if let Some(target) = self.catalog.take_follow() {
            actions.extend(self.open_session(&target));
        }
        if let Some(id) = self.catalog.take_copy() {
            actions.push(Action::CopyId { id });
        }
        if self.catalog.take_exit() {
            self.pane = Pane::Composer;
        }
        actions
    }

    /// Open one session through the reducer's existing path (unfollow
    /// the old one, follow the new one); the follow baseline replaces
    /// the transcript.
    fn open_session(&mut self, session_id: &str) -> Vec<Action> {
        self.pane = Pane::Composer;
        if Some(session_id) == self.session_id.as_deref() {
            return Vec::new();
        }
        let mut actions = Vec::new();
        if let Some(old) = self.session_id.take() {
            actions.push(Action::Unfollow { session_id: old });
        }
        self.status = format!("opening session {session_id}");
        actions.push(Action::Follow {
            session_id: session_id.to_string(),
        });
        actions
    }

    /// The follow baseline's resume material. `None` before any
    /// session is followed; the resume operation is disabled unless
    /// one exists and still has suspended interrupts.
    fn resume_material(&self) -> Option<ResumeMaterial> {
        let session_id = self.session_id.clone()?;
        // The feedback target is the current turn's answer row's
        // **ordinal** — the `seq` the log's writer stamped on it. A
        // count of the assistant lines in this view would name
        // nothing: two sessions both count "msg-1", and the server
        // resolves the target against the log's rows. A line whose row
        // carried no ordinal (an old log) offers no target at all.
        let last_message_id =
            self.current_turn_target().map(|seq| seq.to_string());
        Some(ResumeMaterial {
            session_id,
            last_seq: self.follow_cursor,
            pending: self.follow_pending.clone(),
            request_snapshot_hash: self.follow_request_hash.clone(),
            last_message_id,
        })
    }

    /// The ordinal of the row the **current turn's** answer came
    /// from.
    ///
    /// A rating is about this turn's answer: an earlier turn's answer
    /// belongs to a different turn, so the target never reaches back
    /// into it. The turn starts at the operator's last prompt — the
    /// boundary the web's projection splits its assistant bubbles at
    /// too, so both surfaces name the same row.
    ///
    /// A turn that wrote only reasoning renders no assistant line, so
    /// it names no row at all: the web offers no target for a bubble
    /// whose only content is reasoning, and the TUI offers none
    /// either.
    fn current_turn_target(&self) -> Option<u64> {
        let turn_start = self
            .messages
            .iter()
            .rposition(|line| line.role == Role::User)
            .map(|index| index + 1)
            .unwrap_or(0);
        self.messages[turn_start..]
            .iter()
            .rev()
            .find(|line| line.role == Role::Assistant)
            .and_then(|line| line.seq)
    }

    /// The composer's keys: characters, backspace, submit, and `Esc`
    /// stepping out to the list.
    fn handle_composer_key(
        &mut self,
        code: KeyCode,
        ctrl: bool,
    ) -> Vec<Action> {
        match code {
            KeyCode::Enter => self.submit(),
            KeyCode::Backspace => {
                self.backspace();
                Vec::new()
            }
            KeyCode::Esc => {
                self.pane = Pane::Sidebar;
                Vec::new()
            }
            KeyCode::Char(character) if !ctrl => {
                self.insert_char(character);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// The sidebar's keys: Vim list motions, search, and the
    /// per-entry verbs.
    fn handle_sidebar_key(&mut self, code: KeyCode, ctrl: bool) -> Vec<Action> {
        // A pending operator (g, d) consumes exactly one key: its
        // partner fires, anything else disarms.
        if let Some(operator) = self.sidebar.pending.take() {
            match (operator, code) {
                ('g', KeyCode::Char('g')) => self.sidebar.cursor = 0,
                ('d', KeyCode::Char('d')) => return self.delete_selected(),
                _ => {}
            }
            return Vec::new();
        }
        if self.sidebar.search_typing {
            self.handle_search_key(code);
            return Vec::new();
        }
        if ctrl {
            return match code {
                KeyCode::Char('d') => {
                    self.move_cursor(HALF_PAGE);
                    Vec::new()
                }
                KeyCode::Char('u') => {
                    self.move_cursor_back(HALF_PAGE);
                    Vec::new()
                }
                _ => Vec::new(),
            };
        }
        match code {
            KeyCode::Char('j') => {
                self.move_cursor(1);
                Vec::new()
            }
            KeyCode::Char('k') => {
                self.move_cursor_back(1);
                Vec::new()
            }
            KeyCode::Char('g') => {
                self.sidebar.pending = Some('g');
                Vec::new()
            }
            KeyCode::Char('G') => {
                self.sidebar.cursor = self.sidebar_len().saturating_sub(1);
                Vec::new()
            }
            KeyCode::Char('/') => {
                self.sidebar.search = Some(String::new());
                self.sidebar.search_typing = true;
                Vec::new()
            }
            KeyCode::Char('n') => {
                self.jump_match(true);
                Vec::new()
            }
            KeyCode::Char('N') => {
                self.jump_match(false);
                Vec::new()
            }
            KeyCode::Enter => self.open_selected(),
            KeyCode::Char('y') => self.copy_selected(),
            KeyCode::Char('x') => self.delete_selected(),
            KeyCode::Char('d') => {
                self.sidebar.pending = Some('d');
                Vec::new()
            }
            KeyCode::Char('i') => {
                self.pane = Pane::Composer;
                Vec::new()
            }
            KeyCode::Tab => {
                self.sidebar.tab = match self.sidebar.tab {
                    SidebarTab::Sessions => SidebarTab::Settings,
                    SidebarTab::Settings => SidebarTab::Sessions,
                };
                self.sidebar.cursor = 0;
                self.sidebar.search = None;
                Vec::new()
            }
            KeyCode::Char(':') => {
                self.overlay = Some(Overlay::CommandPalette);
                Vec::new()
            }
            KeyCode::Char('?') => {
                self.overlay = Some(Overlay::Help);
                Vec::new()
            }
            KeyCode::Char('r') => self.refresh_sidebar(),
            KeyCode::Char('o') => self.open_catalog(),
            KeyCode::Char('s') => self.open_search_catalog(),
            KeyCode::Esc => {
                self.sidebar.search = None;
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// The search's typing keys: characters edit the query, `Enter`
    /// accepts it and jumps, `Esc` cancels the whole search.
    fn handle_search_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                self.sidebar.search = None;
                self.sidebar.search_typing = false;
            }
            KeyCode::Enter => {
                self.sidebar.search_typing = false;
                self.jump_match(true);
            }
            KeyCode::Backspace => {
                if let Some(query) = self.sidebar.search.as_mut() {
                    query.pop();
                }
            }
            KeyCode::Char(character) => {
                if let Some(query) = self.sidebar.search.as_mut() {
                    query.push(character);
                }
            }
            _ => {}
        }
    }

    /// Move the cursor down by `steps`, clamped to the list.
    fn move_cursor(&mut self, steps: usize) {
        let last = self.sidebar_len().saturating_sub(1);
        self.sidebar.cursor = (self.sidebar.cursor + steps).min(last);
    }

    /// Move the cursor up by `steps`, clamped to the first row.
    fn move_cursor_back(&mut self, steps: usize) {
        self.sidebar.cursor = self.sidebar.cursor.saturating_sub(steps);
    }

    /// Jump to the next (or previous) row matching the active query,
    /// wrapping at the ends. An empty query degrades to a plain
    /// step.
    fn jump_match(&mut self, forward: bool) {
        let Some(query) = self.sidebar.search.clone() else {
            return;
        };
        if query.is_empty() {
            if forward {
                self.move_cursor(1);
            } else {
                self.move_cursor_back(1);
            }
            return;
        }
        let needle = query.to_lowercase();
        let matches: Vec<usize> = (0..self.sidebar_len())
            .filter(|&row| self.row_text(row).to_lowercase().contains(&needle))
            .collect();
        let Some(first) = matches.first().copied() else {
            return;
        };
        let cursor = self.sidebar.cursor;
        self.sidebar.cursor = if forward {
            matches
                .iter()
                .find(|&&row| row > cursor)
                .copied()
                .unwrap_or(first)
        } else {
            matches
                .iter()
                .rev()
                .find(|&&row| row < cursor)
                .copied()
                .unwrap_or(matches[matches.len() - 1])
        };
    }

    /// The searchable text of selectable row `row`.
    fn row_text(&self, row: usize) -> String {
        match self.sidebar.tab {
            SidebarTab::Sessions => self
                .workspaces
                .iter()
                .flat_map(|node| node.sessions.iter())
                .nth(row)
                .map(|entry| {
                    if entry.status.is_empty() {
                        entry.id.clone()
                    } else {
                        format!("{} {}", entry.id, entry.status)
                    }
                })
                .unwrap_or_default(),
            SidebarTab::Settings => SETTINGS_SECTIONS
                .get(row)
                .map(|section| (*section).to_string())
                .unwrap_or_default(),
        }
    }

    /// The session the sidebar's cursor sits on, if the sessions tab
    /// has one there.
    fn selected_session(&self) -> Option<&SessionEntry> {
        if self.sidebar.tab != SidebarTab::Sessions {
            return None;
        }
        let mut before = 0;
        for node in &self.workspaces {
            if self.sidebar.cursor < before + node.sessions.len() {
                return Some(&node.sessions[self.sidebar.cursor - before]);
            }
            before += node.sessions.len();
        }
        None
    }

    /// `Enter` on a session: stop following the old one and follow
    /// the selected one. The follow baseline replaces the transcript.
    fn open_selected(&mut self) -> Vec<Action> {
        let Some(entry) = self.selected_session().cloned() else {
            return Vec::new();
        };
        if Some(entry.id.as_str()) == self.session_id.as_deref() {
            return Vec::new();
        }
        let mut actions = Vec::new();
        if let Some(old) = self.session_id.take() {
            actions.push(Action::Unfollow { session_id: old });
        }
        self.status = format!("opening session {}", entry.id);
        actions.push(Action::Follow {
            session_id: entry.id,
        });
        actions
    }

    /// `y` on a session: copy its id.
    fn copy_selected(&mut self) -> Vec<Action> {
        self.selected_session()
            .map(|entry| Action::CopyId {
                id: entry.id.clone(),
            })
            .into_iter()
            .collect()
    }

    /// `x` (or `dd`) on a session: ask the core to delete it. The
    /// sidebar refreshes when the answer lands.
    fn delete_selected(&mut self) -> Vec<Action> {
        let Some(entry) = self.selected_session().cloned() else {
            return Vec::new();
        };
        self.deleting = Some(entry.id.clone());
        vec![Action::DeleteSession { id: entry.id }]
    }

    /// Whether the loop should stop.
    #[must_use]
    pub fn is_quit(&self) -> bool {
        self.quit
    }

    /// Append one character to the composer.
    pub fn insert_char(&mut self, character: char) {
        self.composer.push(character);
    }

    /// Append text to the composer.
    pub fn insert_str(&mut self, text: &str) {
        self.composer.push_str(text);
    }

    /// Remove the composer's last character.
    pub fn backspace(&mut self) {
        self.composer.pop();
    }

    /// Ask the loop to stop.
    pub fn quit(&mut self) {
        self.quit = true;
    }

    /// Record a local failure (a terminal read, a write to the core).
    pub fn note_error(&mut self, message: impl Into<String>) {
        self.status = message.into();
    }

    /// Submit the composer.
    ///
    /// With a session, the prompt is appended optimistically and a
    /// `chat/message/send` is returned. Without one, the prompt is
    /// parked and a `chat/session/create` is returned; the send follows
    /// the new session's `sessions/follow` baseline, so no event can
    /// arrive before the subscription exists.
    #[must_use]
    pub fn submit(&mut self) -> Vec<Action> {
        let text = self.composer.trim().to_string();
        if text.is_empty() {
            return Vec::new();
        }
        self.composer.clear();
        if let Some(session_id) = self.session_id.clone() {
            // The optimistic line has no row yet: the durable
            // `user_message` row lands later, and it is the row that
            // carries an ordinal.
            self.push_line(Role::User, text.clone(), None);
            self.turn = TurnState::Running;
            self.status = "sending".to_string();
            return vec![Action::SendMessage { session_id, text }];
        }
        self.pending_prompt = Some(text);
        self.status = "creating a session".to_string();
        if self.creating {
            return Vec::new();
        }
        self.creating = true;
        vec![Action::CreateSession]
    }

    /// Answer the held call.
    ///
    /// Returns `None` when there is no prompt, or when the current
    /// prompt was already answered. The prompt stays rendered until
    /// the core's response (or the resumed `interrupts` event) clears
    /// it, so a failed answer can be retried.
    #[must_use]
    pub fn answer_approval(&mut self, decision: Decision) -> Option<Action> {
        if self.approval_answered {
            return None;
        }
        let (session_id, approval_id) = {
            let prompt = self.approval.as_ref()?;
            (prompt.session_id.clone(), prompt.approval_id.clone())
        };
        self.approval_answered = true;
        self.turn = TurnState::Running;
        self.status = format!("approval {} sent", decision.as_str());
        Some(Action::RespondApproval {
            session_id,
            approval_id,
            decision,
        })
    }

    /// Cancel the running turn, when there is a session.
    #[must_use]
    pub fn cancel_turn(&mut self) -> Option<Action> {
        let session_id = self.session_id.clone()?;
        Some(Action::Cancel { session_id })
    }

    /// Fold one transport event into the projection.
    pub fn apply(&mut self, event: TransportEvent) -> Vec<Action> {
        match event {
            TransportEvent::Ready {
                version,
                workspace_root,
            } => self.apply_ready(version, workspace_root),
            TransportEvent::Response {
                request, result, ..
            } => self.apply_response(request, result),
            TransportEvent::Notification { method, params } => {
                self.apply_notification(&method, &params);
                Vec::new()
            }
            TransportEvent::Disconnected { reason } => {
                self.apply_disconnected(reason);
                Vec::new()
            }
            TransportEvent::SpawnFailed { message, retry_in } => {
                self.apply_spawn_failed(&message, retry_in);
                Vec::new()
            }
            TransportEvent::GatewayStarting { reason } => {
                self.connection = ConnectionState::Starting;
                self.status = reason;
                Vec::new()
            }
            TransportEvent::GatewayRestarting { attempt } => {
                self.connection = ConnectionState::Starting;
                self.status = format!("gateway restarting, attempt {attempt}");
                Vec::new()
            }
        }
    }

    fn apply_ready(
        &mut self,
        version: String,
        workspace_root: String,
    ) -> Vec<Action> {
        self.connection = ConnectionState::Ready {
            version,
            workspace_root,
        };
        self.status = "connected".to_string();
        // A first generation with no session gets created here; after a
        // crash the session (and its transport-level follow) already
        // exists, so no duplicate create is issued.
        let mut actions = if self.session_id.is_none() && !self.creating {
            self.creating = true;
            self.status = "creating a session".to_string();
            vec![Action::CreateSession]
        } else {
            Vec::new()
        };
        // The UI tree belongs to the deployment, not to a session:
        // every core generation is asked once after it announces ready.
        actions.push(Action::FetchUiSchema);
        actions
    }

    fn apply_spawn_failed(
        &mut self,
        message: &str,
        _retry_in: std::time::Duration,
    ) {
        self.connection = ConnectionState::Failed(format!(
            "could not start the gateway: {message}"
        ));
        self.creating = false;
        self.status = format!("gateway start failed: {message}");
    }

    fn apply_disconnected(&mut self, reason: String) {
        self.connection = ConnectionState::Failed(reason.clone());
        self.creating = false;
        self.turn = TurnState::Idle;
        self.live_text.clear();
        self.reasoning.clear();
        self.approval = None;
        self.approval_answered = false;
        self.status = reason;
    }

    fn apply_response(
        &mut self,
        request: Option<RequestKind>,
        result: Result<Value, RpcFailure>,
    ) -> Vec<Action> {
        let Some(kind) = request else {
            if let Err(error) = result {
                self.status = error.to_string();
            }
            return Vec::new();
        };
        match result {
            Err(error) => {
                self.apply_failure(kind, &error);
                Vec::new()
            }
            Ok(value) => self.apply_success(kind, value),
        }
    }

    fn apply_failure(&mut self, kind: RequestKind, error: &RpcFailure) {
        self.status = error.to_string();
        match kind {
            RequestKind::CreateSession => self.creating = false,
            RequestKind::SendMessage | RequestKind::Cancel => {
                self.turn = TurnState::Idle;
            }
            RequestKind::ApprovalRespond => {
                // The prompt is still open: let the operator decide
                // again rather than silently dropping the suspension.
                self.approval_answered = false;
                self.turn = TurnState::AwaitingApproval;
            }
            RequestKind::Follow
            | RequestKind::Unfollow
            | RequestKind::Events
            | RequestKind::ListSessions
            | RequestKind::UiSchema => {}
            RequestKind::DeleteSession => {
                // The refusal names the reason; nothing was deleted.
                self.deleting = None;
            }
            // Directory requests are intercepted by `main` and routed
            // to `views::catalog`; this arm is the exhaustiveness
            // placeholder and never runs.
            RequestKind::Catalog(_) => {}
        }
    }

    fn apply_success(
        &mut self,
        kind: RequestKind,
        value: Value,
    ) -> Vec<Action> {
        match kind {
            RequestKind::CreateSession => {
                self.creating = false;
                let Some(session_id) = value
                    .get("session_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                else {
                    self.status =
                        "the core answered create without a session id"
                            .to_string();
                    return Vec::new();
                };
                self.session_id = Some(session_id.clone());
                self.agent_name = value
                    .get("agent_name")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                self.status = "session created".to_string();
                vec![Action::Follow { session_id }]
            }
            RequestKind::Follow => {
                self.apply_follow(&value);
                let mut actions = self.take_pending_prompt();
                // The first baseline without a parked prompt is also
                // the moment the sidebar can list what exists. When a
                // prompt was parked, its send owns the answer and the
                // listing waits for the next baseline.
                if actions.is_empty() && !self.sessions_listed {
                    self.sessions_listed = true;
                    actions.push(Action::ListSessions);
                }
                actions
            }
            RequestKind::SendMessage => {
                self.turn = TurnState::Running;
                self.status = send_status(&value);
                Vec::new()
            }
            RequestKind::ApprovalRespond => {
                self.approval = None;
                self.approval_answered = false;
                self.turn = TurnState::Running;
                self.status = "approval accepted; resuming".to_string();
                Vec::new()
            }
            RequestKind::Cancel => {
                self.turn = TurnState::Idle;
                self.live_text.clear();
                self.status = "cancelled".to_string();
                Vec::new()
            }
            RequestKind::Unfollow => {
                self.status = "session unfollowed".to_string();
                Vec::new()
            }
            RequestKind::Events => {
                if let Some(rows) = value.as_array() {
                    for row in rows {
                        self.apply_row(row);
                    }
                }
                Vec::new()
            }
            RequestKind::ListSessions => {
                self.apply_sessions_list(&value);
                Vec::new()
            }
            RequestKind::DeleteSession => {
                self.status = "session deleted".to_string();
                vec![Action::ListSessions]
            }
            RequestKind::UiSchema => {
                // The tree is data the terminal may not understand:
                // an unreadable answer is a status line, not a crash.
                match serde_json::from_value::<crate::ui_schema::UiSchemaResponse>(
                    value,
                ) {
                    Ok(tree) => self.ui = tree,
                    Err(error) => {
                        self.status =
                            format!("the ui schema was unreadable: {error}");
                    }
                }
                Vec::new()
            }
            // Directory requests are intercepted by `main` and routed
            // to `views::catalog`; this arm is the exhaustiveness
            // placeholder and never runs.
            RequestKind::Catalog(_) => Vec::new(),
        }
    }

    /// Fold a `sessions/list` response into the sidebar's tree. One
    /// gateway serves one workspace, so the listing lands as a single
    /// group named after the workspace root the connection resolved.
    fn apply_sessions_list(&mut self, value: &Value) {
        let name = match &self.connection {
            ConnectionState::Ready { workspace_root, .. } => {
                let base = workspace_root
                    .rsplit('/')
                    .find(|part| !part.is_empty())
                    .unwrap_or(workspace_root);
                if base.is_empty() {
                    "workspace".to_string()
                } else {
                    base.to_string()
                }
            }
            _ => "workspace".to_string(),
        };
        let mut sessions = Vec::new();
        if let Some(rows) = value.get("data").and_then(Value::as_array) {
            for row in rows {
                if let Some(id) = row.get("id").and_then(Value::as_str) {
                    sessions.push(SessionEntry {
                        id: id.to_string(),
                        status: row
                            .get("status")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    });
                }
            }
        }
        self.set_workspaces(vec![WorkspaceNode { name, sessions }]);
        self.prune_deleted_session();
    }

    /// After a delete-driven refresh: if the followed session was the
    /// one deleted and the listing no longer knows it, the projection
    /// has nothing left to follow — clear it. A session the listing
    /// still holds (the delete was refused, or a fresh create raced
    /// the refresh) survives untouched.
    fn prune_deleted_session(&mut self) {
        let Some(deleted) = self.deleting.take() else {
            return;
        };
        let still_listed = self
            .workspaces
            .iter()
            .any(|node| node.sessions.iter().any(|entry| entry.id == deleted));
        if self.session_id.as_deref() == Some(deleted.as_str()) && !still_listed
        {
            self.session_id = None;
            self.messages.clear();
            self.live_text.clear();
            self.reasoning.clear();
            self.approval = None;
            self.approval_answered = false;
            self.tool_names.clear();
            self.turn = TurnState::Idle;
            self.status = "session deleted".to_string();
        }
    }

    fn take_pending_prompt(&mut self) -> Vec<Action> {
        let Some(text) = self.pending_prompt.take() else {
            return Vec::new();
        };
        let Some(session_id) = self.session_id.clone() else {
            self.pending_prompt = Some(text);
            return Vec::new();
        };
        self.push_line(Role::User, text.clone(), None);
        self.turn = TurnState::Running;
        self.status = "sending".to_string();
        vec![Action::SendMessage { session_id, text }]
    }

    fn apply_notification(&mut self, method: &str, params: &Value) {
        match method {
            "session/event" => self.apply_session_event(params),
            "approval/request" => {
                if let Some(prompt) =
                    approval_from(params, self.session_id.as_deref())
                {
                    self.approval = Some(prompt);
                    self.approval_answered = false;
                    self.turn = TurnState::AwaitingApproval;
                    self.status = "awaiting approval".to_string();
                }
            }
            _ => {}
        }
    }

    fn apply_session_event(&mut self, params: &Value) {
        match params.get("kind").and_then(Value::as_str) {
            Some("row") => {
                if let Some(row) = params.get("row") {
                    self.apply_row(row);
                }
            }
            Some("live") => self.apply_live(params.get("live")),
            Some("interrupts") => {
                let first = params
                    .get("pending_interrupts")
                    .and_then(Value::as_array)
                    .and_then(|interrupts| interrupts.first());
                if let Some(first) = first {
                    if let Some(prompt) =
                        approval_from(first, self.session_id.as_deref())
                    {
                        self.approval = Some(prompt);
                        self.approval_answered = false;
                        self.turn = TurnState::AwaitingApproval;
                        self.status = "awaiting approval".to_string();
                    }
                } else {
                    self.approval = None;
                    self.approval_answered = false;
                    if self.turn == TurnState::AwaitingApproval {
                        self.turn = TurnState::Running;
                    }
                }
            }
            _ => {}
        }
    }

    fn apply_follow(&mut self, snapshot: &Value) {
        if let Some(session_id) = snapshot
            .pointer("/header/session_id")
            .and_then(Value::as_str)
        {
            self.session_id = Some(session_id.to_string());
        }
        self.agent_name = snapshot
            .pointer("/header/agent_name")
            .and_then(Value::as_str)
            .map(str::to_string);
        self.messages.clear();
        self.live_text.clear();
        self.reasoning.clear();
        self.approval = None;
        self.approval_answered = false;
        // The baseline's rows replace the transcript, so the join
        // between a call and its result is rebuilt from them too.
        self.tool_names.clear();
        if let Some(rows) = snapshot.get("rows").and_then(Value::as_array) {
            for row in rows {
                self.apply_row(row);
            }
        }
        if let Some(state) =
            snapshot.pointer("/header/state").and_then(Value::as_str)
        {
            self.turn = turn_from_state(state);
        }
        self.apply_live(snapshot.get("live"));
        if let Some(prompt) = snapshot
            .get("pending_interrupts")
            .and_then(Value::as_array)
            .and_then(|interrupts| interrupts.first())
            .and_then(|request| {
                approval_from(request, self.session_id.as_deref())
            })
        {
            self.approval = Some(prompt);
            self.approval_answered = false;
            self.turn = TurnState::AwaitingApproval;
        }
        // The resume token's three parts, straight off the baseline
        // (`Review Focus #4`). Added fields only; the projection
        // above is untouched.
        self.follow_cursor =
            snapshot.get("cursor").and_then(Value::as_u64).unwrap_or(0);
        self.follow_pending = snapshot
            .get("pending_interrupts")
            .and_then(Value::as_array)
            .map(|interrupts| {
                interrupts
                    .iter()
                    .filter_map(|request| {
                        request
                            .get("call_id")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.follow_request_hash = snapshot
            .get("request_snapshot_hash")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        self.status = match self.turn {
            TurnState::Idle => "idle".to_string(),
            TurnState::Running => "running".to_string(),
            TurnState::AwaitingApproval => "awaiting approval".to_string(),
        };
    }

    fn apply_live(&mut self, live: Option<&Value>) {
        let Some(live) = live else {
            return;
        };
        if let Some(text) = live.get("text").and_then(Value::as_str) {
            self.live_text = text.to_string();
        }
        if let Some(text) = live.get("reasoning").and_then(Value::as_str) {
            self.reasoning = text.to_string();
        }
        if live.get("running").and_then(Value::as_bool) == Some(true) {
            self.turn = TurnState::Running;
        }
    }

    pub(crate) fn apply_row(&mut self, row: &Value) {
        let Some(tag) = row.get("type").and_then(Value::as_str) else {
            return;
        };
        if IGNORED_ROW_TYPES.contains(&tag) {
            // Declared ignore: log-only bookkeeping the transcript
            // deliberately does not render.
            return;
        }
        if !APPLIED_ROW_TYPES.contains(&tag) {
            // A tag this build predates (version skew); the
            // accounting test classifies every tag of the current
            // vocabulary, so this branch is empty for same-version
            // peers.
            return;
        }
        // The row's ordinal: the row's identity in the session log,
        // and what a `chat/feedback` target names. An old log's rows
        // carry none, and a line without one offers no rating.
        let seq = row.get("seq").and_then(Value::as_u64);
        match tag {
            "user_message" => self.push_row(Role::User, row, seq),
            "assistant_message" => self.apply_assistant_row(row, seq),
            "tool_result" => self.apply_tool_result_row(row, seq),
            "hook_block" => self.apply_hook_block_row(row, seq),
            "turn" => self.apply_turn_row(row),
            "approval_asked" => self.apply_approval_row(row),
            "session_shutdown" => {
                self.turn = TurnState::Idle;
                self.live_text.clear();
            }
            "warning" => self.apply_warning_row(row, seq),
            // The arm test feeds one row of every `APPLIED_ROW_TYPES`
            // tag through this match: a tag added to the accounting
            // without an arm fails that test, not the first
            // production row.
            _ => unreachable!("APPLIED_ROW_TYPES names `{tag}` with no arm"),
        }
    }

    /// Render one `warning` row — a durable fact about the session the
    /// runtime could not report any other way — as a one-line notice in
    /// the transcript's muted system style. Mirrors the web projection:
    /// with no message, the kind slug says which warning it was.
    fn apply_warning_row(&mut self, row: &Value, seq: Option<u64>) {
        let message = row
            .pointer("/data/message")
            .and_then(Value::as_str)
            .filter(|message| !message.is_empty());
        let kind = row
            .pointer("/data/kind")
            .and_then(Value::as_str)
            .filter(|kind| !kind.is_empty());
        let text = message.or(kind).unwrap_or("warning");
        self.push_line(Role::System, format!("warning: {text}"), seq);
    }

    fn apply_assistant_row(&mut self, row: &Value, seq: Option<u64>) {
        let text = row_text(row);
        if !text.is_empty() {
            // The live projection usually carries the same text until
            // the terminal `turn` row; once the durable row lands, drop
            // the copy so the transcript is not rendered twice. The
            // server's live text concatenates the streamed pieces, so
            // the copy is compared in that shape ([`glued_row_text`])
            // while the line above joins the row's parts with a
            // space.
            if self.live_text == glued_row_text(row) {
                self.live_text.clear();
            }
            self.push_line(Role::Assistant, text, seq);
        }
        // The turn's calls travel in the same row as its text: each
        // one is a line of its own, and its call id is the key the
        // result row answers by.
        //
        // The call line carries the row's ordinal too: it is a line
        // of the same assistant row, and the transcript's last
        // assistant line is the one a rating names. Without it the
        // row's ordinal would be lost the moment a turn called a
        // tool.
        for call in row_tool_calls(row) {
            let ToolCall {
                id,
                name,
                arguments,
            } = call;
            if let Some(id) = id {
                self.tool_names.insert(id, name.clone());
            }
            // The call line carries the arguments too: what the model
            // asked for is the half of the call the transcript was
            // missing. A one-line entry holds a one-line preview of
            // them, not the whole payload.
            let text = match arguments {
                Some(arguments) => {
                    format!("tool call: {name} {}", one_line(&arguments))
                }
                None => format!("tool call: {name}"),
            };
            self.push_distinct_line(Role::Assistant, text, seq);
        }
    }

    /// Render one `tool_result` row — the outcome of a call the
    /// assistant row announced. The row names the call it answers,
    /// not the tool, so the tool's name comes from the join the
    /// assistant row filled; a result whose call the transcript never
    /// saw names no tool and says so.
    fn apply_tool_result_row(&mut self, row: &Value, seq: Option<u64>) {
        let outcome = tool_outcome(row);
        // The outcome says more than ok/failed: a call that never
        // finished carries why (the crash-repair closer's recovery
        // code), and a tool that answered carries what it said — the
        // second half of the call line's pairing.
        let status = outcome.status();
        let name = outcome
            .call_id
            .as_ref()
            .and_then(|id| self.tool_names.get(id))
            .cloned()
            .or(outcome.name)
            .unwrap_or_else(|| UNKNOWN_TOOL.to_string());
        let output = tool_output_text(row);
        let text = if output.is_empty() {
            format!("{name}: {status}")
        } else {
            format!("{name}: {status} {}", one_line(&output))
        };
        self.push_distinct_line(Role::Tool, text, seq);
    }

    /// Render one `hook_block` row — a policy's veto of a call — as a
    /// one-line notice, like the warning row. The row's shape is
    /// pinned by the one writer it has (`docs/log-format.md`):
    /// `data.hook` names the policy that refused, `data.reason` says
    /// why, `data.tool` the call it refused — the same three the web's
    /// `⛔` notice reads.
    fn apply_hook_block_row(&mut self, row: &Value, seq: Option<u64>) {
        let hook = data_field(row, "hook");
        let reason = data_field(row, "reason");
        let tool = data_field(row, "tool");
        let named = match (&hook, &tool) {
            (Some(hook), Some(tool)) => format!("{hook} blocked {tool}"),
            (Some(hook), None) => hook.clone(),
            (None, Some(tool)) => tool.clone(),
            (None, None) => String::new(),
        };
        let text = match (named.is_empty(), reason) {
            (true, Some(reason)) => reason,
            (false, Some(reason)) => format!("{named} — {reason}"),
            (false, None) => named,
            (true, None) => "blocked".to_string(),
        };
        self.push_line(Role::System, format!("hook: {text}"), seq);
    }

    fn push_row(&mut self, role: Role, row: &Value, seq: Option<u64>) {
        let text = row_text(row);
        if !text.is_empty() {
            self.push_line(role, text, seq);
        }
    }

    fn push_line(&mut self, role: Role, text: String, seq: Option<u64>) {
        if self
            .messages
            .last()
            .is_some_and(|line| line.role == role && line.text == text)
        {
            return;
        }
        self.messages.push(TranscriptLine { role, text, seq });
    }

    /// Push a line that must not be collapsed. [`push_line`] drops a
    /// line identical to the one before it — the durable user row
    /// that echoes the prompt the composer already showed. Two calls
    /// to the same tool are two calls, not one line repeated.
    fn push_distinct_line(
        &mut self,
        role: Role,
        text: String,
        seq: Option<u64>,
    ) {
        self.messages.push(TranscriptLine { role, text, seq });
    }

    fn apply_turn_row(&mut self, row: &Value) {
        let data = row.get("data");
        let kind = data
            .and_then(|data| data.get("kind").or_else(|| data.get("phase")))
            .and_then(Value::as_str)
            .unwrap_or_default();
        match kind {
            "start" => {
                self.turn = TurnState::Running;
                self.live_text.clear();
                self.reasoning.clear();
            }
            "end" => {
                let reason = data
                    .and_then(|data| data.get("reason"))
                    .and_then(Value::as_str)
                    .unwrap_or("completed");
                self.turn = if reason == "suspended" {
                    TurnState::AwaitingApproval
                } else {
                    TurnState::Idle
                };
                if reason != "suspended" {
                    self.live_text.clear();
                    self.approval = None;
                    self.approval_answered = false;
                }
                self.status = format!("turn {reason}");
            }
            _ => {}
        }
    }

    fn apply_approval_row(&mut self, row: &Value) {
        let data = row.get("data").unwrap_or(row);
        if let Some(prompt) = approval_from(data, self.session_id.as_deref()) {
            self.approval = Some(prompt);
            self.approval_answered = false;
            self.turn = TurnState::AwaitingApproval;
            self.status = "awaiting approval".to_string();
        }
    }
}
/// Durable row kinds the transcript renders or tracks. Together with
/// [`IGNORED_ROW_TYPES`] this is the projection's accounting of the
/// wire vocabulary — pinned against
/// `uva_session::KNOWN_SESSION_EVENT_TYPES` by the test
/// `every_known_row_kind_is_applied_or_ignored_by_design`, so a tag
/// added to the vocabulary cannot land here silently unrendered.
pub(crate) const APPLIED_ROW_TYPES: &[&str] = &[
    "user_message",
    "assistant_message",
    "tool_result",
    "hook_block",
    "turn",
    "approval_asked",
    "session_shutdown",
    "warning",
];

/// Row kinds the transcript deliberately does not render: log-only
/// bookkeeping whose facts either never reach the operator or arrive
/// through the `live` projection instead. Declared, not accidental —
/// the test named on [`APPLIED_ROW_TYPES`] fails if a vocabulary tag
/// is in neither list.
pub(crate) const IGNORED_ROW_TYPES: &[&str] = &[
    "assistant_chunk",
    "compaction",
    "compaction_start",
    "compaction_summary",
    "compaction_end",
    "tool_call",
    // Streamed chunks of a running tool: the `live` projection paints
    // them while the call runs, and a reload's transcript is rebuilt
    // from the settled `tool_result` row.
    "tool_output",
    "step",
    "iteration",
    "steering_guard",
    "steering_hint",
    "subagent_enter",
    "subagent_exit",
    "request_header",
    "approval_decided",
    "approval_granted",
    "tool_dispatch",
    "request_snapshot",
    "usage",
    "sandbox_mode",
    "tool_result_transformed",
    "parked_prompt",
    "park_consumed",
    "feedback",
    "plugin",
];

/// Map a `sessions/follow` header's state label.
fn turn_from_state(state: &str) -> TurnState {
    match state {
        "running" => TurnState::Running,
        "awaiting_approval" => TurnState::AwaitingApproval,
        _ => TurnState::Idle,
    }
}

/// Render the `chat/message/send` result as a status line.
fn send_status(value: &Value) -> String {
    let queued = value
        .get("queued")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let delivery = value
        .get("delivery")
        .and_then(Value::as_str)
        .unwrap_or("queue");
    if queued {
        format!("message queued ({delivery})")
    } else {
        format!("message accepted ({delivery})")
    }
}

/// Build a prompt from an `approval/request` notification or a pending
/// interrupt entry.
fn approval_from(
    value: &Value,
    session_hint: Option<&str>,
) -> Option<ApprovalPrompt> {
    let approval_id = value
        .get("approval_id")
        .or_else(|| value.get("id"))
        .and_then(Value::as_str)?
        .to_string();
    Some(ApprovalPrompt {
        session_id: value
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| session_hint.map(str::to_string))
            .unwrap_or_default(),
        approval_id,
        call_id: value
            .get("call_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        tool: value
            .get("tool")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        risk: value
            .get("risk")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        agent: value
            .get("agent")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

/// The text of one durable row (`data.content`), independent of
/// whether the core serialized it as `Single`, `Multi` or a bare
/// string. Its parts join with a space ([`joined_content_text`]): the
/// parts are pieces of one message, not one glued word.
fn row_text(row: &Value) -> String {
    row.get("data")
        .and_then(|data| data.get("content"))
        .map(joined_content_text)
        .unwrap_or_default()
}

/// The same row's text with its parts concatenated — the shape the
/// server's live projection holds, since it appends each streamed
/// piece. The durable row and the live copy it duplicates are
/// compared in this shape; the row's own line joins the parts with a
/// space instead ([`row_text`]).
fn glued_row_text(row: &Value) -> String {
    row.get("data")
        .and_then(|data| data.get("content"))
        .map(content_text)
        .unwrap_or_default()
}

/// Extract the text parts of a serialized provider `Content`,
/// concatenated as they came.
fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        other => content_parts(other)
            .iter()
            .map(|part| part_text(part))
            .collect(),
    }
}

/// The parts of one serialized provider `Content`, in the shapes the
/// core serializes it in: `Single`, `Multi`, a bare part — and a bare
/// array, the shape a `tool_result` part's own `content` list uses.
fn content_parts(content: &Value) -> Vec<&Value> {
    match content {
        Value::Object(map) => {
            if let Some(single) = map.get("Single") {
                vec![single]
            } else if let Some(parts) =
                map.get("Multi").and_then(Value::as_array)
            {
                parts.iter().collect()
            } else {
                vec![content]
            }
        }
        Value::Array(parts) => parts.iter().collect(),
        _ => Vec::new(),
    }
}

/// Extract the text of one serialized `ContentPart` (or a bare text
/// object).
fn part_text(part: &Value) -> String {
    match part {
        Value::String(text) => text.clone(),
        Value::Object(map) => {
            let kind = map.get("type").and_then(Value::as_str);
            if matches!(kind, Some("text") | None) {
                map.get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            } else {
                String::new()
            }
        }
        _ => String::new(),
    }
}

/// The tool calls one assistant row carries: each `tool_use` part's
/// call id and the tool's name.
fn row_tool_calls(row: &Value) -> Vec<ToolCall> {
    row.get("data")
        .and_then(|data| data.get("content"))
        .map(content_parts)
        .unwrap_or_default()
        .into_iter()
        .filter_map(ToolCall::of)
        .collect()
}

/// What one `tool_result` row says about the call it answers.
struct ToolOutcome {
    /// The id of the call the result answers — the key the assistant
    /// row's call registered under.
    call_id: Option<String>,
    /// The tool's name, when the row itself knows it.
    name: Option<String>,
    /// True when the tool failed.
    failed: bool,
    /// Why the call never finished, when the row is the crash-repair
    /// closer: one of the wire-stable recovery codes (`TOOL_NOT_STARTED`,
    /// `TOOL_OUTCOME_UNKNOWN`). The web shows the code as the result's
    /// content; the transcript's line carries it beside `failed`.
    error_code: Option<String>,
    /// The closer's `interrupted` marker, for a row that names no
    /// recovery code.
    interrupted: bool,
}

impl ToolOutcome {
    /// The status word the result line carries: `ok`, or `failed`
    /// with the reason the row names — the closer's recovery code,
    /// else its `interrupted` marker. "Failed" alone would send the
    /// operator hunting for a reason the row already had.
    fn status(&self) -> String {
        if !self.failed {
            return "ok".to_string();
        }
        match &self.error_code {
            Some(code) => format!("failed ({code})"),
            None if self.interrupted => "failed (interrupted)".to_string(),
            None => "failed".to_string(),
        }
    }
}

/// Read one `tool_result` row.
///
/// Two rows carry this type. The run's own row holds the tool
/// message the loop committed: the provider `Message` with
/// `role: "tool"`, the tool's output as its `content`, and the id of
/// the call it answers. The crash-repair closer holds a marker
/// instead — `call_id`, the tool's name, an `error` code and
/// `interrupted: true` — because the call never finished. A row whose
/// content is a `tool_result` part carries the whole payload in that
/// part. All three name the call, so all three join.
fn tool_outcome(row: &Value) -> ToolOutcome {
    let part = row
        .get("data")
        .and_then(|data| data.get("content"))
        .map(content_parts)
        .unwrap_or_default()
        .into_iter()
        .find(|part| {
            part.get("type").and_then(Value::as_str) == Some("tool_result")
        });
    let call_id = row
        .get("tool_call_id")
        .and_then(Value::as_str)
        .or_else(|| row.pointer("/data/tool_call_id").and_then(Value::as_str))
        .or_else(|| row.pointer("/data/call_id").and_then(Value::as_str))
        .or_else(|| {
            part.and_then(|part| part.get("tool_use_id"))
                .and_then(Value::as_str)
        })
        .map(str::to_string);
    let name = row
        .pointer("/data/tool_name")
        .and_then(Value::as_str)
        .or_else(|| row.pointer("/data/name").and_then(Value::as_str))
        .or_else(|| {
            part.and_then(|part| part.get("tool_name"))
                .and_then(Value::as_str)
        })
        .map(str::to_string);
    // A failed tool says so three ways: the row's own flag, the
    // part's flag, and the closer's error code or `interrupted`
    // marker. A call that never finished must not read as one that
    // returned.
    let error_code = row
        .pointer("/data/error")
        .and_then(Value::as_str)
        .filter(|code| !code.is_empty())
        .map(str::to_string);
    let interrupted = row
        .pointer("/data/interrupted")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let failed = row
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || part
            .and_then(|part| part.get("is_error"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
        || error_code.is_some()
        || interrupted;
    ToolOutcome {
        call_id,
        name,
        failed,
        error_code,
        interrupted,
    }
}

/// One tool call: the provider's call id, the tool the model asked
/// for, and what it asked for — the arguments, as the part carries
/// them.
struct ToolCall {
    /// The id the result row answers by.
    id: Option<String>,
    /// The tool's name.
    name: String,
    /// The call's arguments in their compact JSON form, when the part
    /// carries any worth showing.
    arguments: Option<String>,
}

impl ToolCall {
    /// Read one `tool_use` part. A part without an id still names a
    /// call the operator should see; it just cannot be joined to a
    /// result.
    fn of(part: &Value) -> Option<Self> {
        let map = part.as_object()?;
        if map.get("type").and_then(Value::as_str) != Some("tool_use") {
            return None;
        }
        // `null` is no arguments and `{}` an empty ask; neither adds
        // anything to the line, so neither renders.
        let arguments = map
            .get("input")
            .map(|input| input.to_string())
            .filter(|input| input != "null" && input != "{}");
        Some(Self {
            id: map.get("id").and_then(Value::as_str).map(str::to_string),
            name: map
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .unwrap_or(UNNAMED_TOOL)
                .to_string(),
            arguments,
        })
    }
}

/// One transcript line's worth of a payload: whitespace collapsed to
/// single spaces — a tool's output is usually many lines — then cut
/// to [`ONE_LINE_LIMIT`] characters on a character boundary and
/// marked with the `…` ellipsis when the cut happened.
fn one_line(text: &str) -> String {
    let flattened = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let (kept, truncated) =
        uva_core::text::truncate_chars(&flattened, ONE_LINE_LIMIT);
    if truncated {
        format!("{kept}…")
    } else {
        kept
    }
}

/// The output a `tool_result` row carries, if any. The run's own row
/// holds the tool message's content; a row the app-server writes for
/// seeded history holds a `tool_result` part whose own `content` list
/// is the output. The crash-repair closer holds neither — its
/// recovery code is the only thing it knows. A multi-part output
/// joins with a space ([`joined_content_text`]): the parts are
/// pieces of one result, not one glued word.
fn tool_output_text(row: &Value) -> String {
    let content = row.get("data").and_then(|data| data.get("content"));
    let part = content
        .map(content_parts)
        .unwrap_or_default()
        .into_iter()
        .find(|part| {
            part.get("type").and_then(Value::as_str) == Some("tool_result")
        });
    match part {
        Some(part) => part
            .get("content")
            .map(joined_content_text)
            .unwrap_or_default(),
        None => content.map(joined_content_text).unwrap_or_default(),
    }
}

/// The text of a serialized provider `Content`, its non-empty parts
/// joined by a space — the tool line's preview of a multi-part
/// output, and the text of the row lines.
///
/// [`content_text`] keeps the parts' own concatenation for the caller
/// that needs it: the live copy's comparison.
fn joined_content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        other => content_parts(other)
            .iter()
            .map(|part| part_text(part))
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// A non-empty string field of a row's `data`, when the row carries
/// one.
fn data_field(row: &Value, field: &str) -> Option<String> {
    row.pointer(&format!("/data/{field}"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests;
