# uva-policy-plugins — the best-of-N strategy

`BestOfNStrategy` — N parallel samples, scored, the winner published via
the event sink.

The strategy asks the model the same question N times concurrently,
scores every usable answer, and publishes the highest-scoring one.
Sampling and scoring both fan out: an LLM-as-judge scorer's N provider
calls overlap instead of queueing behind each other.

## Minimal example

```rust
use std::sync::Arc;

use uva_policy_plugins::best_of_n::{BestOfNStrategy, LongestAnswer};

let strategy = BestOfNStrategy::new(3, Arc::new(LongestAnswer));
// The strategy is `Debug`: the report names the scorer's label.
assert!(format!("{strategy:?}").contains("longest-answer"));
```

The crate also ships `LlmJudgeScorer` (an LLM-as-judge scorer) behind
the `judge` module.

A custom scorer replaces the default heuristic:

```rust
use std::sync::Arc;

use uva_policy_plugins::best_of_n::{BestOfNStrategy, CandidateScorer};

struct PrefersConcise;

#[async_trait::async_trait]
impl CandidateScorer for PrefersConcise {
    async fn score(&self, candidate: &str) -> f64 {
        // Higher wins. Characters, not bytes: a CJK answer must not
        // outrank an ASCII one of the same length.
        -(candidate.chars().count() as f64)
    }

    fn name(&self) -> &str {
        "prefers-concise"
    }
}

let strategy = BestOfNStrategy::new(4, Arc::new(PrefersConcise));
let _ = strategy;
```

Wiring it into a run: the strategy implements
`uva_harness::ReasoningStrategy`, so it is handed to the agent
where the default strategy would go.

## Dependencies

- **Depends on**: `uva-harness` (the strategy trait and the agent
  runtime), `uva-provider` (the request / result vocabulary),
  `uva-session` (the step rows), `uva-core`.
- **Depended on by**: the `uva` facade's `strategy-best-of-n`
  feature. No other crate in the workspace depends on it — a deployment
  picks the strategy it wants.

## Track

**Library base (opt-in reasoning strategy).** It is not an agent-visible
tool plugin; it replaces *how* the model is asked, not *what* it can
call.

See `crates/uva-policy-plugins/src/best_of_n/mod.rs` for the full API.
