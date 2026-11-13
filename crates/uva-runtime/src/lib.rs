//! # uva-runtime
//!
//! Config-driven assembly for uva's built-in native plugins.
//!
//! A deployment's plugin set is assembled from profile entries: the
//! factories here read each entry's `config` and build the plugin that
//! publishes the matching service (`llm` / `llm.default`, `agentLoop`).
//! Hosts — the app-server gateway and external lib consumers alike —
//! mount through this crate, so a deployment's provider and loop
//! construction has one implementation.
//!
//! The crate starts with the stage-1 pair: [`model_adapter`] (the
//! provider catalogue plus the tier-1 wrapping chain) and
//! [`agent_loop`] (the `ReActAgent` assembly). Remaining built-ins
//! migrate here as their boot-time construction is solidified; until
//! then the app-server boot still pre-builds a fallback catalogue for
//! its pre-mount consumers (the N3 seam documented on
//! [`ModelAdapterServicePlugin`]).

pub mod agent_loop;
pub mod model_adapter;
pub mod tier1;

pub use agent_loop::{
    AGENT_LOOP_INJECTS,
    AgentLoopDefaults,
    AgentLoopServicePlugin,
    CompactionBoot,
    agent_loop_manifest,
    agent_loop_service_plugin,
    server_agent_loop,
};
pub use model_adapter::{
    BudgetSection,
    CacheSection,
    ModelAdapterConfig,
    ModelAdapterServicePlugin,
    ModelSelectionError,
    ProviderCatalogue,
    ResilienceSection,
    UNDECLARED_MODEL,
    catalogue_from_config,
    model_adapter_manifest,
    model_adapter_plugin,
};
pub use tier1::wrap_tier1_providers;
