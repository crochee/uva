//! `synthia-session` — durable session storage for the agent runtime.
//!
//! ## Public surface
//!
//! This crate used to expose ~30 types (`SessionManager`,
//! `Session`, `SessionStateMachine`, `Store`, …) plus state
//! machine, token budget, and approval modules. After the
//! refactor the public surface collapses to:
//!
//! - [`SessionSink`] — the single trait every consumer
//!   (typically `synthia-harness`) depends on. **6 required
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
//! synthia-server  →  synthia-session  ←  synthia-harness
//! ```
//!
//! `synthia-session` is a leaf crate, and a **runtime-free** one: it
//! depends on `serde`, `serde_json`, `async_trait`, `futures`, and
//! `parking_lot` — no runtime, and not on `synthia-harness`. The two
//! things that used to name tokio go through `synthia-core`'s seams
//! instead: fs work runs on a [`Blocking`](synthia_core::blocking::Blocking)
//! ([`fs::FsBackend`]) and a summary's deadline is armed by a
//! [`Sleeper`](synthia_core::sleeper::Sleeper). Both default to the
//! runtime-free implementation, and a host on a real runtime swaps
//! its own in (`with_blocking` / `with_sleeper`). Callers
//! (specifically the agent loop) serialize their events to
//! `serde_json::Value` before calling `append`.

pub mod compaction;
pub mod compaction_checkpoint;
pub mod events;
pub mod fs;
pub mod history;
pub mod in_memory;
pub mod jsonl;
pub mod ledger_key;
pub mod log_surface;
pub mod manager;
pub mod operation;
pub mod repair;
pub mod search;
pub mod sink;
pub mod surface;
pub mod token_meter;
pub mod tool_pairing;
pub mod typed_event_builders;
pub mod typed_event_sink;
mod writer;

pub use compaction::{
    CompactionError,
    CompactionOutcome,
    CompactionPass,
    CompactionPassError,
    CompactionPlan,
    Compactor,
    DEFAULT_PRUNE_MIN_CHARS,
    DEFAULT_SUMMARY_TIMEOUT,
    PRUNED_TOOL_RESULT,
    PlannedCompaction,
    SummaryCompactor,
    TokenBudget,
    begin_compaction,
    finish_compaction,
    plan_compaction,
    prune_tool_results,
};
pub use compaction_checkpoint::{
    CompactionCheckpoint,
    CompactionLifecycleView,
};
pub use events::{
    AttachmentRefRow,
    CompactionLifecycleOutcome,
    KNOWN_SESSION_EVENT_TYPES,
    MAX_PLUGIN_KIND_BYTES,
    MAX_PLUGIN_PAYLOAD_BYTES,
    ReplaceRange,
    SessionEvent,
    SurfaceOp,
    SurfaceToken,
    empty_event_of,
};
pub use history::{
    HistoryError,
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
pub use log_surface::{
    MappingGap,
    ResolvedReplace,
    SharedSurfaceLedger,
    SurfaceLedger,
    fold_log_surface,
    log_tail_seq,
    surface_events_from_log,
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
pub use repair::{
    OrphanedCompaction,
    ParkedPromptRecovery,
    TOOL_NOT_STARTED,
    TOOL_OUTCOME_UNKNOWN,
    TURN_INTERRUPTED,
    compaction_outcomes,
    interrupted_turn_closers,
    orphaned_compactions,
    parked_prompts,
};
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
    iteration_end,
    iteration_start,
    request_header,
    request_snapshot,
    round_trip,
    step_end,
    step_end_suspended,
    step_start,
    structural_kind,
    subagent_enter,
    subagent_exit,
    tool_dispatch,
    turn_end,
    turn_end_suspended,
    turn_start,
    turn_suspended,
    usage,
    user_message,
};
pub use typed_event_sink::{CompactionRecordView, TypedEventRecord};
pub use writer::LogWriter;
