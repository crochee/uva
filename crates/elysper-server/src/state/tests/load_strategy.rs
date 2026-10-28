//! Tests for `load_default_strategy` (R50) — the
//! `agents.<name>.strategy` reader that resolves the
//! configured reasoning loop.

use crate::state::{
    load_default_strategy,
    tests::resolve_server::EnvContentGuard,
};

#[tokio::test]
async fn load_default_strategy_reads_agent_table() {
    let _guard = EnvContentGuard::cleared();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        r#"{
            "version": "1.0",
            "agents": {
                "reviewer": { "max_steps": 10, "strategy": "chain-of-thought" }
            }
        }"#,
    )
    .unwrap();

    let cfg = crate::config::resolve_server_config(dir.path(), None)
        .expect("the workspace config must parse");
    let strategy = load_default_strategy(Some(&cfg), Some("reviewer"))
        .expect("a named strategy must resolve");
    assert_eq!(strategy.name(), "chain-of-thought");

    std::fs::write(
        dir.path().join("config.toml"),
        r#"{"version": "1.0", "agents": {"reviewer": {"max_steps": 3}}}"#,
    )
    .unwrap();
    let cfg = crate::config::resolve_server_config(dir.path(), None).unwrap();
    assert!(load_default_strategy(Some(&cfg), Some("reviewer")).is_none());
    assert!(load_default_strategy(Some(&cfg), Some("nope")).is_none());
    assert!(load_default_strategy(Some(&cfg), None).is_none());
    assert!(load_default_strategy(None, Some("reviewer")).is_none());
}

/// R50: a strategy name this build does not ship is logged
/// and treated as unset.
#[tokio::test]
async fn load_default_strategy_falls_back_on_an_unknown_name() {
    let _guard = EnvContentGuard::cleared();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        r#"{"version": "1.0", "agents": {"reviewer": {"strategy": "mcts"}}}"#,
    )
    .unwrap();
    let cfg = crate::config::resolve_server_config(dir.path(), None).unwrap();
    assert!(load_default_strategy(Some(&cfg), Some("reviewer")).is_none());
}

/// The plugin strategies register under their config aliases and resolve
/// through the same registry call as the built-in `react` — one resolution
/// path, no `.unwrap()` panic and no silent `warn`-and-fallback.
#[tokio::test]
async fn resolve_strategy_resolves_every_config_alias() {
    for (alias, expected) in [
        ("chain-of-thought", "chain-of-thought"),
        ("chain_of_thought", "chain-of-thought"),
        ("cot", "chain-of-thought"),
        ("best-of-n", "best-of-n"),
        ("best_of_n", "best-of-n"),
        ("bon", "best-of-n"),
        ("react", "react"),
        ("ReAct", "react"),
    ] {
        let strategy = crate::state::boot::resolve_strategy(alias)
            .unwrap_or_else(|e| panic!("{alias:?} must resolve: {e}"));
        assert_eq!(strategy.name(), expected, "for {alias:?}");
    }
    let err = crate::state::boot::resolve_strategy("mcts")
        .err()
        .expect("mcts must not resolve");
    assert!(
        err.to_string().contains("mcts"),
        "the error must name the unknown strategy: {err}"
    );
}
