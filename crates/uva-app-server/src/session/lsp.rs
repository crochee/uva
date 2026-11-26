//! The deployment's language servers.
//!
//! The `lsp` tool asks the server that speaks a language ✓; the
//! deployment decides which servers exist ✓ — the `[lsp]` section of
//! its configuration ✓. This is the one place the configuration's
//! servers become a value the tool can ask ✓.

use std::sync::Arc;

use uva::tool_lsp::{LspServer, LspServers};

/// The language servers the deployment's configuration names ✓.
#[derive(Debug, Clone, Default)]
pub struct DeploymentLspServers {
    servers: Vec<LspServer>,
}

impl DeploymentLspServers {
    /// The servers the configuration names ✓.
    #[must_use]
    pub fn new(servers: Vec<LspServer>) -> Self {
        Self { servers }
    }
}

impl LspServers for DeploymentLspServers {
    fn servers(&self) -> Vec<LspServer> {
        self.servers.clone()
    }
}

/// The servers as the tool's seam holds them ✓.
#[must_use]
pub fn servers_handle(servers: Vec<LspServer>) -> Arc<dyn LspServers> {
    Arc::new(DeploymentLspServers::new(servers))
}
