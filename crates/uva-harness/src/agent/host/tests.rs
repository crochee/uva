//! The host's own tables: what `default_policy` installs, and the
//! tool bindings.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use uva_hook::{HookRegistry, MountPoint, RegistrationOrigin};
use uva_steering::{HookDecision, HookEvent};
use uva_tool::{Tool, ToolOutput, ToolRegistry};

use super::*;
use crate::seams::Seams;

// An observer that counts its own dispatches.
struct CountingHook {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Hook for CountingHook {
    fn name(&self) -> &str {
        "counting"
    }

    async fn observe(
        &self,
        _event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> HookDecision {
        self.calls.fetch_add(1, Ordering::SeqCst);
        HookDecision::Allow
    }
}

// The production policy is installed, not just built: every
// registration has to survive the call that made it, or a deployment
// that opts into `default_policy` runs with no guards at all.
#[test]
fn default_policy_installs_the_production_policy() {
    let registry = HookRegistry::new();
    let host = Host::default_policy(std::env::temp_dir(), registry.clone());
    assert_eq!(
        host.registration_count(),
        9,
        "5 guards + 3 hints + 1 tracker"
    );
    for mount in MountPoint::ALL {
        assert_eq!(
            registry.has_entries_at(mount),
            mount.is_event(),
            "the logging hook is registered at every event point and \
             nowhere else: {mount}"
        );
    }
    assert!(
        host.diagnostics().is_empty(),
        "the production policy installs without refusals"
    );
}

// The registry and the host are one table: a registration made on the
// table before the host exists is dispatched by the host's seam layer,
// and a host clone reads the same rows.
#[tokio::test]
async fn the_host_dispatches_the_registry_it_was_built_over() {
    let (tx, _rx) = futures::channel::mpsc::unbounded();
    let sink = crate::agent::sink::EventSink::new(Arc::new(tx), None);
    let state = uva_context::AgentState::with_window(1_000);
    let cancel = uva_core::AtomicCancelToken::shared();
    let ctx = HookContext {
        state: &state,
        cancel: &cancel,
    };

    let registry = HookRegistry::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let _registration = registry.register_hook_at(
        MountPoint::BeforeRun,
        Arc::new(CountingHook {
            calls: Arc::clone(&calls),
        }),
        RegistrationOrigin::Plugin,
    );
    let seams =
        Seams::new(Host::default_policy(std::env::temp_dir(), registry));

    seams.run_started(&ctx, &sink, "a", 0).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the run's seam layer reads the deployment's table"
    );
}

// A tool the host can register, distinguishable by its description.
struct NamedTool {
    name: &'static str,
    description: &'static str,
}

#[async_trait]
impl Tool for NamedTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        self.description
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    async fn call(
        &self,
        _input: serde_json::Value,
        _context: &uva_tool::Context,
    ) -> ToolOutput {
        ToolOutput::text("ok")
    }
}

#[test]
fn a_withdrawn_tool_leaves_the_registry_and_is_no_longer_callable() {
    let registry = Arc::new(ToolRegistry::new());
    let mut host = Host::new();
    let registration = host.tool(Arc::new(NamedTool {
        name: "hosted",
        description: "the host's tool",
    }));
    assert!(host.bind(Arc::clone(&registry)).is_empty());

    assert!(
        registry.contains("hosted"),
        "the bound host's tool is callable"
    );
    assert!(
        registry.get("hosted").is_some(),
        "the registry resolves the host's tool"
    );

    drop(registration);

    assert!(
        !registry.contains("hosted"),
        "a withdrawn tool must not stay in the registry"
    );
    assert!(
        registry.get("hosted").is_none(),
        "a withdrawn tool must not stay callable"
    );
}

#[test]
fn a_tool_added_to_a_bound_host_reaches_the_registry() {
    let registry = Arc::new(ToolRegistry::new());
    let mut host = Host::new();
    assert!(host.bind(Arc::clone(&registry)).is_empty());

    let registration = host.tool(Arc::new(NamedTool {
        name: "late",
        description: "a late tool",
    }));

    assert!(
        registry.contains("late"),
        "a tool registered after the binding is callable too"
    );

    drop(registration);

    assert!(!registry.contains("late"));
}

#[test]
fn a_withdrawn_tool_leaves_the_other_entry_under_the_name_alone() {
    let registry = Arc::new(ToolRegistry::new());
    // Another party already filed a tool under the name.
    registry.register_entry(uva_tool::ToolEntry::new(Arc::new(NamedTool {
        name: "shared",
        description: "the other party's tool",
    })));
    let mut host = Host::new();
    let registration = host.tool(Arc::new(NamedTool {
        name: "shared",
        description: "the host's tool",
    }));
    assert!(host.bind(Arc::clone(&registry)).is_empty());

    drop(registration);

    // The other party's entry survives: the withdrawal names the
    // host's own registration, not the name.
    let entry = registry
        .get("shared")
        .expect("the other party's tool must survive the withdrawal");
    assert_eq!(
        entry.tool_instance().description(),
        "the other party's tool",
        "the surviving entry must be the other party's"
    );
}

// R4: one host is one policy. Every agent over a deployment binds
// the same registry, so the repeated bind is a no-op — and a second,
// *different* registry is refused rather than silently stranding the
// tokens the first one holds.
#[test]
fn rebinding_the_same_registry_is_a_no_op() {
    let registry = Arc::new(ToolRegistry::new());
    let mut host = Host::new();
    let registration = host.tool(Arc::new(NamedTool {
        name: "hosted",
        description: "the host's tool",
    }));

    assert!(host.bind(Arc::clone(&registry)).is_empty());
    assert!(host.bind(Arc::clone(&registry)).is_empty());

    assert!(
        host.diagnostics().is_empty(),
        "the second bind of the same registry is not a diagnostic"
    );
    assert!(registry.contains("hosted"));
    drop(registration);
    assert!(
        !registry.contains("hosted"),
        "the single binding is the one the withdrawal targets"
    );
}

#[test]
fn rebinding_a_different_registry_keeps_the_original_binding() {
    let first = Arc::new(ToolRegistry::new());
    let second = Arc::new(ToolRegistry::new());
    let mut host = Host::new();
    let registration = host.tool(Arc::new(NamedTool {
        name: "hosted",
        description: "the host's tool",
    }));
    assert!(host.bind(Arc::clone(&first)).is_empty());

    let refusals = host.bind(Arc::clone(&second));

    assert_eq!(refusals.len(), 1, "a second, different registry is refused");
    assert_eq!(host.diagnostics().len(), 1, "and recorded once on the host");
    assert_eq!(
        host.diagnostics()[0].kind,
        DiagnosticKind::RegistryAlreadyBound
    );
    assert!(
        second.descriptors().is_empty(),
        "the refused registry receives nothing"
    );

    // A tool registered after the refusal joins the original
    // registry, because that is still the binding the host holds.
    let late = host.tool(Arc::new(NamedTool {
        name: "late",
        description: "a late tool",
    }));
    assert!(first.contains("late"));
    assert!(!second.contains("late"));

    drop(late);
    assert!(!first.contains("late"));
    drop(registration);
    assert!(!first.contains("hosted"));
}

// The refusals a bind reports are that call's own, not a slice of the
// host's diagnostic history. A consumer that folds the returned list
// therefore cannot re-report an earlier bind's refusal, and the
// history's cap cannot drop a refusal while a caller still needs it.
#[test]
fn bind_reports_only_the_refusals_of_that_call() {
    let registry = Arc::new(ToolRegistry::new());
    let mut host = Host::new();
    let held = host.tool(Arc::new(NamedTool {
        name: "hosted",
        description: "the host's tool",
    }));
    assert!(host.bind(Arc::clone(&registry)).is_empty());
    assert!(
        host.bind(Arc::clone(&registry)).is_empty(),
        "a repeated bind of the same registry refuses nothing"
    );

    let elsewhere = Arc::new(ToolRegistry::new());
    let refused = host.bind(Arc::clone(&elsewhere));
    assert_eq!(refused.len(), 1, "one refusal for the one bind");
    assert_eq!(refused[0].kind, DiagnosticKind::RegistryAlreadyBound);
    assert_eq!(
        host.diagnostics().len(),
        1,
        "the history holds the same single refusal"
    );
    drop(held);
}

// A pending tool the registry will not take is reported by the bind
// itself, so the caller learns it without reading the history.
#[test]
fn bind_reports_a_tool_the_registry_refuses() {
    let registry = Arc::new(ToolRegistry::new());
    assert!(
        registry.register_core_entry(uva_tool::ToolEntry::new(Arc::new(
            NamedTool {
                name: "builtin",
                description: "the core tool",
            }
        )))
    );
    let mut host = Host::new();
    let held = host.tool(Arc::new(NamedTool {
        name: "builtin",
        description: "the host's tool",
    }));

    let refusals = host.bind(Arc::clone(&registry));

    assert_eq!(refusals.len(), 1);
    assert_eq!(refusals[0].kind, DiagnosticKind::ToolNameConflict);
    assert_eq!(refusals[0].source, "builtin");
    assert!(refusals[0].detail.contains("core tool"));
    assert_eq!(host.diagnostics(), refusals, "the history agrees");
    drop(held);
}
