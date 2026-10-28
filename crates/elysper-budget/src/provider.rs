//! [`BudgetProvider`] — the `ModelProvider` wrapper.

use std::sync::Arc;

use elysper_provider::{
    CompletionRequest,
    CompletionResponse,
    ModelConfig,
    ModelProvider,
    ProviderConfig,
    StreamChunk,
};

use crate::budget::{Budget, BudgetExceeded, BudgetKind};

/// `ModelProvider` wrapper that enforces a [`Budget`]. The wrapper
/// checks the budget *before* the call and *after* a successful
/// call (to record cost / tokens). The cache decision lives on
/// `complete`; streaming passes through.
pub struct BudgetProvider {
    inner: Arc<dyn ModelProvider>,
    budget: parking_lot::RwLock<Budget>,
    /// Counter for soft-finalize responses.
    pub(crate) trip_count: parking_lot::Mutex<u64>,
}

impl BudgetProvider {
    pub fn new(inner: Arc<dyn ModelProvider>, budget: Budget) -> Self {
        Self {
            inner,
            budget: parking_lot::RwLock::new(budget),
            trip_count: parking_lot::Mutex::new(0),
        }
    }

    /// Snapshot of the live budget.
    #[must_use]
    pub fn budget(&self) -> Budget {
        self.budget.read().clone()
    }

    /// Hot-reload budget.
    pub fn set_budget(&self, budget: Budget) {
        *self.budget.write() = budget;
    }

    /// Number of trips observed since construction.
    #[must_use]
    pub fn trip_count(&self) -> u64 {
        *self.trip_count.lock()
    }
}

#[async_trait::async_trait]
impl ModelProvider for BudgetProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), elysper_core::Error> {
        // Initialisation is the inner provider's responsibility; see
        // the rationale in `elysper-semantic-cache::wrapper` /
        // `elysper-resilience::provider`.
        Ok(())
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn model_config(&self) -> ModelConfig {
        self.inner.model_config()
    }

    fn tier(&self) -> elysper_provider::ModelTier {
        self.inner.tier()
    }

    fn supports_inline_cache_hints(&self) -> bool {
        self.inner.supports_inline_cache_hints()
    }

    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, elysper_core::Error> {
        // Pre-call budget check.
        let budget = self.budget.read().clone();
        if let Some(exceeded) = check_budget(&budget) {
            *self.trip_count.lock() += 1;
            return Err(budget_error(exceeded));
        }

        let response = self.inner.complete(request).await?;

        // Post-call: record cost / tokens / latency.
        let mut budget = self.budget.write();
        if let Some(t) = budget.tokens.as_ref() {
            let delta = (response.usage.prompt_tokens
                + response.usage.completion_tokens)
                as u64;
            t.add(delta);
        }
        if let Some(c) = budget.cost.as_ref() {
            c.record(
                &response.model,
                response.usage.prompt_tokens as u64,
                response.usage.completion_tokens as u64,
            );
        }
        if let Some(l) = budget.latency.as_mut() {
            l.reset();
        }
        // Re-check after the call.
        if let Some(exceeded) = check_budget(&budget) {
            *self.trip_count.lock() += 1;
            return Err(budget_error(exceeded));
        }
        Ok(response)
    }

    async fn complete_with_stream(
        &self,
        request: CompletionRequest,
        cancel_token: Option<Arc<dyn elysper_core::CancelToken>>,
        on_delta: Box<dyn FnMut(StreamChunk) + Send>,
    ) -> Result<CompletionResponse, elysper_core::Error> {
        // Streaming path is policy-free.
        self.inner
            .complete_with_stream(request, cancel_token, on_delta)
            .await
    }

    async fn embed(
        &self,
        texts: Vec<String>,
    ) -> Result<Vec<Vec<f64>>, elysper_core::Error> {
        self.inner.embed(texts).await
    }
}

impl std::fmt::Debug for BudgetProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BudgetProvider")
            .field("inner", &self.inner.name())
            .field("budget", &self.budget())
            .finish()
    }
}

/// Check the budget; return the first tripped budget (priority:
/// tokens → latency → cost).
fn check_budget(budget: &Budget) -> Option<BudgetExceeded> {
    if let Some(t) = budget.tokens.as_ref()
        && t.exceeded()
    {
        return Some(BudgetExceeded {
            kind: BudgetKind::Tokens,
            used: t.used(),
            limit: t.limit,
        });
    }
    if let Some(l) = budget.latency.as_ref()
        && l.exceeded()
    {
        return Some(BudgetExceeded {
            kind: BudgetKind::Latency,
            used: l.elapsed().as_secs(),
            limit: l.limit.as_secs(),
        });
    }
    if let Some(c) = budget.cost.as_ref()
        && c.exceeded()
    {
        return Some(BudgetExceeded {
            kind: BudgetKind::Cost,
            used: c.used_cents(),
            limit: c.limit_cents,
        });
    }
    None
}

/// Convert a tripped budget into an [`elysper_core::Error`]. The
/// payload is the underlying error; the wrapper decides whether
/// the run finalizes or aborts based on `on_exceeded`.
fn budget_error(exceeded: BudgetExceeded) -> elysper_core::Error {
    let kind = match exceeded.kind {
        BudgetKind::Tokens => "tokens",
        BudgetKind::Latency => "latency",
        BudgetKind::Cost => "cost",
    };
    elysper_core::Error::Internal {
        message: format!(
            "budget exceeded (kind={kind}, used={}, limit={})",
            exceeded.used, exceeded.limit
        ),
    }
}
