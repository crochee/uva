//! Test suite for `SessionController`.
//!
//! Split into focused submodules by concern, so the controller
//! itself stays a single file. Each submodule tests one slice of
//! the lifecycle (state machine, persistence, multi-modal turn,
//! rerun, dependencies wiring); cross-cutting fixtures live in
//! [`support`].
//!
//! | Submodule        | What it covers                                                |
//! |------------------|---------------------------------------------------------------|
//! | [`support`]      | Shared fixtures (`VecFactory`, `BlockingFactory`, `RecordingFactory`, `ToolCapturingProvider`, `test_deps`, `make_manager_and_controller`, `wait_for_runs`, …) |
//! | [`truncate`]     | The `truncate` helper — char-counted, ellipsis-on-truncation contract |
//! | [`lifecycle`]    | State transitions — concurrent prompt dedup, idle shutdown, post-shutdown error, snapshot bus, idempotent `close` |
//! | [`approval_deadline`] | The approval deadline — a held call nobody answers is denied and the turn continues |
//! | [`cancel`]       | `Cancel` op — terminates run, idempotent, safe before run starts |
//! | [`compaction_gate`] | The compaction gate as the handle offers it — a promise about run starts, and the remote handle's ungated work |
//! | [`event_emission`] | Persist + broadcast of agent events, durable/ephemeral split, `request_header` epoch marker |
//! | [`history`]      | Multi-turn memory: second prompt seeds `history` from sink, user-role reconstruction, compaction-checkpoint fold |
//! | [`live_projection`] | The live projection and the durable log agree about an answered call |
//! | [`prompt_multi`] | `PromptMulti` — lossless parts round-trip + text-only persistence of multimodal turn |
//! | [`rerun`]        | `Rerun` — parts reach factory, prompt row shadows the replaced turn, chained reruns compose, defensive no-user-turn fallback, mid-run rerun |
//! | [`run_config`]   | R34/R50/R58 + factory wiring: `tool_surface` / `strategy` / `tool_restriction` deps-to-config hop, factory narrows wire tools, runs the configured strategy |
//! | [`usage`]        | Process-wide usage counters: `SystemEvent::Usage` token totals and one turn per `SystemEvent::SessionEnded` (any reason) |
//! | [`writer`]       | One writer per session: feedback interleaves cleanly, a durable row is on disk before broadcast, a fork re-stamps, the ledger takes the sink's ordinals |
mod approval_deadline;
mod cancel;
mod compaction_gate;
mod event_emission;
mod history;
mod lifecycle;
mod live_projection;
mod parked_prompt;
mod prompt_multi;
mod provider_park;
mod rerun;
mod run_config;
mod subagent_sessions;
mod support;
mod truncate;
mod usage;
mod writer;
