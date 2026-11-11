//! [`elysper_policy_plugins::cot`] — the chain-of-thought reasoning
//! strategy plugin for [`harness`](crate::harness).
//!
//! One tool-free pass with a step-format instruction appended to
//! the system prompt. Opt-in via the `strategy-cot` feature; the
//! harness crate itself ships only the canonical ReAct loop.
pub use elysper_policy_plugins::cot::*;
