//! The `task` tool as a **plain tool**: a model call names a registered
//! peer agent, and the tool runs that peer as a child run — child
//! session, depth cap, one child per call and cancellation — then
//! returns the child's final text as its result.
//!
//! # Why this crate exists
//!
//! Delegation used to be a loop-internal seam: the tool had to be
//! installed on the agent and the loop dispatched it ahead of the
//! registry. That made a third-party delegating tool depend on the whole
//! harness. Here `task` is an ordinary [`Tool`](uva_tool::Tool): it
//! registers in the tool registry like `read` or `shell`, so exposure,
//! restriction, argument validation and the model's visible list are
//! the registry's mechanisms. The dependency set is the vocabulary
//! ([`uva_agent`], [`uva_events`]) plus the tool trait — never the loop.
//!
//! # The arguments
//!
//! One declaration owns the tool's arguments: the wire names, the
//! JSON shapes, the bounds those shapes put on a value, and what each
//! argument means. The schema the model reads and the parser that
//! reads the tool-call payload are two projections of that table
//! (the `args` module), so the two cannot drift apart: the model is
//! told a shape the tool accepts, and the tool refuses with the
//! words the declaration names.
//!
//! The same module owns the tool's description: the one recorded copy
//! of the words the model reads about the `task` tool. The crate
//! publishes no definition of its own — the mounted tool carries
//! those words to the model, so what the model sees is what this
//! repository records.
//!
//! # The child session
//!
//! The tool mints the child's session id **before** dispatch, asks the
//! host's [`uva_agent::SubagentWriters`] source to open the session
//! (`subagent_enter(child, parent, depth, agent)` plus the dispatch
//! prompt), hands the returned writer to the child run, and appends the
//! `subagent_exit` footer when the run ends. The parent's log gets only
//! the ordinary `tool_call` / `tool_output` / `tool_result` rows: the
//! child session is the one full-fidelity record, and the tool result
//! carries the child id for drill-down.
//!
//! The footer — and the round's artefact set and roster settle — do not
//! live in the dispatch future: dropping that future (the harness stops
//! polling a cancelled run) still lands them, so a cancelled round never
//! leaves a dangling `subagent_enter`, a row stuck at `running`, or a
//! missing artefact set.
//!
//! # Bounds
//!
//! - [`MAX_SUBAGENT_DEPTH`] = 3; the round module's depth rule decides
//!   whether a session may start another round, and refuses past the
//!   cap with a model-readable error. Both paths ask it: the `task`
//!   tool before it delegates, the message tool before it wakes a
//!   child for another round.
//! - One `task` call starts one child run. The tool is
//!   [`uva_tool::ExecutionMode::Sequential`], so the batch scheduler
//!   serialises `task` calls within a batch: delegating starts a whole
//!   run, and running a batch's calls concurrently would change the
//!   loop's cancellation and batch semantics, not just the timing.
//! - The run's cancellation token travels in
//!   [`uva_tool::Context::cancel`]; the child run receives it, so a
//!   cancelled parent terminates its children.
//!
//! # L1 progress
//!
//! While the child runs, the tool streams one merged activity line into
//! the parent's tool block: `[child id] status: activity`. The lines
//! are `Tool::stream`'s `Progress` items, merged over a 150 ms window,
//! so the live projection (`SystemEvent::ToolProgress`) and the durable
//! `tool_output` row a reload replays hold the same summary. The child
//! id on the line is also the L2 handle: it names the child session to
//! open for the full transcript.
//!
//! # The control surface
//!
//! Dispatching a child is only half of the job: the model has to be
//! able to see the children it has and steer them. The `control`
//! module adds three more ordinary tools — `agents_list`,
//! `agents_message` and `agents_interrupt` — and the handle table
//! ([`ControlRegistry`]) they work through. A message is dispatched by
//! the target's state: a running child is steered, an idle one is
//! asked for another round, and one this process no longer holds is
//! woken from a cold start. An interrupt stops the round that is in
//! flight and leaves the child able to answer another message.
//!
//! The lifecycle itself is pure logic ([`ChildState`]): the four
//! states, and the transitions the control surface drives. An illegal
//! transition is refused with a model-readable error, never applied
//! silently and never left hanging.
//!
//! In memory it is the handle table's job. An idle child past
//! [`PARK_TTL`] is parked: its four handles are released and its row
//! stays, so the roster still names it and a message wakes it from its
//! session log again. The session that owns the table is never parked.
//! [`ControlSurface::tick`] advances the lifecycle; a host that wants
//! it to advance while the process is idle installs its runtime's
//! seams with [`ControlSurface::set_park_loop`] (the `tool-task` brick
//! does that from the deployment's `Spawner` / `Sleeper`).
//!
//! # The artefact set
//!
//! Every round leaves `<session dir>/artifacts/<id>.{md,jsonl,patch}`
//! next to the child's own log: a frozen transcript, its markdown
//! rendering, and — for an isolated round that changed something — the
//! diff. The convention is documented in `docs/log-format.md`; a host
//! that keeps sessions off the filesystem returns `None` from
//! [`SessionIndex::session_dir`] and no set is written.

mod args;
mod child;
mod control;
mod gate;
mod plugin;
mod progress;
mod round;
mod spec;
mod tail;
mod tool;
mod worktree;

pub use control::{
    AGENTS_INTERRUPT_TOOL_NAME,
    AGENTS_LIST_TOOL_NAME,
    AGENTS_MESSAGE_TOOL_NAME,
    ChildHandle,
    ChildRecord,
    ChildState,
    ChildStatus,
    ChildSummary,
    ColdWake,
    ControlError,
    ControlRegistry,
    ControlSurface,
    FollowUpTarget,
    HEAD_ROWS,
    MessageOutcome,
    MessageTarget,
    PARK_TTL,
    RecordSource,
    RoundOutcome,
    Scope,
    SessionIndex,
    SteerTarget,
    TransitionError,
    deliver_steer,
    discover,
    head_records,
};
pub use gate::{
    CommandOutcome,
    CommandRunner,
    CommandStopReason,
    GateError,
    GateSpec,
    GateVerdict,
    StdCommandRunner,
    apply_gate_to_output,
    run_gate,
    verdict_summary,
};
pub use plugin::{KEY_CONTROL, TASK_TOOL_PLUGIN_ID, TaskToolPlugin};
pub use spec::{MAX_SUBAGENT_DEPTH, TASK_TOOL_NAME, TaskSpec};
pub use tool::TaskTool;
pub use worktree::{
    WorktreeCleanupResult,
    WorktreeError,
    WorktreeInfo,
    WorktreeSpec,
    cleanup_worktree,
    create_worktree,
    worktree_patch,
};
