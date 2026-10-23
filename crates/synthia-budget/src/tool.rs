//! [`BudgetTool`] — the `Tool` wrapper.

use std::{pin::Pin, sync::Arc};

use futures::Stream;
use synthia_tool::{
    Context,
    ExecutionMode,
    StreamOutput,
    Tool,
    ToolAnnotations,
    ToolOutput,
    ToolOutputDefinition,
};

use crate::budget::Budget;

/// `Tool` wrapper that enforces a [`Budget`]. The wrapper checks
/// the budget *before* the call. Streaming passes through.
pub struct BudgetTool {
    inner: Arc<dyn Tool>,
    budget: parking_lot::RwLock<Budget>,
}

impl BudgetTool {
    pub fn new(inner: Arc<dyn Tool>, budget: Budget) -> Self {
        Self {
            inner,
            budget: parking_lot::RwLock::new(budget),
        }
    }

    #[must_use]
    pub fn budget(&self) -> Budget {
        self.budget.read().clone()
    }

    pub fn set_budget(&self, budget: Budget) {
        *self.budget.write() = budget;
    }
}

#[async_trait::async_trait]
impl Tool for BudgetTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters(&self) -> serde_json::Value {
        self.inner.parameters()
    }

    fn mode(&self) -> ExecutionMode {
        self.inner.mode()
    }

    fn annotations(&self) -> Option<ToolAnnotations> {
        self.inner.annotations()
    }

    fn output_definition(&self) -> ToolOutputDefinition {
        self.inner.output_definition()
    }

    async fn truncate(&self, output: &mut ToolOutput, context: &Context) {
        self.inner.truncate(output, context).await;
    }

    fn stream<'a>(
        &'a self,
        input: serde_json::Value,
        context: &'a Context,
    ) -> Pin<Box<dyn Stream<Item = StreamOutput> + Send + 'a>> {
        // Streaming path is policy-free.
        self.inner.stream(input, context)
    }

    async fn call(
        &self,
        input: serde_json::Value,
        context: &Context,
    ) -> ToolOutput {
        let budget = self.budget.read().clone();
        if let Some(_exceeded) = check_budget(&budget) {
            return ToolOutput::error(format!(
                "budget exceeded: kind={:?}, used={}, limit={}",
                _exceeded.kind, _exceeded.used, _exceeded.limit
            ));
        }
        self.inner.call(input, context).await
    }
}

impl std::fmt::Debug for BudgetTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BudgetTool")
            .field("inner", &self.inner.name())
            .field("budget", &self.budget())
            .finish()
    }
}

/// Mirror of [`crate::provider::check_budget`] — local copy so the
/// tool wrapper doesn't pull in the provider module's private
/// symbols.
fn check_budget(budget: &Budget) -> Option<crate::budget::BudgetExceeded> {
    use crate::budget::BudgetKind;
    if let Some(t) = budget.tokens.as_ref()
        && t.exceeded()
    {
        return Some(crate::budget::BudgetExceeded {
            kind: BudgetKind::Tokens,
            used: t.used(),
            limit: t.limit,
        });
    }
    if let Some(l) = budget.latency.as_ref()
        && l.exceeded()
    {
        return Some(crate::budget::BudgetExceeded {
            kind: BudgetKind::Latency,
            used: l.elapsed().as_secs(),
            limit: l.limit.as_secs(),
        });
    }
    if let Some(c) = budget.cost.as_ref()
        && c.exceeded()
    {
        return Some(crate::budget::BudgetExceeded {
            kind: BudgetKind::Cost,
            used: c.used_cents(),
            limit: c.limit_cents,
        });
    }
    None
}

// Reference unused import so rustfmt doesn't drop it.
#[allow(unused_imports)]
use crate::BudgetKind as _UnusedKind;
