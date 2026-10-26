//! [`DelegatingTool`] — the harness's seam for tools that need loop
//! internals (event sink, cancel token, depth bookkeeping, tool
//! context), and [`Delegators`], the name-keyed table the loop
//! dispatches them through.
use std::{collections::HashMap, sync::Arc};

use futures::{channel::mpsc, future::BoxFuture};
use synthia_core::CancelToken;
use synthia_provider::{ToolDefinition, ToolUse};
use synthia_tool::{ExecutionMode, ToolAnnotations, ToolOutput};

use crate::events::AgentEvent;

pub struct DelegationCall<'a> {
    pub call: &'a ToolUse,
    pub cancel: Arc<dyn CancelToken>,
    pub depth: usize,
    /// The run's tool context — the same one a registry tool receives
    /// from `Tool::call` (workspace root, session id, output bound). The
    /// loop builds it once per run, so `session_id` is empty here; a
    /// delegator that needs the child's session resolves it itself (the
    /// `task` delegator goes through `subagent_writers`).
    pub context: &'a synthia_tool::Context,
    /// Forward an event into the parent run's stream — it reaches the caller like a loop-owned event. The sender owns its
    /// channel: a plugin that spawns child tasks and forgets the closure can still emit until the stream is dropped.
    pub emit: mpsc::UnboundedSender<AgentEvent>,
    /// Writer source for the sessions this plugin's children run
    /// in. The `task` delegator resolves each child's session
    /// writer through it before starting the child run. `None`
    /// when the host installed no source: children then stream
    /// live but persist nothing.
    pub subagent_writers: Option<Arc<dyn crate::input::SubagentWriters>>,
}

/// A delegating tool the loop dispatches ahead of the registry. Install with
/// [`ReActAgent::with_delegator`](crate::agent::ReActAgent::with_delegator) or
/// [`Host::delegator`](crate::agent::host::Host::delegator); its definitions are advertised beside the registry's (subject to
/// restriction), and a name one of them declares routes to [`execute`](DelegatingTool::execute).
/// [`mode`](DelegatingTool::mode) defaults to sequential because a delegator
/// spawns whole runs: sharing the parallel bucket with a registry tool is a
/// decision the plugin makes, not a default.
pub trait DelegatingTool: Send + Sync {
    /// The definitions to advertise; their names are what dispatch resolves.
    fn definitions(&self) -> Vec<ToolDefinition>;

    /// Whether the calls may share the parallel bucket.
    fn mode(&self) -> ExecutionMode {
        ExecutionMode::Sequential
    }

    /// Hints for the definitions that carry none of their own.
    fn annotations(&self) -> Option<ToolAnnotations> {
        None
    }

    fn execute(
        &self,
        call: DelegationCall<'_>,
    ) -> BoxFuture<'static, ToolOutput>;
}

/// The run's delegators, indexed by the tool names they advertise.
///
/// Dispatch is one map lookup rather than a scan of every installed plugin
/// asking whether it claims the name: a plugin's names are fixed when it is
/// installed, so the index is built there and the lookup stays O(1).
/// `installed` keeps install order — definitions are collected from it once
/// per run, so a plugin advertising several names contributes them once —
/// and `by_name` maps an advertised name to that list's index.
#[derive(Default)]
pub(crate) struct Delegators {
    installed: Vec<Arc<dyn DelegatingTool>>,
    by_name: HashMap<String, usize>,
}

impl Delegators {
    /// The table for a list whose names already passed
    /// [`install`](Delegators::install): an agent's own installs never
    /// conflict with each other.
    ///
    /// `ToolCaps::delegators` is a public field, so a caller can bypass
    /// `with_delegator` and push two delegators with the same advertised
    /// name. Debug builds say so here rather than letting the second one
    /// vanish quietly.
    pub(crate) fn from_installed(
        installed: &[Arc<dyn DelegatingTool>],
    ) -> Self {
        let mut table = Self::default();
        for tool in installed {
            // Evaluated in every profile; only the assertion is debug-only.
            let installed = table.install(Arc::clone(tool));
            debug_assert!(
                installed.is_ok(),
                "installed delegators must advertise distinct names: {installed:?}"
            );
        }
        table
    }

    /// Install a delegator, rejecting a name another one already advertises:
    /// same-name dispatch has no defined precedence. `Err` carries the
    /// conflicting name.
    pub(crate) fn install(
        &mut self,
        tool: Arc<dyn DelegatingTool>,
    ) -> Result<(), String> {
        let definitions = definitions_of(&tool);
        if let Some(taken) = self.conflict(&definitions) {
            return Err(taken);
        }
        let index = self.installed.len();
        for def in definitions {
            self.by_name.insert(def.name, index);
        }
        self.installed.push(tool);
        Ok(())
    }

    /// The advertised name `definitions` would collide with, if any.
    pub(crate) fn conflict(
        &self,
        definitions: &[ToolDefinition],
    ) -> Option<String> {
        definitions
            .iter()
            .find(|def| self.by_name.contains_key(&def.name))
            .map(|def| def.name.clone())
    }

    /// Merge a delegator the host registered: the first owner of a name keeps
    /// it, and a delegator already installed (the same `Arc`) is skipped —
    /// installing one delegator through both the agent and its host leaves one
    /// entry, never two.
    pub(crate) fn merge(&mut self, tool: Arc<dyn DelegatingTool>) {
        if self.installed.iter().any(|t| Arc::ptr_eq(t, &tool)) {
            return;
        }
        // A host registration never takes a name the table already owns: an
        // agent install wins, and among host rows the earlier registration
        // wins. This is the one install path that can drop a live
        // registration, so name it instead of dropping it silently.
        if let Err(shadowed) = self.install(tool) {
            tracing::warn!(
                tool = %shadowed,
                "delegator shadowed by an earlier delegator; the host still \
                 lists the registration, but the loop dispatches the name's \
                 existing owner"
            );
        }
    }

    /// The delegator advertising `name`.
    pub(crate) fn get(&self, name: &str) -> Option<&Arc<dyn DelegatingTool>> {
        self.by_name.get(name).map(|index| &self.installed[*index])
    }

    /// The execution mode `name` declares, `None` when no delegator owns it.
    pub(crate) fn mode(&self, name: &str) -> Option<ExecutionMode> {
        self.get(name).map(|tool| tool.mode())
    }

    /// Every advertised definition in install order; a definition carrying no
    /// annotations of its own takes the delegator's.
    pub(crate) fn definitions(&self) -> Vec<ToolDefinition> {
        self.installed.iter().flat_map(definitions_of).collect()
    }
}

fn definitions_of(tool: &Arc<dyn DelegatingTool>) -> Vec<ToolDefinition> {
    let annotations = tool.annotations();
    let mut definitions = tool.definitions();
    for definition in &mut definitions {
        if definition.annotations.is_none() {
            definition.annotations = annotations.map(Into::into);
        }
    }
    definitions
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) struct EchoInterceptor {
        pub name: &'static str,
        pub mode: ExecutionMode,
        pub annotations: Option<ToolAnnotations>,
        pub seen: Option<Arc<parking_lot::Mutex<Vec<String>>>>,
    }

    impl EchoInterceptor {
        pub(crate) fn new(name: &'static str) -> Self {
            Self {
                name,
                mode: ExecutionMode::Sequential,
                annotations: None,
                seen: None,
            }
        }
    }

    impl DelegatingTool for EchoInterceptor {
        fn definitions(&self) -> Vec<ToolDefinition> {
            vec![ToolDefinition {
                name: self.name.to_string(),
                description: "echo the input back".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {}
                }),
                cache_control: None,
                annotations: None,
            }]
        }

        fn mode(&self) -> ExecutionMode {
            self.mode
        }

        fn annotations(&self) -> Option<ToolAnnotations> {
            self.annotations
        }

        fn execute(
            &self,
            call: DelegationCall<'_>,
        ) -> BoxFuture<'static, ToolOutput> {
            let name = self.name;
            let input = call.call.input.clone();
            if let Some(seen) = &self.seen {
                seen.lock().push(format!(
                    "{name}:{}:{}",
                    call.depth,
                    call.context.workspace_root.display()
                ));
            }
            Box::pin(async move {
                ToolOutput::text(format!("intercepted:{}:{}", name, input))
            })
        }
    }

    fn find(builder: impl FnOnce(&mut Delegators)) -> Delegators {
        let mut table = Delegators::default();
        builder(&mut table);
        table
    }

    #[test]
    fn delegator_is_found_by_name_not_by_linear_claims_scan() {
        let first = Arc::new(EchoInterceptor::new("echo_one")) as Arc<_>;
        let second = Arc::new(EchoInterceptor::new("echo_two")) as Arc<_>;
        let table = find(|table| {
            table.install(first).expect("distinct names");
            table.install(second).expect("distinct names");
        });

        assert_eq!(table.definitions().len(), 2);
        let hit = table.get("echo_two").expect("the name resolves");
        assert_eq!(hit.definitions()[0].name, "echo_two");
        let other = table.get("echo_one").expect("the other name resolves");
        assert!(
            !Arc::ptr_eq(hit, other),
            "each name resolves to the delegator that owns it"
        );
        assert!(table.get("echo_three").is_none());
    }

    #[test]
    fn the_same_delegator_merged_twice_stays_one_entry() {
        let tool = Arc::new(EchoInterceptor::new("echo_me")) as Arc<_>;
        let mut table = Delegators::default();
        table.install(Arc::clone(&tool)).expect("first install");
        table.merge(Arc::clone(&tool));

        assert_eq!(table.definitions().len(), 1);
        assert!(Arc::ptr_eq(table.get("echo_me").expect("resolves"), &tool));
    }

    #[test]
    fn a_merged_delegator_never_takes_an_owned_name() {
        let mut table = Delegators::default();
        table
            .install(Arc::new(EchoInterceptor::new("shared")) as Arc<_>)
            .expect("first install");
        let shadowed = Arc::new(EchoInterceptor::new("shared")) as Arc<_>;
        table.merge(Arc::clone(&shadowed));

        assert_eq!(table.definitions().len(), 1);
        assert!(!Arc::ptr_eq(
            table.get("shared").expect("resolves"),
            &shadowed
        ));
    }

    #[test]
    fn a_delegator_mode_defaults_to_sequential_and_is_read_by_name() {
        let mut table = Delegators::default();
        table
            .install(Arc::new(EchoInterceptor::new("serial")) as Arc<_>)
            .expect("first install");
        table
            .install(Arc::new(EchoInterceptor {
                mode: ExecutionMode::Parallel,
                ..EchoInterceptor::new("concurrent")
            }) as Arc<_>)
            .expect("distinct names");

        assert_eq!(table.mode("serial"), Some(ExecutionMode::Sequential));
        assert_eq!(table.mode("concurrent"), Some(ExecutionMode::Parallel));
        assert_eq!(table.mode("registry_tool"), None);
        assert_eq!(
            EchoInterceptor::new("plain").mode(),
            ExecutionMode::Sequential
        );
    }

    #[test]
    fn a_delegator_annotation_fills_a_definition_that_has_none() {
        let annotations = ToolAnnotations {
            read_only_hint: Some(true),
            ..ToolAnnotations::default()
        };
        let mut table = Delegators::default();
        table
            .install(Arc::new(EchoInterceptor {
                annotations: Some(annotations),
                ..EchoInterceptor::new("annotated")
            }) as Arc<_>)
            .expect("first install");

        let definitions = table.definitions();
        assert_eq!(definitions.len(), 1);
        assert_eq!(
            definitions[0].annotations.as_ref().unwrap().read_only_hint,
            Some(true)
        );
    }

    #[test]
    fn with_delegator_rejects_duplicate_name() {
        use std::sync::Arc;
        let provider: Arc<dyn synthia_provider::ModelProvider> =
            Arc::new(synthia_test_support::FakeProvider::new(Vec::new()));
        let result = crate::agent::re_act::ReActAgent::new(
            provider,
            Arc::new(synthia_tool::ToolRegistry::new()),
        )
        .with_delegator(Arc::new(EchoInterceptor::new("echo_me")))
        .and_then(|a| {
            a.with_delegator(Arc::new(EchoInterceptor::new("echo_me")))
        });
        assert!(result.is_err());
    }

    #[test]
    fn with_delegator_accepts_distinct_names() {
        use std::sync::Arc;
        let provider: Arc<dyn synthia_provider::ModelProvider> =
            Arc::new(synthia_test_support::FakeProvider::new(Vec::new()));
        let result = crate::agent::re_act::ReActAgent::new(
            provider,
            Arc::new(synthia_tool::ToolRegistry::new()),
        )
        .with_delegator(Arc::new(EchoInterceptor::new("echo_me")))
        .and_then(|a| {
            a.with_delegator(Arc::new(EchoInterceptor::new("echo_other")))
        });
        assert!(result.is_ok());
    }
}
