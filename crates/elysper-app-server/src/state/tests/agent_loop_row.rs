//! The `agent-loop` row's config: what the boot materializes into it
//! and who wins when the profile already carries one.
//!
//! The row owns its loop, so the boot hands the row the deployment's
//! agent table — the strategy **alias**, the allow/deny tool lists,
//! and the compaction policy — instead of pre-built objects. A profile
//! that already carries a row config keeps it: the deployment's
//! profile is the authority.

use serde_json::json;

use crate::state::{agent_loop_row_config, materialize_row_config};

/// The deployment's agent table: one default agent with a strategy,
/// a tool restriction, and a compaction policy.
fn server_config() -> crate::config::ServerConfig {
    serde_json::from_value(json!({
        "version": "1.0",
        "default_agent": "reviewer",
        "agents": {
            "reviewer": {
                "strategy": "chain-of-thought",
                "allowed_tools": ["read"],
                "denied_tools": ["write"],
                "compaction": {
                    "enabled": true,
                    "reserve_tokens": 4096,
                    "keep_recent_tokens": 512,
                    "min_messages_between_compaction": 4
                }
            }
        }
    }))
    .expect("the agent table parses")
}

/// The materialized row config carries the agent table's three values.
#[test]
fn the_row_config_carries_the_agent_table() {
    let config = server_config();
    let restriction = crate::state::load_default_tool_restriction(
        Some(&config),
        Some("reviewer"),
    );
    let compaction =
        crate::state::load_default_compaction(Some(&config), Some("reviewer"));
    let row = agent_loop_row_config(
        Some(&config),
        Some("reviewer"),
        restriction.as_deref(),
        compaction,
    );

    assert_eq!(
        row["strategy"],
        json!("chain-of-thought"),
        "the row carries the strategy alias, not a strategy object"
    );
    assert_eq!(
        row["tool_restriction"],
        json!({ "allow": ["read"], "deny": ["write"] }),
        "the row carries the allow/deny lists"
    );
    assert_eq!(
        row["compaction"]["keep_recent_tokens"],
        json!(512),
        "the row carries the compaction policy"
    );
}

/// A deployment that configured nothing leaves every value unset —
/// "nothing configured" is not "an empty value".
#[test]
fn an_unconfigured_agent_table_leaves_the_row_unset() {
    let row = agent_loop_row_config(None, None, None, None);
    assert_eq!(row["strategy"], json!(null));
    assert_eq!(row["tool_restriction"], json!(null));
    assert_eq!(row["compaction"], json!(null));
}

/// A profile row with an empty config gets the boot's materialized
/// value: the row is where the deployment's agent table lands.
#[test]
fn an_empty_row_config_gets_the_boot_materialized_defaults() {
    let mut profile = crate::composition::kernel::native_profile();
    let materialized = agent_loop_row_config(
        Some(&server_config()),
        Some("reviewer"),
        None,
        None,
    );
    materialize_row_config(&mut profile, "agent-loop", materialized.clone());

    let row = profile
        .core
        .iter()
        .find(|entry| entry.id == "agent-loop")
        .expect("the native set has agent-loop");
    assert_eq!(row.config, materialized, "the empty row is filled");
}

/// The profile wins: a row that already carries a config keeps it, so
/// a deployment can pin the loop's configuration in its profile.
#[test]
fn a_profile_row_config_wins_over_the_boot_defaults() {
    let mut profile = crate::composition::kernel::native_profile();
    let row = profile
        .core
        .iter_mut()
        .find(|entry| entry.id == "agent-loop")
        .expect("the native set has agent-loop");
    row.config = json!({ "strategy": "best-of-n" });

    materialize_row_config(
        &mut profile,
        "agent-loop",
        json!({ "strategy": "chain-of-thought" }),
    );

    let row = profile
        .core
        .iter()
        .find(|entry| entry.id == "agent-loop")
        .expect("the native set has agent-loop");
    assert_eq!(
        row.config,
        json!({ "strategy": "best-of-n" }),
        "the profile's own config wins"
    );
}

/// The reader the agent table's strategy alias travels through: the
/// raw name, not a resolved object.
#[test]
fn the_strategy_alias_is_read_raw() {
    let config = server_config();
    assert_eq!(
        crate::state::load_default_strategy_alias(
            Some(&config),
            Some("reviewer")
        ),
        Some("chain-of-thought".to_owned()),
        "the alias is the configuration's own vocabulary"
    );
    assert!(
        crate::state::load_default_strategy_alias(Some(&config), Some("nope"))
            .is_none(),
        "an unnamed agent has no alias"
    );
}
