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
//! | [`model_order`]            | The batch runs in the model's order; exclusive = barrier   |
//! | [`tool_projection_memo`]   | Tool projection memoization across iterations               |
//! | [`tool_restriction`]       | Per-agent `denied_tools` / `allow_list`                     |
//! | [`tool_metrics`]           | The Prometheus tool counters the loop's dispatch feeds      |
//! | [`compaction_wiring`]      | Which context manager / compactor a run uses                |
//! | [`host_wiring`]            | Host registrations reaching the loop's decision points      |
//! | [`changed_paths`]          | The durable row says what the call changed                  |
//! | [`steering_guards`]        | Guard pipeline + sanitising rewrites                        |
//! | [`steering_hints`]         | Hints                                                       |
//! | [`steering_hooks`]         | Hook dispatch                                               |
//! | [`steering_tracker_panic`] | Panic isolation of the tracker                              |
//! | [`steering_transformers`]  | Output transformers                                         |
//! | [`hot_path_costs`]         | Hot-path cost budget assertions                             |
//! | [`support`]                | Shared fixtures                                             |

use std::path::PathBuf;

use uva_events::{AgentEvent, SessionEndReason, SystemEvent, WarningKind};
use uva_provider::{Content, ContentPart, Message, TextContent, ToolUse};

use super::*;
use crate::{
    Agent,
    agent::AgentDescriptor,
    input::AgentInput,
    prompt::PromptContext,
};

mod agent_smoke;
mod changed_paths;
mod compaction;
mod compaction_wiring;
mod compose_pipeline_tests;
mod end_to_end_runtime;
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
mod model_order;
pub(crate) mod support;
mod tool_metrics;
mod tool_projection_memo;
mod tool_restriction;
