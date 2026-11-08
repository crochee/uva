//! The `eval` service as a native kernel plugin.
//!
//! The service is the host-built [`EvalRunner`]. These tests resolve it
//! through the kernel and run a suite through the resolved handle: that
//! run is the only proof the published value is the runner the host
//! configured, not a copy with different metrics or threshold.

use std::sync::Arc;

use async_trait::async_trait;
use elysper_eval::{
    EvalAgent,
    EvalError,
    EvalRunner,
    EvalSuite,
    TestCase,
    plugin::{EvalPlugin, KEY_EVAL},
};
use elysper_kernel::{
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

#[tokio::test]
async fn provides_eval_service_and_unload_removes_it() {
    let ctx = Context::new();
    let runner = Arc::new(EvalRunner::new().threshold(0.5));
    let plugin = EvalPlugin::new(Arc::clone(&runner));

    let mounted = mount_all(&ctx, &[Arc::new(plugin) as Arc<dyn KernelPlugin>])
        .expect("mount `eval`");
    assert_eq!(mounted.len(), 1);

    let service = ctx
        .require::<EvalRunner>(KEY_EVAL)
        .expect("`eval` resolves");
    assert!(
        Arc::ptr_eq(&service, &runner),
        "the service must be the runner the plugin was built with"
    );

    let suite = EvalSuite::new("smoke").add_case(
        TestCase::new("greeting", "Say hello").expect_contains("echo"),
    );
    let report = service
        .run(&EchoAgent, &suite)
        .await
        .expect("the resolved runner must score the suite");
    assert_eq!(report.suite_name, "smoke");
    assert_eq!(report.total, 1);
    assert_eq!(report.passed, 1);
    assert_eq!(report.results[0].actual_output, "echo: Say hello");

    drop(mounted);
    assert!(
        ctx.require::<EvalRunner>(KEY_EVAL).is_err(),
        "unload must withdraw `eval`"
    );
}

#[test]
fn manifest_matches_provided_key() {
    let plugin = EvalPlugin::new(Arc::new(EvalRunner::new()));
    let manifest = plugin.manifest();

    assert_eq!(manifest.id, "eval");
    assert_eq!(manifest.version, PLUGIN_PROTOCOL_VERSION);
    assert_eq!(manifest.runtime, RuntimeKind::Native);
    assert_eq!(manifest.provides, vec![KEY_EVAL.to_string()]);
    assert_eq!(KEY_EVAL, "eval");
    assert!(
        manifest.inject.is_empty(),
        "the runner arrives with the plugin value; it injects nothing"
    );
    assert!(manifest.capabilities.is_empty());
    assert!(manifest.abi.is_none());
    assert!(manifest.config_schema.is_none());
}
