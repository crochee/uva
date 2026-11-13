//! [`uva_policy_plugins::best_of_n`] — the best-of-N reasoning
//! strategy plugin for [`harness`](crate::harness).
//!
//! N parallel samples against the same fitted context, scored,
//! the winner published via `EventSink`. Opt-in via the
//! `strategy-best-of-n` feature; the harness crate itself ships
//! only the canonical ReAct loop.
pub use uva_policy_plugins::best_of_n::*;
