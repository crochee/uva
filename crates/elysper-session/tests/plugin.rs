//! The crate's two services as native kernel plugins: `sessions` (the
//! registry) and `compaction` (the compactor).
//!
//! The `sessions` plugin builds its registry from a
//! [`SessionStoreConfig`]; callers take a per-run `SessionSink` /
//! writer from the resolved service. The `compaction` plugin builds a
//! [`SummaryCompactor`](elysper_session::SummaryCompactor) over the
//! `llm.default` provider it injects and the model and policy its
//! [`CompactionPluginConfig`] carries; it adds no selection logic of
//! its own, so the tool-call / result pairing rule stays where it is
//! enforced (`balanced_cut_at_or_before`).

use std::sync::Arc;

use async_trait::async_trait;
use elysper_core::Error;
use elysper_kernel::{
    Context,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    RuntimeKind,
    mount_all,
};
use elysper_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    ContentPart,
    Message,
    ModelConfig,
    ModelProvider,
    ProviderConfig,
    Role,
    ToolUse,
    plugin::KEY_LLM_DEFAULT,
};
use elysper_session::{
    CompactionSettings,
    Compactor,
    SessionSink,
    TokenBudget,
    balanced_message_cut,
    compaction_plugin::{
        CompactionPlugin,
        CompactionPluginConfig,
        KEY_COMPACTION,
    },
    manager::SessionRegistry,
    plugin::{KEY_SESSIONS, SessionStoreConfig, SessionStorePlugin},
};

/// A provider that answers every completion with one line of text.
///
/// The plugin only needs the handle to build its compactor; the
/// assertions drive `plan`, which never calls the provider.
struct StubProvider;

#[async_trait]
impl ModelProvider for StubProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "stub"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "stub-model".to_string(),
            provider: "stub".to_string(),
            context_window: 200_000,
            max_output_tokens: 4_096,
            supports_tools: true,
            supports_streaming: false,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, Error> {
        Ok(CompletionResponse {
            content: Content::text("summary"),
            ..CompletionResponse::default()
        })
    }
}

fn session_store_config(dir: &std::path::Path) -> SessionStoreConfig {
    SessionStoreConfig {
        sessions_dir: dir.to_path_buf(),
        blocking: None,
    }
}

#[test]
fn provides_service_and_unload_removes_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = Context::new();
    let plugin = SessionStorePlugin::new(session_store_config(dir.path()));

    let mounted = mount_all(&ctx, &[Arc::new(plugin) as Arc<dyn KernelPlugin>])
        .expect("mount `session-store`");
    assert_eq!(mounted.len(), 1);

    let service = ctx
        .require::<SessionRegistry>(KEY_SESSIONS)
        .expect("`sessions` resolves");
    assert_eq!(
        service.sessions_root(),
        dir.path(),
        "the plugin builds its registry over the configured root"
    );

    // The registry half: a run takes its sink from the service, and
    // the sink the built registry hands out actually writes.
    let sink: Arc<dyn SessionSink> = service.sink("user", "session");
    assert_eq!(sink.id(), "session");
    assert!(
        Arc::ptr_eq(&service.sink("user", "session"), &sink),
        "one session resolves to one sink"
    );
    futures::executor::block_on(async {
        sink.append(&serde_json::json!({"type": "Test", "data": {"n": 1}}))
            .await
            .expect("the sink the plugin built appends");
        let rows = sink.read().await.expect("the sink reads back");
        assert_eq!(rows.len(), 1, "one appended row round-trips");
        assert_eq!(rows[0]["data"]["n"], 1);
    });

    drop(mounted);
    assert!(
        ctx.require::<SessionRegistry>(KEY_SESSIONS).is_err(),
        "unload must withdraw `sessions`"
    );
}

#[test]
fn manifest_matches_provided_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let plugin = SessionStorePlugin::new(session_store_config(dir.path()));
    let manifest = plugin.manifest();

    assert_eq!(manifest.id, "session-store");
    assert_eq!(manifest.version, PLUGIN_PROTOCOL_VERSION);
    assert_eq!(manifest.runtime, RuntimeKind::Native);
    assert_eq!(manifest.provides, vec![KEY_SESSIONS.to_string()]);
    assert_eq!(KEY_SESSIONS, "sessions");
    assert!(
        manifest.inject.is_empty(),
        "the plugin builds the registry from its config; it injects nothing"
    );
    assert!(manifest.capabilities.is_empty());
    assert!(manifest.abi.is_none());
    assert!(manifest.config_schema.is_none());
}

// --- the `compaction` service ----------------------------------------

/// A history whose first message is large enough to cross a small
/// trigger, with one tool call and its result at the tail so a plan
/// can be checked for pair preservation.
fn tool_call_history() -> Vec<Message> {
    vec![
        Message::user("please read the file ".repeat(200)),
        Message::new(
            Role::Assistant,
            Content::Single(ContentPart::ToolUse(ToolUse {
                id: "c1".to_string(),
                name: "read".to_string(),
                input: serde_json::json!({"path": "a.txt"}),
            })),
        ),
        Message::tool(Content::text("file body ".repeat(100)), "c1"),
        Message::assistant("done"),
    ]
}

fn compactor_config(settings: CompactionSettings) -> CompactionPluginConfig {
    CompactionPluginConfig {
        model: "stub-model".to_string(),
        settings,
    }
}

#[test]
fn compaction_keeps_tool_pairs() {
    let ctx = Context::new();
    let _llm = ctx
        .provide(
            KEY_LLM_DEFAULT,
            Arc::new(StubProvider) as Arc<dyn ModelProvider>,
        )
        .expect("provide `llm.default`");
    let plugin = CompactionPlugin::new(compactor_config(CompactionSettings {
        enabled: true,
        reserve_tokens: 1,
        keep_recent_tokens: 1,
        min_messages_between_compaction: 4,
    }));

    let mounted = mount_all(&ctx, &[Arc::new(plugin) as Arc<dyn KernelPlugin>])
        .expect("mount `context-compactor`");

    let service = ctx
        .require::<Arc<dyn Compactor>>(KEY_COMPACTION)
        .expect("`compaction` resolves");
    let history = tool_call_history();
    let plan = service
        .plan(
            &history,
            TokenBudget {
                window: 8,
                reserve: 0,
            },
        )
        .expect("the published compactor plans a compaction");

    assert_eq!(plan.range, 0..3);
    assert_eq!(plan.keep_recent, 1);
    assert_eq!(plan.shadowed.len(), plan.range.len());
    assert!(
        balanced_message_cut(&history, plan.range.end),
        "the cut must leave every pair whole"
    );
    assert!(
        plan.shadowed
            .iter()
            .any(|message| message.content.has_tool_use()),
        "the tool call is inside the shadowed span"
    );
    assert!(
        plan.shadowed
            .iter()
            .any(|message| message.tool_call_id.as_deref() == Some("c1")),
        "the call's result travels with it"
    );

    drop(mounted);
    assert!(
        ctx.require::<Arc<dyn Compactor>>(KEY_COMPACTION).is_err(),
        "unload must withdraw `compaction`"
    );
}

#[test]
fn compaction_config_settings_gate_the_published_compactor() {
    let ctx = Context::new();
    let _llm = ctx
        .provide(
            KEY_LLM_DEFAULT,
            Arc::new(StubProvider) as Arc<dyn ModelProvider>,
        )
        .expect("provide `llm.default`");
    let plugin = CompactionPlugin::new(compactor_config(CompactionSettings {
        enabled: false,
        ..CompactionSettings::default()
    }));

    let mounted = mount_all(&ctx, &[Arc::new(plugin) as Arc<dyn KernelPlugin>])
        .expect("mount `context-compactor`");

    let service = ctx
        .require::<Arc<dyn Compactor>>(KEY_COMPACTION)
        .expect("`compaction` resolves");
    assert!(
        service
            .plan(
                &tool_call_history(),
                TokenBudget {
                    window: 8,
                    reserve: 0,
                },
            )
            .is_none(),
        "the settings the config carries govern the built compactor"
    );

    drop(mounted);
}

#[test]
fn compaction_manifest_matches_provided_key() {
    let plugin =
        CompactionPlugin::new(compactor_config(CompactionSettings::default()));
    let manifest = plugin.manifest();

    assert_eq!(manifest.id, "context-compactor");
    assert_eq!(manifest.version, PLUGIN_PROTOCOL_VERSION);
    assert_eq!(manifest.runtime, RuntimeKind::Native);
    assert_eq!(manifest.provides, vec![KEY_COMPACTION.to_string()]);
    assert_eq!(KEY_COMPACTION, "compaction");
    assert_eq!(
        manifest.inject,
        vec![KEY_LLM_DEFAULT.to_string()],
        "the compactor is built over the injected default provider"
    );
    assert!(manifest.capabilities.is_empty());
    assert!(manifest.abi.is_none());
    assert!(manifest.config_schema.is_none());
}
