//! The `eval` service as a native kernel plugin.
//!
//! `EvalPlugin::apply` constructs the runner from the plugin's own
//! configuration. These tests mount it over a fresh kernel context,
//! resolve the service, and run a suite through the resolved handle:
//! that run (plus the pinned `generated_at`) is the proof the published
//! value is a working runner built from the configuration, not a copy.

use async_trait::async_trait;
use uva_core::SharedClock;
use uva_eval::{
    EvalAgent,
    EvalError,
    EvalRunner,
    EvalSuite,
    TestCase,
    plugin::{EvalPlugin, EvalPluginConfig, KEY_EVAL},
};
use uva_kernel::{
    Context,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    RuntimeKind,
    mount_all,
};

/// The smallest agent under test: it echoes its input, so a case that
/// expects the word "echo" passes.
struct EchoAgent;

#[async_trait]
impl EvalAgent for EchoAgent {
    async fn respond(&self, input: &str) -> Result<String, EvalError> {
        Ok(format!("echo: {input}"))
    }
}

/// The suite the service must be able to score.
fn smoke_suite() -> EvalSuite {
    EvalSuite::new("smoke").add_case(
        TestCase::new("greeting", "Say hello").expect_contains("echo"),
    )
}

#[tokio::test]
async fn applies_configured_runner_and_unload_removes_it() {
    let ctx = Context::new();
    let plugin = EvalPlugin::new(EvalPluginConfig {
        keyword_metric: true,
        clock: Some(SharedClock::fixed_from_rfc3339("2026-01-02T03:04:05Z")),
    });

    let mounted = mount_all(
        &ctx,
        &[std::sync::Arc::new(plugin) as std::sync::Arc<dyn KernelPlugin>],
    )
    .expect("mount `eval`");
    assert_eq!(mounted.len(), 1);

    let service = ctx
        .require::<EvalRunner>(KEY_EVAL)
        .expect("`eval` resolves");

    let report = service
        .run(&EchoAgent, &smoke_suite())
        .await
        .expect("the applied runner must score the suite");
    assert_eq!(report.suite_name, "smoke");
    assert_eq!(report.total, 1);
    assert_eq!(report.passed, 1);
    assert_eq!(report.results[0].actual_output, "echo: Say hello");
    assert_eq!(
        report.generated_at.to_rfc3339(),
        "2026-01-02T03:04:05+00:00",
        "the applied runner must read `generated_at` from the configured clock"
    );

    drop(mounted);
    assert!(
        ctx.require::<EvalRunner>(KEY_EVAL).is_err(),
        "unload must withdraw `eval`"
    );
}

#[tokio::test]
async fn config_without_keyword_metric_still_provides_a_runner() {
    let ctx = Context::new();
    let plugin = EvalPlugin::new(EvalPluginConfig {
        keyword_metric: false,
        clock: None,
    });

    let mounted = mount_all(
        &ctx,
        &[std::sync::Arc::new(plugin) as std::sync::Arc<dyn KernelPlugin>],
    )
    .expect("mount `eval`");

    let service = ctx
        .require::<EvalRunner>(KEY_EVAL)
        .expect("`eval` resolves");
    let report = service
        .run(&EchoAgent, &smoke_suite())
        .await
        .expect("the runner without metrics falls back to keyword matching");
    assert_eq!(report.passed, 1);

    drop(mounted);
    assert!(ctx.require::<EvalRunner>(KEY_EVAL).is_err());
}

#[test]
fn default_config_enables_the_keyword_metric() {
    let config = EvalPluginConfig::default();
    assert!(
        config.keyword_metric,
        "the default must mirror the boot's `KeywordMetric` runner"
    );
    assert!(
        config.clock.is_none(),
        "the default clock is the runner's system clock"
    );
}

#[test]
fn manifest_matches_provided_key() {
    let plugin = EvalPlugin::new(EvalPluginConfig::default());
    let manifest = plugin.manifest();

    assert_eq!(manifest.id, "eval");
    assert_eq!(manifest.version, PLUGIN_PROTOCOL_VERSION);
    assert_eq!(manifest.runtime, RuntimeKind::Native);
    assert_eq!(manifest.provides, vec![KEY_EVAL.to_string()]);
    assert_eq!(KEY_EVAL, "eval");
    assert!(
        manifest.inject.is_empty(),
        "`apply` builds the runner from config; it injects nothing"
    );
    assert!(manifest.capabilities.is_empty());
    assert!(manifest.config_schema.is_none());
}

#[test]
fn service_is_gone_when_its_effect_handle_drops() {
    let ctx = Context::new();

    let before = ctx.require::<EvalRunner>(KEY_EVAL);
    assert!(before.is_err(), "an empty context has no `eval` service");

    let plugin = EvalPlugin::new(EvalPluginConfig::default());
    let handles = plugin.apply(&ctx).expect("apply `eval`");
    assert!(
        ctx.require::<EvalRunner>(KEY_EVAL).is_ok(),
        "apply must publish `eval`"
    );

    drop(handles);
    assert!(
        ctx.require::<EvalRunner>(KEY_EVAL).is_err(),
        "dropping the provide effect must withdraw `eval`"
    );
}
