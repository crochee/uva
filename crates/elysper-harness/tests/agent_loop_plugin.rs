//! `agentLoop` as a kernel plugin, plus the seam layer the loop
//! dispatches through.
//!
//! The four seam names live in [`elysper_harness::seams`]; these tests
//! register policy on a [`Host`] and drive it through [`Seams`], so the
//! assertions cover the same path the loop takes — never the host's
//! dispatch methods directly.

use std::{
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc,
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use elysper_context::AgentState;
use elysper_core::{CancelToken, Error};
use elysper_harness::{
    Agent,
    AgentEvent,
    AgentInput,
    DelegatingTool,
    EventSink,
    PromptContext,
    PromptSection,
    ReActAgent,
    SystemEvent,
    agent::host::{
        Gate,
        Hook,
        HookContext,
        Host,
        RequestTransformer,
        ResultTransformer,
    },
    default_descriptor,
    plugin::{
        AgentLoopFactory,
        AgentLoopPlugin,
        KEY_AGENT_LOOP,
        ReActAgentFactory,
        RunConfig,
    },
    seams::{self, Seams},
};
use elysper_kernel::{
    Context,
    EffectHandle,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    PluginManifest,
    RuntimeKind,
    mount_all,
};
use elysper_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    ContentPart,
    ModelConfig,
    ModelProvider,
    ProviderConfig,
    Role,
    ToolDefinition,
    ToolUse,
};
use elysper_steering::{HookDecision, HookEvent, RequestParams};
use elysper_test_support::FakeTool;
use elysper_tool::{Tool, ToolEntry, ToolOutput, ToolRegistry};
use futures::{Stream, StreamExt, channel::mpsc, future::BoxFuture};
use serde_json::json;
use tokio_util::sync::CancellationToken;

// --- fixtures ---------------------------------------------------------

/// A factory that never creates an agent: the service tests only need a
/// stable pointer to prove `agentLoop` resolves to the installed value.
struct StubLoop;

impl AgentLoopFactory for StubLoop {
    fn create(&self, _run: &RunConfig) -> Arc<dyn Agent> {
        unreachable!("the stub factory never creates an agent")
    }
}

/// Provide the three keys `agent-loop` injects, for tests that only
/// exercise mounting and lifecycle.
fn provide_dependencies(
    ctx: &Context,
) -> (EffectHandle, EffectHandle, EffectHandle) {
    let llm = ctx.provide("llm", ()).expect("dummy `llm` provider");
    let tools = ctx.provide("tools", ()).expect("dummy `tools` provider");
    let sessions = ctx
        .provide("sessions", ())
        .expect("dummy `sessions` provider");
    (llm, tools, sessions)
}

fn loop_plugin(factory: Arc<dyn AgentLoopFactory>) -> Arc<dyn KernelPlugin> {
    Arc::new(AgentLoopPlugin::new(factory))
}

fn loop_manifest() -> PluginManifest {
    let mut manifest =
        PluginManifest::new("agent-loop", PLUGIN_PROTOCOL_VERSION);
    manifest.provides = vec![KEY_AGENT_LOOP.to_string()];
    manifest.inject = vec![
        "llm".to_string(),
        "tools".to_string(),
        "sessions".to_string(),
    ];
    manifest
}

enum Script {
    ToolThenText,
    Text(&'static str),
}

/// A scripted provider: captures every request, and follows its script —
/// ask for the `echo` tool then answer, or answer the same text every
/// time.
struct ScriptedProvider {
    calls: AtomicUsize,
    captured: Mutex<Vec<CompletionRequest>>,
    script: Script,
}

impl ScriptedProvider {
    fn tool_then_text() -> Self {
        Self::with_script(Script::ToolThenText)
    }

    fn text(answer: &'static str) -> Self {
        Self::with_script(Script::Text(answer))
    }

    fn with_script(script: Script) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            captured: Mutex::new(Vec::new()),
            script,
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn requests(&self) -> std::sync::MutexGuard<'_, Vec<CompletionRequest>> {
        self.captured.lock().expect("captured requests")
    }
}

#[async_trait]
impl ModelProvider for ScriptedProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "scripted"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "scripted-1".to_string(),
            provider: "scripted".to_string(),
            context_window: 8_192,
            max_output_tokens: 1_024,
            supports_tools: true,
            supports_streaming: false,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, Error> {
        self.captured.lock().expect("capture").push(request);
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let content = match self.script {
            Script::Text(answer) => Content::text(answer),
            Script::ToolThenText if call == 0 => {
                Content::Single(ContentPart::ToolUse(ToolUse {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    input: json!({ "text": "hi" }),
                }))
            }
            Script::ToolThenText => Content::text("done"),
        };
        Ok(CompletionResponse {
            content,
            ..CompletionResponse::default()
        })
    }
}

struct BlockEcho;

#[async_trait]
impl Gate for BlockEcho {
    fn name(&self) -> &str {
        "block-echo"
    }

    async fn check(
        &self,
        event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> Result<HookDecision, String> {
        match event {
            HookEvent::BeforeTool { tool_name, .. } if tool_name == "echo" => {
                Ok(HookDecision::Block {
                    reason: "test veto".to_string(),
                })
            }
            _ => Ok(HookDecision::Allow),
        }
    }
}

/// A gate that short-circuits the step before the model is asked.
///
/// `BeforeStep` accepts `Terminate` in the steering vocabulary (`Block`
/// is tool-scoped); the seam layer treats both as the step's
/// short-circuit.
struct BlockStep;

#[async_trait]
impl Gate for BlockStep {
    fn name(&self) -> &str {
        "block-step"
    }

    async fn check(
        &self,
        event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> Result<HookDecision, String> {
        match event {
            HookEvent::BeforeStep { .. } => Ok(HookDecision::Terminate {
                reason: "blocked at step pre".to_string(),
            }),
            _ => Ok(HookDecision::Allow),
        }
    }
}

struct AfterStepProbe(Arc<AtomicUsize>);

#[async_trait]
impl Hook for AfterStepProbe {
    fn name(&self) -> &str {
        "after-step-probe"
    }

    async fn observe(
        &self,
        event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> HookDecision {
        if matches!(event, HookEvent::AfterStep { .. }) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
        HookDecision::Allow
    }
}

struct RewriteEcho;

#[async_trait]
impl ResultTransformer for RewriteEcho {
    fn name(&self) -> &str {
        "rewrite-echo"
    }

    async fn transform(
        &self,
        output: &mut ToolOutput,
        _ctx: &HookContext<'_>,
    ) -> Result<(), String> {
        *output = ToolOutput::text("rewritten by post-execute");
        Ok(())
    }
}

struct SetTemperature;

#[async_trait]
impl RequestTransformer for SetTemperature {
    fn name(&self) -> &str {
        "set-temperature"
    }

    async fn transform(
        &self,
        params: &mut RequestParams,
        _ctx: &HookContext<'_>,
    ) -> Result<(), String> {
        params.temperature = Some(0.25);
        params.model = "rewritten-model".to_string();
        Ok(())
    }
}

struct RewriteResult;

#[async_trait]
impl ResultTransformer for RewriteResult {
    fn name(&self) -> &str {
        "rewrite-result"
    }

    async fn transform(
        &self,
        output: &mut ToolOutput,
        _ctx: &HookContext<'_>,
    ) -> Result<(), String> {
        *output = ToolOutput::text("rewritten by result transformer");
        Ok(())
    }
}

struct OrderHook {
    label: &'static str,
    seen: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl Hook for OrderHook {
    fn name(&self) -> &str {
        self.label
    }

    async fn observe(
        &self,
        _event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> HookDecision {
        self.seen.lock().expect("order log").push(self.label);
        HookDecision::Allow
    }
}

struct DenyGate;

#[async_trait]
impl Gate for DenyGate {
    fn name(&self) -> &str {
        "deny-gate"
    }

    async fn check(
        &self,
        _event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> Result<HookDecision, String> {
        Ok(HookDecision::Block {
            reason: "denied by the first gate".to_string(),
        })
    }
}

struct LaterGate(Arc<AtomicUsize>);

#[async_trait]
impl Gate for LaterGate {
    fn name(&self) -> &str {
        "later-gate"
    }

    async fn check(
        &self,
        _event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> Result<HookDecision, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(HookDecision::Allow)
    }
}

struct NoopDelegator;

impl DelegatingTool for NoopDelegator {
    fn definitions(&self) -> Vec<ToolDefinition> {
        Vec::new()
    }

    fn execute(
        &self,
        _call: elysper_harness::DelegationCall<'_>,
    ) -> BoxFuture<'static, ToolOutput> {
        Box::pin(async { ToolOutput::text("never runs") })
    }
}

struct DropProbe(Arc<AtomicUsize>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl Hook for DropProbe {
    fn name(&self) -> &str {
        "drop-probe"
    }

    async fn observe(
        &self,
        _event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> HookDecision {
        HookDecision::Allow
    }
}

/// A kernel plugin that provides `agentLoop` over a factory.
struct SeamPlugin {
    factory: Arc<dyn AgentLoopFactory>,
    manifest: PluginManifest,
}

impl SeamPlugin {
    fn new(factory: Arc<dyn AgentLoopFactory>) -> Self {
        Self {
            factory,
            manifest: loop_manifest(),
        }
    }
}

impl KernelPlugin for SeamPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(
        &self,
        ctx: &Context,
    ) -> Result<Vec<EffectHandle>, elysper_kernel::PluginError> {
        let effect = ctx.provide(KEY_AGENT_LOOP, Arc::clone(&self.factory))?;
        Ok(vec![effect])
    }
}

fn run_config(
    provider: Arc<dyn ModelProvider>,
    registry: Arc<ToolRegistry>,
    host: Host,
) -> RunConfig {
    RunConfig {
        provider,
        tool_registry: registry,
        workspace_root: PathBuf::from("."),
        descriptor: default_descriptor(
            "agent-loop integration test".to_string(),
        ),
        prompt_context: Arc::new(PromptContext::default()),
        max_iterations: 8,
        delegators: Vec::new(),
        tool_surface: None,
        strategy: None,
        tool_restriction: None,
        log_writer: None,
        subagent_writers: None,
        context_manager: None,
        compactor: None,
        host: Some(host),
        spawner: None,
    }
}

/// Build an agent whose loop dispatches through `seams`.
fn agent_with_seams(
    provider: Arc<dyn ModelProvider>,
    registry: Arc<ToolRegistry>,
    seams: Seams,
) -> Arc<dyn Agent> {
    Arc::new(
        ReActAgent::new(provider, registry)
            .with_workspace(".")
            .with_max_iterations(8)
            .with_seams(seams)
            .expect("bind the seam layer"),
    )
}

fn registry_with_echo() -> (Arc<ToolRegistry>, Arc<FakeTool>) {
    let tool = Arc::new(FakeTool::new("echo", "raw output"));
    let registry = Arc::new(ToolRegistry::new());
    let registered: Arc<dyn Tool> = tool.clone();
    registry.register_entry(ToolEntry::new(registered));
    (registry, tool)
}

fn seam_context<'a>(
    state: &'a AgentState,
    sink: &'a EventSink,
    cancel: &'a Arc<dyn CancelToken>,
) -> HookContext<'a> {
    HookContext {
        state,
        events: sink,
        cancel,
    }
}

fn probe_sink() -> (EventSink, mpsc::UnboundedReceiver<AgentEvent>) {
    let (tx, rx) = mpsc::unbounded::<AgentEvent>();
    (EventSink::new(Arc::new(tx), None), rx)
}

fn single_text(output: &ToolOutput) -> String {
    output
        .content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

async fn collect(
    mut stream: Pin<Box<dyn Stream<Item = AgentEvent> + Send>>,
) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    events
}

/// Every text a tool result carried, concatenated in transcript order.
///
/// A committed tool result is a `Role::Tool` message whose content is
/// the tool output's parts; this is exactly what the next model request
/// carries.
fn tool_result_text(request: &CompletionRequest) -> String {
    let mut text = String::new();
    for message in request.messages.iter() {
        if !matches!(message.role, Role::Tool) {
            continue;
        }
        for part in message.content.iter() {
            if let ContentPart::Text(value) = part {
                text.push_str(&value.text);
            }
        }
    }
    text
}

fn final_text(events: &[AgentEvent]) -> String {
    events
        .iter()
        .rev()
        .find_map(|event| match event {
            AgentEvent::ModelDone(result) => Some(result.text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

fn end_reasons(
    events: &[AgentEvent],
) -> Vec<elysper_harness::SessionEndReason> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::System(SystemEvent::SessionEnded { reason }) => {
                Some(reason.clone())
            }
            _ => None,
        })
        .collect()
}

async fn run_agent(agent: Arc<dyn Agent>) -> Vec<AgentEvent> {
    let stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    collect(stream).await
}

// --- the service ------------------------------------------------------

#[test]
fn provides_agent_loop_service() {
    let ctx = Context::new();
    let _deps = provide_dependencies(&ctx);
    let factory: Arc<dyn AgentLoopFactory> = Arc::new(StubLoop);
    let plugin = AgentLoopPlugin::new(Arc::clone(&factory));

    assert_eq!(plugin.manifest().id, "agent-loop");
    assert_eq!(plugin.manifest().version, PLUGIN_PROTOCOL_VERSION);
    assert_eq!(plugin.manifest().runtime, RuntimeKind::Native);
    assert_eq!(plugin.manifest().provides, vec![KEY_AGENT_LOOP.to_string()]);
    assert_eq!(
        plugin.manifest().inject,
        vec![
            "llm".to_string(),
            "tools".to_string(),
            "sessions".to_string(),
        ]
    );
    assert!(plugin.manifest().capabilities.is_empty());

    let mounted = mount_all(&ctx, &[Arc::new(plugin) as Arc<dyn KernelPlugin>])
        .expect("mount `agent-loop`");
    assert_eq!(mounted.len(), 1);

    let resolved = ctx
        .require::<Arc<dyn AgentLoopFactory>>(KEY_AGENT_LOOP)
        .expect("`agentLoop` resolves");
    assert!(
        Arc::ptr_eq(resolved.as_ref(), &factory),
        "the service must be the factory the plugin was built with"
    );
}

#[test]
fn unload_cascades_consumer_to_pending() {
    let ctx = Context::new();
    let (llm, _tools, _sessions) = provide_dependencies(&ctx);
    let factory: Arc<dyn AgentLoopFactory> = Arc::new(StubLoop);
    let _mounted = mount_all(&ctx, &[loop_plugin(factory)]).expect("mount");
    assert!(
        ctx.require::<Arc<dyn AgentLoopFactory>>(KEY_AGENT_LOOP)
            .is_ok()
    );

    drop(llm);
    assert!(
        ctx.require::<Arc<dyn AgentLoopFactory>>(KEY_AGENT_LOOP)
            .is_err(),
        "unloading `llm` must cascade `agentLoop` to Pending"
    );

    let _llm = ctx.provide("llm", ()).expect("re-provide `llm`");
    assert!(
        ctx.require::<Arc<dyn AgentLoopFactory>>(KEY_AGENT_LOOP)
            .is_ok(),
        "remounting `llm` must reactivate `agentLoop`"
    );
}

// --- the step seams ---------------------------------------------------

#[tokio::test]
async fn step_pre_short_circuit_skips_provider() {
    let provider = Arc::new(ScriptedProvider::tool_then_text());
    let mut host = Host::new();
    let _gate = host.gate(Arc::new(BlockStep));

    let agent = agent_with_seams(
        provider.clone(),
        Arc::new(ToolRegistry::new()),
        Seams::new(host),
    );
    let events = run_agent(agent).await;

    assert_eq!(
        provider.call_count(),
        0,
        "a step-pre short-circuit must skip the model"
    );
    let reasons = end_reasons(&events);
    assert_eq!(reasons.len(), 1, "exactly one terminal event");
    assert!(
        matches!(
            &reasons[0],
            elysper_harness::SessionEndReason::Error(reason)
                if reason == "blocked at step pre"
        ),
        "the step result is the block reason, got {:?}",
        reasons[0]
    );
}

#[tokio::test]
async fn step_post_observes_result() {
    // `step_post` is the tool-step boundary: it fires after a step that
    // committed tool results and before the next sample, so the script
    // asks for one tool call, then answers.
    let provider = Arc::new(ScriptedProvider::tool_then_text());
    let (registry, echo) = registry_with_echo();
    let observations = Arc::new(AtomicUsize::new(0));
    let mut host = Host::new();
    let _hook = host.hook(Arc::new(AfterStepProbe(Arc::clone(&observations))));

    let agent = agent_with_seams(provider, registry, Seams::new(host));
    let events = run_agent(agent).await;

    assert_eq!(
        observations.load(Ordering::SeqCst),
        1,
        "the step-post hook runs once for a tool step"
    );
    assert_eq!(
        *echo.call_count.lock(),
        1,
        "an observing middleware leaves the step's tool execution alone"
    );
    assert_eq!(
        final_text(&events),
        "done",
        "an observing middleware leaves the run's result unchanged"
    );
}

// --- the tool seams ---------------------------------------------------

#[tokio::test]
async fn tool_pre_denial_writes_tool_result() {
    let provider = Arc::new(ScriptedProvider::tool_then_text());
    let (registry, echo) = registry_with_echo();
    let mut host = Host::new();
    let _gate = host.gate(Arc::new(BlockEcho));

    let agent = agent_with_seams(provider.clone(), registry, Seams::new(host));
    let _events = run_agent(agent).await;

    assert_eq!(
        *echo.call_count.lock(),
        0,
        "a `tool/pre-execute` veto must never reach the tool"
    );
    let requests = provider.requests();
    assert!(requests.len() >= 2, "the loop must sample after the veto");
    let text = tool_result_text(&requests[1]);
    assert!(
        text.contains("blocked by policy"),
        "the model must see a tool_result carrying the veto, got {text:?}"
    );
}

#[tokio::test]
async fn tool_post_rewrite_reaches_model() {
    let provider = Arc::new(ScriptedProvider::tool_then_text());
    let (registry, echo) = registry_with_echo();
    let mut host = Host::new();
    let _rewrite = host.result_transformer(Arc::new(RewriteEcho));

    let agent = agent_with_seams(provider.clone(), registry, Seams::new(host));
    let _events = run_agent(agent).await;

    assert_eq!(*echo.call_count.lock(), 1, "the tool must have executed");
    let requests = provider.requests();
    assert!(requests.len() >= 2);
    let text = tool_result_text(&requests[1]);
    assert_eq!(
        text, "rewritten by post-execute",
        "the model must read the rewritten result"
    );
}

// --- prompt sections and transformers ---------------------------------

#[test]
fn prompt_sections_withdraw_with_their_registrations() {
    assert_eq!(seams::PROMPT_SECTIONS, "prompt/sections");

    let drops = Arc::new(AtomicUsize::new(0));
    let mut host = Host::new();
    let hook = host.hook(Arc::new(DropProbe(Arc::clone(&drops))));
    let section = host.prompt_section(PromptSection {
        title: "Test".to_string(),
        body: "section body".to_string(),
    });
    let seams = Seams::new(host.clone());
    assert_eq!(seams.prompt_sections().len(), 1);
    assert_eq!(
        drops.load(Ordering::SeqCst),
        0,
        "a live registration keeps the section and its hook alive"
    );

    drop(hook);
    drop(section);
    drop(host);
    assert!(
        seams.prompt_sections().is_empty(),
        "dropping the last registration must remove the prompt section \
         from the seam"
    );
    assert_eq!(
        drops.load(Ordering::SeqCst),
        1,
        "dropping the last registration must drop the hook"
    );
}

#[tokio::test]
async fn llm_request_and_result_transformers() {
    let mut host = Host::new();
    let _request = host.request_transformer(Arc::new(SetTemperature));
    let _result = host.result_transformer(Arc::new(RewriteResult));
    let seams = Seams::new(host);

    let state = AgentState::with_window(4_096);
    let (sink, _rx) = probe_sink();
    let cancel: Arc<dyn CancelToken> = Arc::new(CancellationToken::new());
    let ctx = seam_context(&state, &sink, &cancel);

    let mut params = RequestParams::from_request(&CompletionRequest::default());
    seams.llm_request(&ctx, "scripted", &mut params).await;
    assert_eq!(
        params.temperature,
        Some(0.25),
        "the request transformer must rewrite the parameters"
    );
    assert_eq!(params.model, "rewritten-model");

    let mut output = ToolOutput::text("raw");
    let names = seams.tool_result_transformers(&ctx, &mut output).await;
    assert_eq!(
        names,
        vec!["rewrite-result".to_string()],
        "the result transformer must report that it changed the output"
    );
    assert_eq!(single_text(&output), "rewritten by result transformer");
}

// --- scoped lists, unload, ordering -----------------------------------

#[test]
fn tools_and_delegators_are_seam_scoped() {
    let mut host = Host::new();
    let _tool = host.tool(Arc::new(FakeTool::new("seam-tool", "x")));
    let _delegator = host.delegator(Arc::new(NoopDelegator));
    let seams = Seams::new(host);

    assert_eq!(seams.tools().len(), 1, "the tool comes from the seam");
    assert_eq!(
        seams.delegators().len(),
        1,
        "the delegator comes from the seam"
    );
}

#[test]
fn unload_removes_the_agent_loop_service() {
    let ctx = Context::new();
    let _deps = provide_dependencies(&ctx);

    let factory: Arc<dyn AgentLoopFactory> = Arc::new(StubLoop);
    let plugin: Arc<dyn KernelPlugin> = Arc::new(SeamPlugin::new(factory));
    let mounted = mount_all(&ctx, &[plugin]).expect("mount the seam plugin");

    assert!(
        ctx.require::<Arc<dyn AgentLoopFactory>>(KEY_AGENT_LOOP)
            .is_ok()
    );

    drop(mounted);
    assert!(
        ctx.require::<Arc<dyn AgentLoopFactory>>(KEY_AGENT_LOOP)
            .is_err(),
        "unload must remove `agentLoop`"
    );
}

#[test]
fn seams_empty_when_the_last_registration_drops() {
    let mut host = Host::new();
    let section = host.prompt_section(PromptSection {
        title: "Audit".to_string(),
        body: "cite sources".to_string(),
    });
    let tool = host.tool(Arc::new(FakeTool::new("seam-tool", "x")));
    let probe = Seams::new(host.clone());
    assert_eq!(probe.prompt_sections().len(), 1);
    assert_eq!(probe.tools().len(), 1);

    // The host's own clone is still alive until the end: withdrawal
    // comes from the registrations dropping, the same lifetime the
    // kernel's `process-plugins` entry hands its effect.
    drop(section);
    drop(tool);
    drop(host);
    assert!(
        probe.prompt_sections().is_empty(),
        "dropping the registrations must empty the seam's prompt sections"
    );
    assert!(
        probe.tools().is_empty(),
        "dropping the registrations must empty the seam's tool list"
    );
}

#[tokio::test]
async fn registrations_run_in_order_and_short_circuit_stops_later() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let later = Arc::new(AtomicUsize::new(0));
    let mut host = Host::new();
    let _first = host.hook(Arc::new(OrderHook {
        label: "first",
        seen: Arc::clone(&seen),
    }));
    let _second = host.hook(Arc::new(OrderHook {
        label: "second",
        seen: Arc::clone(&seen),
    }));
    let _deny = host.gate(Arc::new(DenyGate));
    let _later = host.gate(Arc::new(LaterGate(Arc::clone(&later))));
    let seams = Seams::new(host);

    let state = AgentState::with_window(4_096);
    let (sink, _rx) = probe_sink();
    let cancel: Arc<dyn CancelToken> = Arc::new(CancellationToken::new());
    let ctx = seam_context(&state, &sink, &cancel);

    let arbitration = seams.tool_pre(&ctx, "echo", "call-1", &json!({})).await;

    assert!(
        matches!(arbitration.decision, HookDecision::Block { .. }),
        "the first gate's block must win the pass"
    );
    assert_eq!(
        *seen.lock().expect("order log"),
        vec!["first", "second"],
        "hooks run in registration order"
    );
    assert_eq!(
        later.load(Ordering::SeqCst),
        0,
        "a short-circuit must stop later registrations"
    );
}

// --- per-run snapshot -------------------------------------------------

#[tokio::test]
async fn each_run_uses_the_provider_captured_by_its_factory_call() {
    let first = Arc::new(ScriptedProvider::text("from-first"));
    let second = Arc::new(ScriptedProvider::text("from-second"));
    let agent_first = ReActAgentFactory.create(&run_config(
        first.clone(),
        Arc::new(ToolRegistry::new()),
        Host::new(),
    ));
    let agent_second = ReActAgentFactory.create(&run_config(
        second.clone(),
        Arc::new(ToolRegistry::new()),
        Host::new(),
    ));

    let events_first = run_agent(agent_first).await;
    let events_second = run_agent(agent_second).await;

    assert_eq!(final_text(&events_first), "from-first");
    assert_eq!(final_text(&events_second), "from-second");
    assert_eq!(first.call_count(), 1);
    assert_eq!(second.call_count(), 1);
}
