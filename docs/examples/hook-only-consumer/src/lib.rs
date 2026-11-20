//! # A hook plugin whose only normal dependency is `uva-hook`
//!
//! The crate implements a [`Gate`] on the `tool/pre-execute` mount
//! point, registers it through [`HookRegistry`], and reads the tool
//! name, the call id and the arguments off the [`HookEvent::BeforeTool`]
//! payload. A registration can veto the call, and dropping the
//! [`HookRegistration`] withdraws it.
//!
//! The mechanical half of the proof lives outside the library: the
//! harness appears **only** in `[dev-dependencies]`, and
//! `scripts/check-hook-external.sh` runs `cargo tree -e normal` inside
//! this crate to assert the normal tree carries no `uva-harness`. The
//! tests in `tests/interception.rs` drive a real run through that
//! dev-only harness: they assert the handler saw the call, a block
//! short-circuits the tool, a dropped registration stops firing, and
//! an out-of-tree observer receives the `before_run` point.
//!
//! Why this shape is the criterion — and why publishing the mount
//! points was never enough — is recorded in
//! `docs/adr/0004-hook-contract-sinks-below-harness.md` and
//! `docs/adr/0028-one-interception-table.md`.

use std::{
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use uva_hook::{
    Gate,
    HookContext,
    HookDecision,
    HookEvent,
    HookRegistration,
    HookRegistry,
    MountPoint,
    RegistrationOrigin,
    async_trait,
};

/// One tool call the plugin observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeenCall {
    /// The tool the model asked for.
    pub tool_name: String,
    /// The provider-assigned call id.
    pub call_id: String,
    /// The call's raw arguments, in their JSON text form.
    pub arguments: String,
}

/// The calls this plugin has seen, in order.
#[derive(Default)]
pub struct CallLog {
    calls: Mutex<Vec<SeenCall>>,
}

impl CallLog {
    /// An empty log.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The calls recorded so far.
    #[must_use]
    pub fn calls(&self) -> Vec<SeenCall> {
        lock(&self.calls).clone()
    }

    fn record(&self, call: SeenCall) {
        lock(&self.calls).push(call);
    }
}

/// The sample gate: it records every tool call and can deny one tool
/// by name.
pub struct CallGate {
    log: Arc<CallLog>,
    deny: Option<String>,
}

impl CallGate {
    /// A gate that observes every call and allows it.
    #[must_use]
    pub fn allowing(log: Arc<CallLog>) -> Self {
        Self { log, deny: None }
    }

    /// A gate that observes every call and blocks `tool`.
    #[must_use]
    pub fn denying(log: Arc<CallLog>, tool: impl Into<String>) -> Self {
        Self {
            log,
            deny: Some(tool.into()),
        }
    }
}

#[async_trait]
impl Gate for CallGate {
    fn name(&self) -> &str {
        "sample-call-gate"
    }

    async fn check(
        &self,
        event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> Result<HookDecision, String> {
        let HookEvent::BeforeTool {
            tool_name,
            call_id,
            arguments,
        } = event
        else {
            return Ok(HookDecision::Allow);
        };
        self.log.record(SeenCall {
            tool_name: tool_name.clone(),
            call_id: call_id.clone(),
            arguments: arguments.to_string(),
        });
        match &self.deny {
            Some(denied) if denied == tool_name => Ok(HookDecision::Block {
                reason: format!("the sample plugin denies `{tool_name}`"),
            }),
            _ => Ok(HookDecision::Allow),
        }
    }
}

/// Register `gate` on `tool/pre-execute`.
///
/// The registration declares itself a plugin lane row, so a panic
/// skips one pass and keeps the row. Dropping the returned handle
/// withdraws the registration from every pass that shares `registry`.
pub fn install(registry: &HookRegistry, gate: CallGate) -> HookRegistration {
    registry.register_gate_at(
        MountPoint::ToolPre,
        Arc::new(gate),
        RegistrationOrigin::Plugin,
    )
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
