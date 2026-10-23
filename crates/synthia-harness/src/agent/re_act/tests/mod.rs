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
//! | [`tool_restriction`]       | Per-agent `denied_tools` / `allow_list` + synthetic tools   |
//! | [`compaction_wiring`]      | Which context manager a run uses (policy vs caller's)       |
//! | [`steering_guards`]        | Steering guard pipeline + rewrites                         |
//! | [`steering_hints`]         | Steering hints                                              |
//! | [`steering_hooks`]         | Steering hook dispatch                                      |
//! | [`steering_tracker_panic`] | Panic isolation of the steering tracker                     |
//! | [`steering_transformers`]  | Steering transformers                                       |
//! | [`hot_path_costs`]         | Hot-path cost budget assertions                             |
//! | [`support`]                | Shared fixtures                                             |

use std::path::PathBuf;

use synthia_provider::{Content, ContentPart, Message, TextContent, ToolUse};

use super::*;
use crate::{
    Agent,
    agent::AgentDescriptor,
    events::{AgentEvent, SessionEndReason, SystemEvent, WarningKind},
    input::AgentInput,
    prompt::PromptContext,
};

mod agent_smoke;
mod compaction_wiring;
mod compose_pipeline_tests;
mod end_to_end_runtime;
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
mod synthetic_emit_leak;

mod lifecycle;
pub(crate) mod support;
mod tool_projection_memo;
mod tool_restriction;
