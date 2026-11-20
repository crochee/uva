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
//! **The method names are the server's own.** Every method this table
//! names is the server's constant
//! ([`uva_app_server::rpc::methods::names`]) — the terminal keeps no
//! copy of the registry, so a method it calls cannot be misspelled and
//! a method the server adds needs nothing here.
//!
//! Two facts from the server wire shape drove the field list:
//!
//! - the `sessions` domain keys its `id` param differently per
//!   operation — `{"id"}` for `sessions/get|delete|export|fork|status`
//!   and `{"session_id"}` for `sessions/usage|compact|resume` — so
//!   [`Op`] declares its own param key instead of the domain;
//! - not every domain answers `List<T>`
//!   (`models/list` → `{"models": […]}`, `ui/schema` →
//!   `{"schemas": […]}`), so [`DomainSpec::rows_pointer`] names
//!   where the row array is.
//!
//! The envelope itself is the server's: the domains that answer
//! `List<T>` are read through that type ([`decode_list`]), so the
//! rows, the cursor and the total are the server's members rather
//! than a second reading of them here. An envelope that does not
//! decode is **reported** — a drifted shape says so on the status
//! line instead of passing for a domain that holds no rows.

use serde_json::{Map, Value, json};
use uva_app_server::{api::List, rpc::methods::names};

/// The page size a list request asks for: the server's own default.
///
/// The number is the server's (`api/v1/page_query.rs`), referenced
/// rather than copied — the terminal asks for the size the server
/// would use anyway, so it cannot name a page size the server never
/// published.
const PAGE_LIMIT: u64 = uva_app_server::api::v1::DEFAULT_LIMIT;

/// The hit cap a query-shaped listing (search) asks for: the search
/// route's own default (`routes/search.rs`), not the list envelope's
/// — the two endpoints page independently, and one keystroke should
/// not fan out across every domain.
const QUERY_PAGE_LIMIT: usize = uva_app_server::routes::search::DEFAULT_LIMIT;

/// The row array of the server's `List<T>` envelope.
///
/// The cursor-paginated domains answer that envelope; the two bespoke
/// ones (`models/list`, `ui/schema`) answer their own shape.
const LIST_ROWS_POINTER: &str = "/data";

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
/// Wave 1 is `sessions`; wave 2 adds the six definitions and the
/// three bespoke-envelope domains. `chat` is not a directory domain —
/// its operations live in the palette.
pub const DOMAINS: &[DomainSpec] = &[
    DomainSpec {
        label: "会话",
        list_method: names::SESSIONS_LIST,
        rows_pointer: "/data",
        get_method: Some(names::SESSIONS_GET),
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
                method: names::SESSIONS_GET,
                id_key: "id",
                confirm: false,
            },
            Op {
                label: "用量",
                method: names::SESSIONS_USAGE,
                id_key: "session_id",
                confirm: false,
            },
            Op {
                label: "运行状态",
                method: names::SESSIONS_STATUS,
                id_key: "id",
                confirm: false,
            },
            Op {
                label: "导出",
                method: names::SESSIONS_EXPORT,
                id_key: "id",
                confirm: false,
            },
            Op {
                label: "fork",
                method: names::SESSIONS_FORK,
                id_key: "id",
                confirm: true,
            },
            Op {
                label: "resume",
                method: names::SESSIONS_RESUME,
                id_key: "session_id",
                confirm: true,
            },
            Op {
                label: "压缩",
                method: names::SESSIONS_COMPACT,
                id_key: "session_id",
                confirm: true,
            },
            Op {
                label: "删除",
                method: names::SESSIONS_DELETE,
                id_key: "id",
                confirm: true,
            },
        ],
        query_key: None,
    },
    DomainSpec {
        label: "智能体",
        list_method: names::AGENTS_LIST,
        rows_pointer: "/data",
        get_method: Some(names::AGENTS_GET),
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
            method: names::AGENTS_DELETE,
            id_key: "name",
            confirm: true,
        }],
        query_key: None,
    },
    DomainSpec {
        label: "技能",
        list_method: names::SKILLS_LIST,
        rows_pointer: "/data",
        get_method: Some(names::SKILLS_GET),
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
                method: names::SKILLS_DELETE,
                id_key: "name",
                confirm: true,
            },
            // Domain-level: no target id, so it runs with `{}`.
            Op {
                label: "重载",
                method: names::SKILLS_RELOAD,
                id_key: "",
                confirm: true,
            },
        ],
        query_key: None,
    },
    DomainSpec {
        label: "工具",
        list_method: names::TOOLS_LIST,
        rows_pointer: "/data",
        get_method: Some(names::TOOLS_GET),
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
            method: names::TOOLS_UNREGISTER,
            id_key: "name",
            confirm: true,
        }],
        query_key: None,
    },
    DomainSpec {
        label: "任务",
        list_method: names::TASKS_LIST,
        rows_pointer: "/data",
        get_method: Some(names::TASKS_GET),
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
        list_method: names::SCHEDULES_LIST,
        rows_pointer: "/data",
        get_method: Some(names::SCHEDULES_GET),
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
                method: names::SCHEDULES_TICK,
                id_key: "id",
                confirm: true,
            },
            Op {
                label: "删除",
                method: names::SCHEDULES_DELETE,
                id_key: "id",
                confirm: true,
            },
        ],
        query_key: None,
    },
    DomainSpec {
        label: "评测",
        list_method: names::EVALS_LIST,
        rows_pointer: "/data",
        get_method: Some(names::EVALS_GET),
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
                method: names::EVALS_RUN,
                id_key: "name",
                confirm: true,
            },
            Op {
                label: "删除",
                method: names::EVALS_DELETE,
                id_key: "name",
                confirm: true,
            },
        ],
        query_key: None,
    },
    DomainSpec {
        label: "模型",
        list_method: names::MODELS_LIST,
        rows_pointer: "/models",
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
        label: "搜索",
        list_method: names::SEARCH_ALL,
        rows_pointer: "/data",
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
        list_method: names::UI_SCHEMA,
        // `UiSchemaResponse { schemas }`.
        rows_pointer: "/schemas",
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

    /// Whether the domain answers the server's `List<T>` envelope.
    ///
    /// The cursor-paginated domains do: their rows sit in the
    /// envelope's `data`, and the cursor and the total are the
    /// envelope's members. The two bespoke ones (`models/list`,
    /// `ui/schema`) answer their own shape — rows, and nothing else.
    #[must_use]
    pub fn answers_list_envelope(&self) -> bool {
        self.rows_pointer == LIST_ROWS_POINTER
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
        // Only the domains that answer the server's `List<T>`
        // envelope take a limit; the bespoke envelopes (models / ui)
        // answer whole and ignore paging params.
        if self.answers_list_envelope() {
            // Each endpoint's own default: the cursor-paginated
            // envelope pages at `api/v1`'s, the search at the search
            // route's — two owners, two numbers.
            let limit = if self.query_key.is_some() {
                json!(QUERY_PAGE_LIMIT)
            } else {
                json!(PAGE_LIMIT)
            };
            params.insert("limit".to_string(), limit);
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
    /// pointer missed — the bespoke envelopes degrade to an empty
    /// table, never a panic; the `List<T>` domains report their
    /// decode failure instead of degrading).
    pub rows: Vec<Value>,
    /// The next-page cursor, when the response carried one.
    pub next_cursor: Option<String>,
    /// How many rows the whole listing holds, when the response
    /// reported it.
    pub total: Option<u64>,
}

/// Take one page out of a list response through the spec's pointers.
///
/// The cursor-paginated domains answer the server's `List<T>`
/// envelope: the rows, the cursor and the total are that type's
/// members, so the page is read through it — not through a second,
/// hand-written reading of the same envelope. An envelope that does
/// not decode is **reported**: a renamed member is a fact about the
/// server, and a table that silently shows no rows hides it.
///
/// The two bespoke envelopes (`models/list`, `ui/schema`) answer
/// their own shape, and the page they carry has neither a cursor nor
/// a total; their rows are read through the spec's pointer.
///
/// # Errors
///
/// Returns the decode error when the domain answers the `List<T>`
/// envelope and the answer does not decode as one.
pub fn decode_list(
    value: &Value,
    spec: &DomainSpec,
) -> Result<ListPage, serde_json::Error> {
    if spec.answers_list_envelope() {
        let list: List<Value> = serde_json::from_value(value.clone())?;
        return Ok(ListPage {
            rows: list.data,
            next_cursor: list.next_cursor,
            total: list.total,
        });
    }
    Ok(ListPage {
        rows: value
            .pointer(spec.rows_pointer)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        next_cursor: None,
        total: None,
    })
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
