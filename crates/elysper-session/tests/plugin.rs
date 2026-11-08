//! The crate's two services as native kernel plugins: `sessions` (the
//! registry) and `compaction` (the compactor).
//!
//! The `sessions` plugin holds the registry and nothing else: callers
//! take a per-run `SessionSink` / writer from it, and the service is
//! the same registry the host built. The `compaction` plugin publishes
//! a host-built [`Compactor`]; it adds no selection logic of its own,
//! so the tool-call / result pairing rule stays where it is enforced
//! (`balanced_cut_at_or_before`).

use std::sync::Arc;

use elysper_core::CancelToken;
use elysper_kernel::{
    Context,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    RuntimeKind,
    mount_all,
};
use elysper_provider::{
    Content,
    ContentPart,
    Message,
    Role,
    ToolUse,
    estimate_messages_token_count,
};
use elysper_session::{
    CompactionError,
    CompactionOutcome,
    CompactionPlan,
    Compactor,
    SessionSink,
    TokenBudget,
    balanced_cut_at_or_before,
    balanced_message_cut,
    compaction_plugin::{CompactionPlugin, KEY_COMPACTION},
    manager::SessionRegistry,
    plugin::{KEY_SESSIONS, SessionStorePlugin},
};
use futures::future::BoxFuture;

#[test]
fn provides_service_and_unload_removes_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = Context::new();
    let registry = Arc::new(SessionRegistry::new(dir.path().to_path_buf()));
    let plugin = SessionStorePlugin::new(Arc::clone(&registry));

    let mounted = mount_all(&ctx, &[Arc::new(plugin) as Arc<dyn KernelPlugin>])
        .expect("mount `session-store`");
    assert_eq!(mounted.len(), 1);

    let service = ctx
        .require::<SessionRegistry>(KEY_SESSIONS)
        .expect("`sessions` resolves");
    assert!(
        Arc::ptr_eq(&service, &registry),
        "the service must be the registry the plugin was built with"
    );

    // The registry half: a run takes its sink from the service.
    let sink: Arc<dyn SessionSink> = service.sink("user", "session");
    assert_eq!(sink.id(), "session");
    assert!(
        Arc::ptr_eq(&service.sink("user", "session"), &sink),
        "one session resolves to one sink"
    );

    drop(mounted);
    assert!(
        ctx.require::<SessionRegistry>(KEY_SESSIONS).is_err(),
        "unload must withdraw `sessions`"
    );
}

#[test]
fn manifest_matches_provided_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = Arc::new(SessionRegistry::new(dir.path().to_path_buf()));
    let plugin = SessionStorePlugin::new(registry);
    let manifest = plugin.manifest();

    assert_eq!(manifest.id, "session-store");
    assert_eq!(manifest.version, PLUGIN_PROTOCOL_VERSION);
    assert_eq!(manifest.runtime, RuntimeKind::Native);
    assert_eq!(manifest.provides, vec![KEY_SESSIONS.to_string()]);
    assert_eq!(KEY_SESSIONS, "sessions");
    assert!(
        manifest.inject.is_empty(),
        "the plugin owns the registry; it injects nothing"
    );
    assert!(manifest.capabilities.is_empty());
    assert!(manifest.abi.is_none());
    assert!(manifest.config_schema.is_none());
}

// --- the `compaction` service ----------------------------------------

/// A compactor that plans the span up to the first balanced cut at or
/// before the third message. The span therefore includes one tool call
/// *and* its result, and the cut is never allowed to orphan either
/// half.
struct PairPreservingCompactor;

impl Compactor for PairPreservingCompactor {
    fn plan(
        &self,
        history: &[Message],
        _budget: TokenBudget,
    ) -> Option<CompactionPlan> {
        let cut = balanced_cut_at_or_before(history, 3.min(history.len()));
        if cut == 0 {
            return None;
        }
        let shadowed = history[..cut].to_vec();
        Some(CompactionPlan {
            range: 0..cut,
            shadowed_seqs: Vec::new(),
            keep_recent: history.len() - cut,
            shadowed_tokens: estimate_messages_token_count(&shadowed),
            shadowed,
        })
    }

    fn summarize(
        &self,
        _plan: &CompactionPlan,
        _cancel: Arc<dyn CancelToken>,
    ) -> BoxFuture<'static, Result<CompactionOutcome, CompactionError>> {
        Box::pin(async { Err(CompactionError::EmptySummary) })
    }
}

fn tool_call_history() -> Vec<Message> {
    vec![
        Message::user("please read the file"),
        Message::new(
            Role::Assistant,
            Content::Single(ContentPart::ToolUse(ToolUse {
                id: "c1".to_string(),
                name: "read".to_string(),
                input: serde_json::json!({"path": "a.txt"}),
            })),
        ),
        Message::tool(Content::text("file body"), "c1"),
        Message::assistant("done"),
    ]
}

#[test]
fn compaction_keeps_tool_pairs() {
    let ctx = Context::new();
    let plugin = CompactionPlugin::new(Arc::new(PairPreservingCompactor));

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
        .expect("the fake compactor plans a compaction");

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
fn compaction_manifest_matches_provided_key() {
    let plugin = CompactionPlugin::new(Arc::new(PairPreservingCompactor));
    let manifest = plugin.manifest();

    assert_eq!(manifest.id, "context-compactor");
    assert_eq!(manifest.version, PLUGIN_PROTOCOL_VERSION);
    assert_eq!(manifest.runtime, RuntimeKind::Native);
    assert_eq!(manifest.provides, vec![KEY_COMPACTION.to_string()]);
    assert_eq!(KEY_COMPACTION, "compaction");
    assert!(
        manifest.inject.is_empty(),
        "the compactor arrives with the plugin value; it injects nothing"
    );
    assert!(manifest.capabilities.is_empty());
    assert!(manifest.abi.is_none());
    assert!(manifest.config_schema.is_none());
}
