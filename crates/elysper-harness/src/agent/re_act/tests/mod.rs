//! Test suite for the canonical ReAct loop and [`ReActAgent`].
//!
//! Split into focused submodules by concern. The shared
//! fixtures — `ScriptedStreamProvider`, `CapturingProvider`,
//! the chunk-sequence helpers, `run_and_collect`, and the
//! peer-agent descriptor — live in [`support`] so each
//! section can pull them via `super::support::*` without
//! re-declaring them.
//!
//! | Submodule                  | What it covers                                              |
//! |----------------------------|-------------------------------------------------------------|
//! | [`agent_smoke`]            | Agent-level descriptor + smoke tests                        |
//! | [`compose_pipeline_tests`] | Prompt-compose pipeline unit tests                          |
//! | [`prompt_integration`]     | Prompt assembler end-to-end (skills, peer agents, tools)    |
//! | [`end_to_end_runtime`]     | Provider-side injection: what the loop actually sends       |
//! | [`lifecycle`]              | One full loop pass: chunks, tool dispatch, max-iter, cancel |
//! | [`multi_agent_panel`]      | Multi-expert code-review scenario + descriptor fixtures     |
//! | [`parallel_cancel`]        | Cancellation mid parallel batch                             |
//! | [`parallel_descriptor_context`] | Descriptor context pinning for parallel dispatch     |
//! | [`parallel_safe`]          | Safe (concurrent) tool execution                            |
//! | [`parallel_unsafe`]        | Sequential bucket for unsafe tools                          |
//! | [`tool_projection_memo`]   | Tool projection memoization across iterations               |
//! | [`tool_restriction`]       | Per-agent `denied_tools` / `allow_list` + delegators        |
//! | [`compaction_wiring`]      | Which context manager / compactor a run uses                |
//! | [`host_wiring`]            | Host registrations reaching the loop's decision points      |
//! | [`host_snapshot`]          | The host's immutable dispatch view and its invalidation     |
//! | [`steering_guards`]        | Guard pipeline + sanitising rewrites                        |
//! | [`steering_hints`]         | Hints                                                       |
//! | [`steering_hooks`]         | Hook dispatch                                               |
//! | [`steering_tracker_panic`] | Panic isolation of the tracker                              |
//! | [`steering_transformers`]  | Output transformers                                         |
//! | [`hot_path_costs`]         | Hot-path cost budget assertions                             |
//! | [`delegation_routing`]     | Name-keyed dispatch of a delegated tool call                |
//! | [`delegation_scheduling`]  | A delegator's `mode()` decides its batch bucket              |
//! | [`support`]                | Shared fixtures                                             |

use std::path::PathBuf;

use elysper_provider::{Content, ContentPart, Message, TextContent, ToolUse};

use super::*;
use crate::{
    Agent,
    agent::AgentDescriptor,
    events::{AgentEvent, SessionEndReason, SystemEvent, WarningKind},
    input::AgentInput,
    prompt::PromptContext,
};

mod agent_smoke;
mod compaction;
mod compaction_wiring;
mod compose_pipeline_tests;
mod delegation_emit_leak;
mod delegation_routing;
mod delegation_scheduling;
mod end_to_end_runtime;
mod host_snapshot;
mod host_wiring;
mod hot_path_costs;
mod multi_agent_panel;
mod parallel_cancel;
mod parallel_descriptor_context;
mod parallel_safe;
mod parallel_unsafe;
mod prompt_integration;
mod steering_guards;
mod steering_hints;
mod steering_hooks;
mod steering_tracker_panic;
mod steering_transformers;

mod lifecycle;
mod log_read_count;
pub(crate) mod support;
mod tool_projection_memo;
mod tool_restriction;
