//! One column table per domain — data, not code.
//!
//! The directory browser ([`crate::views::catalog`]) is generic: it
//! learns everything a domain needs from a [`DomainSpec`] here — the
//! list method, where the row array lives in the response (the
//! `List<T>` envelope for most domains, a bare field for the ones that
//! answer a bespoke shape), the columns to draw, the `get` method and
//! the per-row operations. Adding a domain means adding an entry to
//! [`DOMAINS`], never a new screen.
//!
//! **The method names are the server registry's names.** Every string
//! in this file must be greppable in
//! `crates/elysper-app-server/src/rpc/methods/**` (`router.register`).
//! [`KNOWN_METHODS`] is the client-side copy of that registry; the
//! tests assert every declared method is in it.
//!
//! Two facts from the server wire shape drove the field list:
//!
//! - the `sessions` domain keys its `id` param differently per
//!   operation — `{"id"}` for `sessions/get|delete|export|fork|status`
//!   and `{"session_id"}` for `sessions/usage|compact|resume` — so
//!   [`Op`] declares its own param key instead of the domain;
//! - not every domain answers `List<T>`
//!   (`models/list` → `{"models": […]}`, `plugins/list` →
//!   `{"resolved": […]}`, `ui/schema` → `{"schemas": […]}`), so
//!   [`DomainSpec::rows_pointer`] names where the row array is.

use serde_json::{Map, Value, json};

/// The default page size a list request asks for. The server's own
/// default is 20 and its ceiling is 100 (`api/v1/page_query.rs`).
const PAGE_LIMIT: u16 = 50;

/// A query-shaped listing (search) pages at the server's default
/// instead, so one keystroke does not fan out across every domain.
const QUERY_PAGE_LIMIT: u16 = 20;

/// One column: the header label, the row pointer that fills it
/// (serde_json pointer syntax) and a suggested width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Column {
    /// The header label.
    pub label: &'static str,
    /// The value pointer inside one row.
    pub pointer: &'static str,
    /// The suggested column width, in cells.
    pub width: u16,
}

/// One operation a domain offers on a focused row (read operations
/// are here too: their result opens as the detail page).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Op {
    /// What the palette and the confirmation modal call it.
    pub label: &'static str,
    /// The registered wire method.
    pub method: &'static str,
    /// The params key the target id goes under (`"id"` /
    /// `"session_id"` / `"name"`); empty for a domain-level operation
    /// that takes no target (`skills/reload`).
    pub id_key: &'static str,
    /// Whether the operator is asked first (the write operations).
    pub confirm: bool,
}

/// What `Enter` means on a list row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnterOp {
    /// Open the session in the conversation pane (the `sessions`
    /// domain): the reducer's existing `sessions/follow` path.
    Follow,
    /// Show the detail page: the domain's `get` method when it has
    /// one, otherwise the row itself flattened into a key/value
    /// table.
    Detail,
}

/// One domain's whole table row.
#[derive(Debug)]
pub struct DomainSpec {
    /// The label the sidebar and the table title show.
    pub label: &'static str,
    /// The registered list method.
    pub list_method: &'static str,
    /// Where the row array sits in the list response.
    pub rows_pointer: &'static str,
    /// Where the next-page cursor sits (`""` when the response has
    /// none).
    pub next_cursor_pointer: &'static str,
    /// The registered detail method, when the domain has one.
    pub get_method: Option<&'static str>,
    /// The params key the detail method takes its id under.
    pub get_id_key: &'static str,
    /// The row pointer that names one row (the `y` copy target).
    pub id_pointer: &'static str,
    /// What `Enter` does on a row.
    pub enter: EnterOp,
    /// The columns, left to right.
    pub columns: &'static [Column],
    /// The per-row operations, in palette order.
    pub ops: &'static [Op],
    /// The params key a required query goes under (`search/all` is
    /// the only one).
    pub query_key: Option<&'static str>,
}

/// The connected domains, in sidebar order.
///
/// Wave 1 is `sessions`; wave 2 adds the seven definitions and the
/// four bespoke-envelope domains. `chat` is not a directory domain —
/// its operations live in the palette.
pub const DOMAINS: &[DomainSpec] = &[
    DomainSpec {
        label: "会话",
        list_method: "sessions/list",
        rows_pointer: "/data",
        next_cursor_pointer: "/next_cursor",
        get_method: Some("sessions/get"),
        get_id_key: "id",
        id_pointer: "/id",
        enter: EnterOp::Follow,
        columns: &[
            Column {
                label: "会话",
                pointer: "/id",
                width: 26,
            },
            Column {
                label: "状态",
                pointer: "/status",
                width: 12,
            },
            Column {
                label: "开始",
                pointer: "/created_at",
                width: 20,
            },
            Column {
                label: "agent",
                pointer: "/agent_name",
                width: 12,
            },
        ],
        ops: &[
            // The id keys differ per operation — `sessions/get` takes
            // `{"id"}`, `sessions/usage` takes `{"session_id"}`.
            Op {
                label: "详情",
                method: "sessions/get",
                id_key: "id",
                confirm: false,
            },
            Op {
                label: "用量",
                method: "sessions/usage",
                id_key: "session_id",
                confirm: false,
            },
            Op {
                label: "运行状态",
                method: "sessions/status",
                id_key: "id",
                confirm: false,
            },
            Op {
                label: "导出",
                method: "sessions/export",
                id_key: "id",
                confirm: false,
            },
            Op {
                label: "fork",
                method: "sessions/fork",
                id_key: "id",
                confirm: true,
            },
            Op {
                label: "resume",
                method: "sessions/resume",
                id_key: "session_id",
                confirm: true,
            },
            Op {
                label: "压缩",
                method: "sessions/compact",
                id_key: "session_id",
                confirm: true,
            },
            Op {
                label: "删除",
                method: "sessions/delete",
                id_key: "id",
                confirm: true,
            },
        ],
        query_key: None,
    },
    DomainSpec {
        label: "智能体",
        list_method: "agents/list",
        rows_pointer: "/data",
        next_cursor_pointer: "/next_cursor",
        get_method: Some("agents/get"),
        get_id_key: "name",
        id_pointer: "/name",
        enter: EnterOp::Detail,
        columns: &[
            Column {
                label: "名称",
                pointer: "/name",
                width: 20,
            },
            Column {
                label: "类型",
                pointer: "/kind",
                width: 10,
            },
            Column {
                label: "版本",
                pointer: "/version",
                width: 8,
            },
        ],
        ops: &[Op {
            label: "删除",
            method: "agents/delete",
            id_key: "name",
            confirm: true,
        }],
        query_key: None,
    },
    DomainSpec {
        label: "技能",
        list_method: "skills/list",
        rows_pointer: "/data",
        next_cursor_pointer: "/next_cursor",
        get_method: Some("skills/get"),
        get_id_key: "name",
        id_pointer: "/name",
        enter: EnterOp::Detail,
        columns: &[
            Column {
                label: "名称",
                pointer: "/name",
                width: 20,
            },
            Column {
                label: "说明",
                pointer: "/description",
                width: 44,
            },
        ],
        ops: &[
            Op {
                label: "删除",
                method: "skills/delete",
                id_key: "name",
                confirm: true,
            },
            // Domain-level: no target id, so it runs with `{}`.
            Op {
                label: "重载",
                method: "skills/reload",
                id_key: "",
                confirm: true,
            },
        ],
        query_key: None,
    },
    DomainSpec {
        label: "工具",
        list_method: "tools/list",
        rows_pointer: "/data",
        next_cursor_pointer: "/next_cursor",
        get_method: Some("tools/get"),
        get_id_key: "name",
        id_pointer: "/name",
        enter: EnterOp::Detail,
        columns: &[
            Column {
                label: "名称",
                pointer: "/name",
                width: 20,
            },
            Column {
                label: "说明",
                pointer: "/description",
                width: 44,
            },
        ],
        ops: &[Op {
            label: "注销",
            method: "tools/unregister",
            id_key: "name",
            confirm: true,
        }],
        query_key: None,
    },
    DomainSpec {
        label: "任务",
        list_method: "tasks/list",
        rows_pointer: "/data",
        next_cursor_pointer: "/next_cursor",
        get_method: Some("tasks/get"),
        get_id_key: "id",
        id_pointer: "/id",
        enter: EnterOp::Detail,
        columns: &[
            Column {
                label: "id",
                pointer: "/id",
                width: 24,
            },
            Column {
                label: "agent",
                pointer: "/agent",
                width: 12,
            },
            Column {
                label: "状态",
                pointer: "/status",
                width: 10,
            },
            Column {
                label: "开始",
                pointer: "/started_at",
                width: 20,
            },
        ],
        // The server registers only `tasks/list` and `tasks/get`.
        ops: &[],
        query_key: None,
    },
    DomainSpec {
        label: "计划",
        list_method: "schedules/list",
        rows_pointer: "/data",
        next_cursor_pointer: "/next_cursor",
        get_method: Some("schedules/get"),
        get_id_key: "id",
        id_pointer: "/id",
        enter: EnterOp::Detail,
        columns: &[
            Column {
                label: "id",
                pointer: "/id",
                width: 22,
            },
            Column {
                label: "名称",
                pointer: "/name",
                width: 16,
            },
            Column {
                label: "状态",
                pointer: "/status",
                width: 10,
            },
            Column {
                label: "下次",
                pointer: "/next_fire_at",
                width: 20,
            },
        ],
        ops: &[
            Op {
                label: "触发",
                method: "schedules/tick",
                id_key: "id",
                confirm: true,
            },
            Op {
                label: "删除",
                method: "schedules/delete",
                id_key: "id",
                confirm: true,
            },
        ],
        query_key: None,
    },
    DomainSpec {
        label: "工作流",
        list_method: "workflows/list",
        rows_pointer: "/data",
        next_cursor_pointer: "/next_cursor",
        get_method: Some("workflows/get"),
        get_id_key: "id",
        id_pointer: "/id",
        enter: EnterOp::Detail,
        columns: &[
            Column {
                label: "id",
                pointer: "/id",
                width: 28,
            },
            Column {
                label: "步骤",
                pointer: "/step_count",
                width: 8,
            },
            Column {
                label: "阶段",
                pointer: "/phase_count",
                width: 8,
            },
        ],
        ops: &[
            Op {
                label: "运行",
                method: "workflows/run",
                id_key: "id",
                confirm: true,
            },
            Op {
                label: "删除",
                method: "workflows/delete",
                id_key: "id",
                confirm: true,
            },
        ],
        query_key: None,
    },
    DomainSpec {
        label: "评测",
        list_method: "evals/list",
        rows_pointer: "/data",
        next_cursor_pointer: "/next_cursor",
        get_method: Some("evals/get"),
        get_id_key: "name",
        id_pointer: "/name",
        enter: EnterOp::Detail,
        columns: &[
            Column {
                label: "名称",
                pointer: "/name",
                width: 24,
            },
            // The array's length: `cell_text` renders `n 项`.
            Column {
                label: "用例",
                pointer: "/cases",
                width: 8,
            },
        ],
        ops: &[
            Op {
                label: "运行",
                method: "evals/run",
                id_key: "name",
                confirm: true,
            },
            Op {
                label: "删除",
                method: "evals/delete",
                id_key: "name",
                confirm: true,
            },
        ],
        query_key: None,
    },
    DomainSpec {
        label: "模型",
        list_method: "models/list",
        rows_pointer: "/models",
        next_cursor_pointer: "",
        get_method: None,
        get_id_key: "",
        id_pointer: "/model",
        enter: EnterOp::Detail,
        columns: &[
            Column {
                label: "provider",
                pointer: "/provider",
                width: 16,
            },
            Column {
                label: "model",
                pointer: "/model",
                width: 28,
            },
            Column {
                label: "窗口",
                pointer: "/context_window",
                width: 10,
            },
        ],
        ops: &[],
        query_key: None,
    },
    DomainSpec {
        label: "插件",
        list_method: "plugins/list",
        // `PluginsResponse { resolved, skipped, bundles }`; the
        // operator's view is the resolved composition.
        rows_pointer: "/resolved",
        next_cursor_pointer: "",
        get_method: None,
        get_id_key: "",
        id_pointer: "/name",
        enter: EnterOp::Detail,
        columns: &[
            Column {
                label: "名称",
                pointer: "/name",
                width: 20,
            },
            Column {
                label: "类型",
                pointer: "/kind",
                width: 10,
            },
            Column {
                label: "风险",
                pointer: "/risk",
                width: 10,
            },
            Column {
                label: "工具数",
                pointer: "/tools",
                width: 12,
            },
        ],
        ops: &[],
        query_key: None,
    },
    DomainSpec {
        label: "搜索",
        list_method: "search/all",
        rows_pointer: "/data",
        next_cursor_pointer: "/next_cursor",
        get_method: None,
        get_id_key: "",
        id_pointer: "/id",
        enter: EnterOp::Detail,
        columns: &[
            Column {
                label: "域",
                pointer: "/domain",
                width: 10,
            },
            Column {
                label: "id",
                pointer: "/id",
                width: 24,
            },
            Column {
                label: "标题",
                pointer: "/title",
                width: 32,
            },
            Column {
                label: "分",
                pointer: "/score",
                width: 8,
            },
        ],
        ops: &[],
        query_key: Some("q"),
    },
    DomainSpec {
        label: "UI",
        list_method: "ui/schema",
        // `UiSchemaResponse { schemas }`.
        rows_pointer: "/schemas",
        next_cursor_pointer: "",
        get_method: None,
        get_id_key: "",
        id_pointer: "/slot",
        enter: EnterOp::Detail,
        columns: &[
            Column {
                label: "slot",
                pointer: "/slot",
                width: 24,
            },
            Column {
                label: "kind",
                pointer: "/kind",
                width: 16,
            },
        ],
        ops: &[],
        query_key: None,
    },
];

/// Every method name the server registers, as one static list.
///
/// This is the client-side copy of
/// `crates/elysper-app-server/src/rpc/methods/**`'s
/// `router.register` calls (66 names). [`DOMAINS`] may only reference
/// names in here — the test asserts that — and the `:json` gate
/// resolves a typed method name against it before falling back to a
/// leaked string.
pub const KNOWN_METHODS: &[&str] = &[
    // chat
    "chat/session/create",
    "chat/message/send",
    "chat/cancel",
    "chat/regenerate",
    "chat/feedback",
    "chat/usage",
    "chat/operation",
    // agents / skills / tools
    "agents/list",
    "agents/create",
    "agents/default",
    "agents/get",
    "agents/delete",
    "skills/list",
    "skills/create",
    "skills/reload",
    "skills/get",
    "skills/delete",
    "tools/list",
    "tools/register",
    "tools/get",
    "tools/unregister",
    // automation
    "schedules/list",
    "schedules/create",
    "schedules/get",
    "schedules/patch",
    "schedules/delete",
    "schedules/tick",
    "workflows/list",
    "workflows/create",
    "workflows/get",
    "workflows/replace",
    "workflows/delete",
    "workflows/run",
    "tasks/list",
    "tasks/get",
    "evals/list",
    "evals/create",
    "evals/get",
    "evals/delete",
    "evals/run",
    // sessions
    "sessions/list",
    "sessions/get",
    "sessions/delete",
    "sessions/export",
    "sessions/fork",
    "sessions/events",
    "sessions/status",
    "sessions/follow",
    "session/unfollow",
    "sessions/resume",
    "approval/respond",
    "sessions/compact",
    "sessions/usage",
    "sessions/search",
    // misc
    "health/ping",
    "health/version",
    "health/livez",
    "health/readyz",
    "models/list",
    "plugins/list",
    "ui/schema",
    "anthropic/messages",
    "search/all",
    "attachments/create",
    "attachments/get",
    "attachments/delete",
];

impl DomainSpec {
    /// The domain whose list method is `method`.
    #[must_use]
    pub fn by_list_method(method: &str) -> Option<&'static DomainSpec> {
        DOMAINS.iter().find(|spec| spec.list_method == method)
    }

    /// The domain whose sidebar label is `label`.
    #[must_use]
    pub fn by_label(label: &str) -> Option<&'static DomainSpec> {
        DOMAINS.iter().find(|spec| spec.label == label)
    }

    /// The named operation, when the domain has it.
    #[must_use]
    pub fn op(&self, label: &str) -> Option<&Op> {
        self.ops.iter().find(|op| op.label == label)
    }

    /// The `list` params: the page limit, the next-page cursor passed
    /// through untouched (the server's cursor is an opaque base64
    /// token), and the query when the domain needs one.
    #[must_use]
    pub fn list_params(
        &self,
        cursor: Option<&str>,
        query: Option<&str>,
    ) -> Value {
        let mut params = Map::new();
        // Only cursor-paginated domains take a limit; the bespoke
        // envelopes (models / plugins / ui) answer whole and ignore
        // paging params.
        if !self.next_cursor_pointer.is_empty() {
            let limit = if self.query_key.is_some() {
                QUERY_PAGE_LIMIT
            } else {
                PAGE_LIMIT
            };
            params.insert("limit".to_string(), json!(limit));
        }
        if let Some(cursor) = cursor {
            params.insert("cursor".to_string(), json!(cursor));
        }
        if let (Some(key), Some(query)) = (self.query_key, query) {
            params.insert(key.to_string(), json!(query));
        }
        Value::Object(params)
    }

    /// The `get` params: the domain's declared id key.
    #[must_use]
    pub fn get_params(&self, id: &str) -> Value {
        let mut params = Map::new();
        params.insert(self.get_id_key.to_string(), json!(id));
        Value::Object(params)
    }
}

/// One decoded page: the rows stay raw [`Value`]s; the column
/// extraction is deferred to the draw.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ListPage {
    /// The row array the spec's pointer selected (empty when the
    /// pointer missed — a changed server shape degrades to an empty
    /// table, never a panic).
    pub rows: Vec<Value>,
    /// The next-page cursor, when the response carried one.
    pub next_cursor: Option<String>,
    /// How many rows the whole listing holds, when the response
    /// reported it.
    pub total: Option<u64>,
}

/// Take one page out of a list response through the spec's pointers.
#[must_use]
pub fn decode_list(value: &Value, spec: &DomainSpec) -> ListPage {
    let rows = value
        .pointer(spec.rows_pointer)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let next_cursor = if spec.next_cursor_pointer.is_empty() {
        None
    } else {
        value
            .pointer(spec.next_cursor_pointer)
            .and_then(Value::as_str)
            .filter(|cursor| !cursor.is_empty())
            .map(str::to_string)
    };
    let total = value.get("total").and_then(Value::as_u64);
    ListPage {
        rows,
        next_cursor,
        total,
    }
}

/// One cell's text: strings as-is, numbers and booleans rendered,
/// arrays/objects as their length (`evals` shows `cases` that way).
#[must_use]
pub fn cell_text(row: &Value, pointer: &str) -> String {
    match row.pointer(pointer) {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(flag)) => flag.to_string(),
        Some(Value::Array(items)) => format!("{} 项", items.len()),
        Some(Value::Object(map)) => format!("{} 项", map.len()),
    }
}

#[cfg(test)]
mod tests;
