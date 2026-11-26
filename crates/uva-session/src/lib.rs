//! `uva-session` — durable session storage for the agent runtime.
//!
//! ## Public surface
//!
//! This crate used to expose ~30 types (`SessionManager`,
//! `Session`, `SessionStateMachine`, `Store`, …) plus state
//! machine, token budget, and approval modules. After the
//! refactor the public surface collapses to:
//!
//! - [`SessionSink`] — the single trait every consumer
//!   (typically `uva-harness`) depends on. **6 required
//!   methods** (`id`, `alloc_seq`, `append`, `read`, `snapshot`,
//!   `close`) plus 4 **default-implemented** methods
//!   (`append_with_key`, `append_many`, `read_tail`, `read_head`)
//!   that opt into idempotency, batched durability and bounded
//!   reads without forcing every backend to implement them.
//! - [`SessionError`] — the only error type a sink returns.
//! - [`SessionEndReason`] — passed to `close`.
//! - [`SessionSnapshot`] — returned by `snapshot`.
//! - [`LogWriter`] — the one writer of a session's log: it stamps
//!   every row with the ordinal the sink reserved, buffers the rows,
//!   and writes one batch per flush.
//! - [`manager::SessionRegistry`] — owns the per-session sink
//!   registry. Server-side glue; agents don't talk to it directly.
//! - [`plugin::SessionStorePlugin`] — publishes that registry as the
//!   native kernel `sessions` service ([`KEY_SESSIONS`]).
//! - [`fs::FsBackend`] — the crate's one filesystem offload, over the
//!   `Blocking` seam, so the read/write paths need no runtime.
//! - [`token_meter::TokenMeter`] — pure replay fold that turns
//!   the durable event stream into provider-anchored token
//!   pressure for compaction gating and status displays.
//!
//! ## Backend implementations
//!
//! - [`in_memory::InMemorySessionSink`] — testing only, no
//!   persistence.
//! - [`jsonl::JsonlSessionSink`] — production on-disk backend,
//!   one event per line, fsync'd on append.
//!
//! ## What used to live here and where it went
//!
//! | Old module | New home |
//! |---|---|
//! | `manager::*` (god object) | [`manager::SessionRegistry`] — minimal sink-registry façade; policy moved to server |
//! | `state_machine/*` | deleted; orchestration policy belongs to the server layer |
//! | `token_budget.rs` | deleted; token budgets are tracked inside the agent loop |
//! | `store/*` | reimplemented as [`jsonl::JsonlSessionSink`] |
//!
//! ## Dependency direction
//!
//! ```text
//! uva-app-server  →  uva-session  ←  uva-harness
//! ```
//!
//! `uva-session` is a leaf crate, and a **runtime-free** one: it
//! depends on `serde`, `serde_json`, `async_trait`, `futures`, and
//! `parking_lot` — no runtime, and not on `uva-harness`. The two
//! things that used to name tokio go through `uva-core`'s seams
//! instead: fs work runs on a [`Blocking`](uva_core::blocking::Blocking)
//! ([`fs::FsBackend`]) and a summary's deadline is armed by a
//! [`Sleeper`](uva_core::sleeper::Sleeper). Both default to the
//! runtime-free implementation, and a host on a real runtime swaps
//! its own in (`with_blocking` / `with_sleeper`). A caller names the
//! event it wants written; the writer's typed constructors
//! (`append_event` / `append_event_nowait`) serialize it, so a row's
//! shape is this crate's knowledge and never a caller's.

pub mod compaction;
/// The `compaction` kernel service: the compactor as a plugin.
pub mod compaction_plugin;
/// The provider's content blocks, read off a raw payload — one reader
/// for the wire shape the surface fold and the tool pairing read.
pub(crate) mod content_blocks;
pub mod events;
pub mod fs;
pub mod history;
pub mod in_memory;
pub mod jsonl;
pub mod ledger_key;
pub mod log;
pub mod log_surface;
/// The in-memory session registry: ids, owners, sinks and writers.
pub mod manager;
pub mod operation;
/// The `sessions` service: [`SessionStorePlugin`] builds the registry
/// from [`SessionStoreConfig`] and publishes it under [`KEY_SESSIONS`].
pub mod plugin;
pub mod repair;
pub mod repair_session;
pub mod search;
pub mod sink;
pub mod surface;
pub mod token_meter;
pub mod tool_pairing;
pub mod typed_event_builders;
mod writer;

pub use compaction::{
    CompactionDetails,
    CompactionError,
    CompactionOutcome,
    CompactionPass,
    CompactionPassError,
    CompactionPlan,
    CompactionSettings,
    Compactor,
    DEFAULT_PRUNE_MIN_CHARS,
    DEFAULT_SUMMARY_TIMEOUT,
    PRUNED_TOOL_RESULT,
    PlannedCompaction,
    SummaryCompactor,
    SummaryInput,
    TokenBudget,
    TouchedPair,
    ValidationError,
    begin_compaction,
    finish_compaction,
    plan_compaction,
    plan_compaction_from_rows,
    prune_tool_results,
    should_compact,
    touched_target,
};
pub use compaction_plugin::{
    CompactionPlugin,
    CompactionPluginConfig,
    KEY_COMPACTION,
};
pub use events::{
    AttachmentRefRow,
    CompactionLifecycleOutcome,
    CurrentRow,
    MAX_PLUGIN_KIND_BYTES,
    MAX_PLUGIN_PAYLOAD_BYTES,
    ReplaceRange,
    RowDefect,
    RowDialect,
    RowTag,
    Rows,
    SessionEvent,
    SurfaceOp,
    SurfaceRole,
    SurfaceToken,
    UnreadableLog,
    publish_row,
    publish_rows,
    restore_row,
};
pub use history::{
    HistoryError,
    ProvenancedHistory,
    derive_history,
    surface_message,
    surface_to_messages,
};
pub use jsonl::{
    CURRENT_SCHEMA_VERSION,
    MetaMarker,
    SessionMetadataHeader,
    is_metadata_header_row,
};
pub use log::{RepairReport, SessionLog};
pub use log_surface::{
    MappingGap,
    ResolvedReplace,
    SharedSurfaceLedger,
    SurfaceLedger,
    fold_log_surface,
    log_tail_seq,
    try_fold_log_surface,
};
pub use operation::{
    CompactionRef,
    DEFAULT_BUS_CAPACITY,
    OperationSnapshot,
    OperationState,
    SnapshotBus,
    SnapshotReceiver,
};
pub use plugin::{KEY_SESSIONS, SessionStoreConfig, SessionStorePlugin};
pub use repair::{
    OrphanedCompaction,
    ParkedPromptRecovery,
    STEP_INTERRUPTED,
    TOOL_NOT_STARTED,
    TOOL_OUTCOME_UNKNOWN,
    TURN_INTERRUPTED,
    compaction_outcomes,
    interrupted_turn_closers,
    orphaned_compactions,
    parked_prompts,
};
pub use repair_session::repair_log;
pub use search::{
    DEFAULT_LIMIT,
    EntryHit,
    JsonlSessionSearch,
    SearchError,
    SearchQuery,
    SessionHit,
    SessionSearch,
    SharedSessionSearch,
    jsonl_session_search,
    modified_at,
};
pub use sink::{SessionEndReason, SessionError, SessionSink, SessionSnapshot};
pub use surface::{FoldError, FoldedSurface, assemble_chunks, fold_surface};
pub use token_meter::{
    ContextPressure,
    MeasurementBaseline,
    TokenMeasurement,
    TokenMeter,
    TokenMeterError,
    UsageBuckets,
    estimate_message_tokens,
};
pub use tool_pairing::{
    BalanceCache,
    UnbalancedCut,
    balanced_cut_at_or_before,
    balanced_message_cut,
    build_balance_cache,
    compaction_balanced,
    validate_replace_with_balance,
};
pub use typed_event_builders::{
    approval_asked,
    approval_decided,
    approval_granted,
    assistant_message,
    compaction_end_committed,
    compaction_end_failed,
    compaction_row,
    compaction_start,
    compaction_summary,
    feedback,
    hook_block,
    iteration_end,
    iteration_start,
    message_row,
    park_consumed,
    parked_prompt,
    plugin_row,
    request_header,
    request_snapshot,
    round_trip,
    session_shutdown,
    step_end,
    step_end_suspended,
    step_start,
    structural_kind,
    subagent_enter,
    subagent_exit,
    tool_dispatch,
    tool_output,
    tool_result,
    tool_result_transformed,
    turn_end,
    turn_end_suspended,
    turn_prompt_row,
    turn_start,
    turn_suspended,
    usage,
    user_message,
    warning,
};
pub use writer::LogWriter;
