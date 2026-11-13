//! The boot: resolve the deployment config, build the process state.
//!
//! One read of the config, one state, one boot. The core mode builds
//! the state and hands it to the JSON-RPC pump; the gateway mode
//! spawns a core subprocess and never builds a state of its own.
//!
//! # What the boot owns
//!
//! - the deployment config (`--config`, then `<directory>/config.toml`,
//!   then `<directory>/.uva/config.toml`) — resolved **once**
//!   (R53), because the bind address depends on it and a second read
//!   would be a second source of truth;
//! - the [`AppState`]: providers, the tool registry, the agent
//!   registry, the session registry, the scheduler, the search
//!   service, the attachment store, the memory tier and the MCP
//!   clients;
//! - the background tasks that outlive a single request: the
//!   scheduler driver and the dead-session reaper.
/// A booted core: the state behind every method, and the address the
/// deployment asked for.
use std::{path::PathBuf, sync::Arc};

use crate::{
    config::server::{BindAddress, BindOverrides},
    profile::{Profile, ProfileError, dump_with_manifests},
    state::AppState,
};

pub struct BootedCore {
    /// The state the methods serve.
    pub state: Arc<AppState>,
    /// Resolved from `--host`/`--port`, `UVA_HOST`/`UVA_PORT`,
    /// then the deployment config's `host`/`port`, then the defaults.
    pub address: BindAddress,
}
/// Resolve the deployment config, build the state, and hand both back.
///
/// # Errors
///
/// Fails when no provider can be built — the deployment has no
/// `providers` entry and no `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` in
/// the environment.
pub async fn boot_core(
    workspace_root: PathBuf,
    config_path: Option<&PathBuf>,
    profile: Option<Profile>,
    bind_overrides: BindOverrides,
    profile_store: Option<crate::profile::ProfileStore>,
) -> Result<BootedCore, String> {
    let server_config =
        crate::config::resolve_server_config(&workspace_root, config_path);
    let address = BindAddress::resolve(bind_overrides, server_config.as_ref());
    let state = AppState::with_server_config_and_profile(
        workspace_root,
        server_config,
        profile,
        profile_store,
    )
    .await?;
    Ok(BootedCore { state, address })
}

/// Render a resolved profile with each core row's manifest services.
///
/// The factory directory carries the concrete plugins' manifests, so
/// `provides` / `inject` are what the kernel will order and bind. A row
/// with no registered factory prints explicit empty lists.
///
/// # Errors
///
/// Returns [`ProfileError`] when a registered factory's manifest names a
/// different id than the row it is bound to.
pub fn dump_profile(profile: &Profile) -> Result<String, ProfileError> {
    let factories = crate::composition::kernel::dump_factories();
    dump_with_manifests(profile, &factories)
}
