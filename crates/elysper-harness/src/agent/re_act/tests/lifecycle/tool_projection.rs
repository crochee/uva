use std::sync::Arc;

use async_trait::async_trait;
use elysper_provider::{ContentPart, ToolUse};
use elysper_tool::{
    Context,
    StreamOutput,
    Tool,
    ToolExposure,
    ToolOutput,
    ToolRegistry,
    ToolSurfacePolicy,
};
use futures::StreamExt;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::super::{support::*, *};
use crate::agent::re_act::projection::{
    ToolProjectionCache,
    compose_tool_definitions,
};

pub(super) async fn tool_definitions_project_exposure_and_promote_after_a_call()
{
    let pending = ToolUse {
        id: "call_deep".to_string(),
        name: "deep".to_string(),
        input: json!({"q": "hello"}),
    };
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![pending]),
        empty_response(),
    ]));
    let real_deep = json!({
        "type": "object",
        "properties": {"q": {"type": "string"}},
        "required": ["q"],
    });
    let real_direct = json!({
        "type": "object",
        "properties": {"key": {"type": "string"}},
        "required": ["key"],
    });
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(elysper_tool::ToolEntry::dynamic(
        "direct_lookup".to_string(),
        "always advertised in full".to_string(),
        real_direct.clone(),
    ));
    registry.register_entry(
        elysper_tool::ToolEntry::dynamic(
            "deep".to_string(),
            "advertised by name until first call".to_string(),
            real_deep.clone(),
        )
        .with_exposure(elysper_tool::ToolExposure::Deferred),
    );
    registry.register_entry(
        elysper_tool::ToolEntry::dynamic(
            "secret".to_string(),
            "never advertised".to_string(),
            json!({"type": "object"}),
        )
        .with_is_hidden(true),
    );

    let _events = run_and_collect(
        provider.clone(),
        Arc::clone(&registry),
        CancellationToken::new(),
        AgentInput::text("go"),
    )
    .await;

    let captured = provider.captured_tools.lock().await;
    assert_eq!(
        captured.len(),
        2,
        "one request for the tool-call iteration and one for the answer"
    );

    // Request 1—nothing called yet: Direct in full, Deferred by
    // name with the placeholder, hidden absent.
    let first = &captured[0];
    let first_names: Vec<&str> =
        first.iter().map(|d| d.name.as_str()).collect();
    assert!(first_names.contains(&"direct_lookup"), "{first_names:?}");
    assert!(first_names.contains(&"deep"), "{first_names:?}");
    assert!(
        !first_names.contains(&"secret"),
        "an is_hidden tool must not reach the model; {first_names:?}"
    );
    let direct = first
        .iter()
        .find(|d| d.name == "direct_lookup")
        .expect("direct_lookup is advertised");
    assert_eq!(direct.input_schema, real_direct);
    let deep = first
        .iter()
        .find(|d| d.name == "deep")
        .expect("deep is advertised");
    assert_eq!(
        deep.input_schema,
        json!({"type": "object", "additionalProperties": true}),
        "an uncalled Deferred tool must not leak its real schema"
    );

    // Request 2—the assistant's `tool_use` for `deep` is now part of
    // the transcript, so the projection promotes it.
    let second = &captured[1];
    assert!(
        !second.iter().any(|d| d.name == "secret"),
        "promotion must not resurrect a hidden tool"
    );
    let promoted = second
        .iter()
        .find(|d| d.name == "deep")
        .expect("deep stays advertised");
    assert_eq!(
        promoted.input_schema, real_deep,
        "the transcript must promote the Deferred tool"
    );

    // A `ToolSurfacePolicy` that declares nothing is the documented
    // no-op, so it must not perturb the unfiltered projection —
    // byte-identical to the projection composed without one and
    // to the first request the run actually sent.
    let mut cache = ToolProjectionCache::new();
    cache.seed(&[]);
    let descriptors = registry.descriptors_cached();
    let baseline =
        compose_tool_definitions(&descriptors, None, None, &[], &cache.called);
    let noop_policy = compose_tool_definitions(
        &descriptors,
        Some(&ToolSurfacePolicy::default()),
        None,
        &[],
        &cache.called,
    );
    assert_eq!(
        serde_json::to_vec(&noop_policy).unwrap(),
        serde_json::to_vec(&baseline).unwrap(),
        "an empty surface policy must not perturb the projection"
    );
    assert_eq!(
        serde_json::to_vec(&baseline).unwrap(),
        serde_json::to_vec(&captured[0]).unwrap(),
        "the policy-less projection must stay byte-identical to the \
         first request"
    );
}

/// The surface policy narrows what the model is told about—inactive
/// groups are withheld, `max_visible` caps the rest in registry order
///—while every registered tool stays dispatcheable: the loop still
/// runs a call to a tool the model was never shown.
pub(super) async fn tool_surface_policy_narrows_advertisement_but_not_dispatch()
{
    let pending = ToolUse {
        id: "call_gamma".to_string(),
        name: "gamma".to_string(),
        input: json!({}),
    };
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![pending.clone()]),
        empty_response(),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    // Registered out of alphabetical order so the cap's registry
    // (name-sorted) order is observable.
    for name in ["gamma", "omega", "beta", "alpha"] {
        registry.register_entry(elysper_tool::ToolEntry::dynamic(
            name.to_string(),
            format!("the {name} tool"),
            json!({"type": "object", "properties": {}}),
        ));
    }
    let policy = ToolSurfacePolicy {
        max_visible: Some(2),
        groups: [
            (
                "core".to_string(),
                vec!["alpha".to_string(), "beta".to_string()],
            ),
            ("extra".to_string(), vec!["gamma".to_string()]),
        ]
        .into_iter()
        .collect(),
        active_groups: vec!["core".to_string()],
    };
    policy
        .write_to_registry(&registry)
        .expect("the policy names registered tools");

    let agent = ReActAgent::new(provider.clone(), Arc::clone(&registry))
        .with_tool_surface(policy);
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    let captured = provider.captured_tools.lock().await;
    let advertised: Vec<&str> =
        captured[0].iter().map(|d| d.name.as_str()).collect();
    assert_eq!(
        advertised,
        vec!["alpha", "beta"],
        "active group first, then the cap; gamma's group is inactive \
         and omega is capped out"
    );
    assert_eq!(
        registry.exposure("gamma"),
        Some(ToolExposure::Hidden),
        "the inactive group's member is Hidden exposure, not hidden"
    );
    assert!(
        registry.contains("gamma"),
        "the policy must not unregister a withheld tool"
    );

    let results: Vec<&elysper_provider::ToolResult> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Model(ContentPart::ToolResult(result)) => Some(result),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].tool_use_id, "call_gamma");
    assert!(
        !results[0].is_error.unwrap_or(true),
        "a withheld tool must still execute: {results:?}"
    );
}

pub(super) async fn run_emits_tool_progress_for_streaming_tool() {
    struct ProgressTool;

    #[async_trait]
    impl Tool for ProgressTool {
        fn name(&self) -> &str {
            "streamer"
        }

        fn description(&self) -> &str {
            "yields progress"
        }

        fn parameters(&self) -> serde_json::Value {
            json!({"type": "object"})
        }

        async fn call(
            &self,
            _input: serde_json::Value,
            _ctx: &Context,
        ) -> ToolOutput {
            ToolOutput::text("done")
        }

        fn stream<'a>(
            &'a self,
            _input: serde_json::Value,
            _ctx: &'a Context,
        ) -> std::pin::Pin<
            Box<dyn futures::Stream<Item = StreamOutput> + Send + 'a>,
        > {
            use futures::stream;
            let s = stream::iter(vec![
                StreamOutput::Progress(ToolOutput::text("halfway")),
                StreamOutput::Result(ToolOutput::text("done")),
            ]);
            Box::pin(s)
        }
    }

    let tool_use = ToolUse {
        id: "call_1".to_string(),
        name: "streamer".to_string(),
        input: json!({}),
    };
    let provider = Arc::new(ScriptedStreamProvider::new(vec![
        tool_call_response(vec![tool_use]),
        empty_response(),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    registry
        .register_entry(elysper_tool::ToolEntry::new(Arc::new(ProgressTool)));

    let events = run_and_collect(
        provider,
        registry,
        CancellationToken::new(),
        AgentInput::text("stream"),
    )
    .await;

    let progress_events: Vec<_> = events
        .iter()
        .filter(|e| {
            matches!(e, AgentEvent::System(SystemEvent::ToolProgress { .. }))
        })
        .collect();
    assert_eq!(progress_events.len(), 1);
    if let AgentEvent::System(SystemEvent::ToolProgress {
        tool_name,
        call_id,
        ..
    }) = progress_events[0]
    {
        assert_eq!(tool_name, "streamer");
        assert_eq!(call_id, "call_1");
    }
}
