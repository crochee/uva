//! Tests for the strategy vocabulary: the alias the agent table
//! carries, and the registry the alias resolves through.
//!
//! The alias reader is what the `agent-loop` row's config carries;
//! the row resolves the alias itself when it builds the loop.

use crate::state::{
    load_default_strategy_alias,
    tests::resolve_server::EnvContentGuard,
};

#[tokio::test]
async fn load_default_strategy_alias_reads_the_agent_table() {
    let _guard = EnvContentGuard::cleared();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        r#"{
            "version": "1.0",
            "agents": {
                "reviewer": { "max_steps": 10, "strategy": "react" }
            }
        }"#,
    )
    .unwrap();

    let cfg = crate::config::resolve_server_config(dir.path(), None)
        .expect("the workspace config must parse");
    let alias = load_default_strategy_alias(Some(&cfg), Some("reviewer"))
        .expect("a configured strategy must carry its alias");
    assert_eq!(alias, "react");

    std::fs::write(
        dir.path().join("config.toml"),
        r#"{"version": "1.0", "agents": {"reviewer": {"max_steps": 3}}}"#,
    )
    .unwrap();
    let cfg = crate::config::resolve_server_config(dir.path(), None).unwrap();
    assert!(
        load_default_strategy_alias(Some(&cfg), Some("reviewer")).is_none()
    );
    assert!(load_default_strategy_alias(Some(&cfg), Some("nope")).is_none());
    assert!(load_default_strategy_alias(Some(&cfg), None).is_none());
    assert!(load_default_strategy_alias(None, Some("reviewer")).is_none());
}

/// The harness's built-in `react` family resolves through the same
/// registry call — one resolution path, no `.unwrap()` panic and no
/// silent `warn`-and-fallback. It is the only strategy this server
/// knows.
#[tokio::test]
async fn resolve_strategy_resolves_the_built_in_react_family() {
    for alias in ["react", "re-act", "re_act", "ReAct"] {
        let strategy = crate::state::boot::resolve_strategy(alias)
            .unwrap_or_else(|e| panic!("{alias:?} must resolve: {e}"));
        assert_eq!(strategy.name(), "react", "for {alias:?}");
    }
    let err = crate::state::boot::resolve_strategy("mcts")
        .err()
        .expect("mcts must not resolve");
    assert!(
        err.to_string().contains("mcts"),
        "the error must name the unknown strategy: {err}"
    );
}
