//! The `mcp.json` declaration file.
//!
//! One file declares the MCP servers a deployment runs:
//!
//! ```json
//! {
//!   "mcpServers": {
//!     "filesystem": {
//!       "command": "npx",
//!       "args": ["-y", "@modelcontextprotocol/server-filesystem", "/srv"],
//!       "env": { "LOG_LEVEL": "info" },
//!       "enabled": true
//!     }
//!   }
//! }
//! ```
//!
//! The shape is the industry's: a `mcpServers` map from a server's
//! name to its declaration. Only **stdio** servers are declared —
//! the command to spawn and the environment to spawn it with. A
//! remote server's transport is not configurable, so there is
//! nothing to declare.
//!
//! # Where the file lives
//!
//! The file is read from the two-layer `~/.agents` layout that
//! [`elysper_core::layout`] owns, so it sits beside the skills,
//! the agents and the tasks of the same layer, and a plugin bundle
//! may carry its own.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::StdioConfig;

/// The name of the declaration file.
pub const MCP_CONFIG_FILE: &str = "mcp.json";

/// A failure while reading an `mcp.json`.
#[derive(Debug, thiserror::Error)]
pub enum McpConfigError {
    /// The file is not valid JSON.
    #[error("cannot parse {path}: {source}")]
    Parse {
        /// The file that could not be parsed.
        path: String,
        /// The underlying parse failure.
        source: serde_json::Error,
    },
    /// The file could not be read.
    #[error("cannot read {path}: {source}")]
    Io {
        /// The file that could not be read.
        path: String,
        /// The underlying I/O failure.
        source: std::io::Error,
    },
}

/// The whole `mcp.json` file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpConfigFile {
    /// The declared servers, keyed by name.
    #[serde(default, rename = "mcpServers")]
    pub servers: HashMap<String, McpServerEntry>,
}

/// One declared MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerEntry {
    /// The program to spawn.
    pub command: String,
    /// The program's arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    /// The program's environment overrides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    /// Whether the server is launched. Defaults to `true`.
    ///
    /// A disabled entry stays in the file — that is the point of
    /// the flag: a deployment can keep a server's declaration
    /// without launching it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

impl McpServerEntry {
    /// Whether the server is launched.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    /// The transport configuration for the server.
    ///
    /// `name` is the name the server is registered under — which
    /// is the *namespaced* name when the declaration came from a
    /// plugin bundle (`<plugin>/<server>`), so the server's tools
    /// land under that namespace too.
    #[must_use]
    pub fn to_stdio_config(&self, name: &str) -> StdioConfig {
        let mut config = StdioConfig::new(&self.command)
            .args(self.args.clone().unwrap_or_default())
            .label(name.to_string());
        for (key, value) in self.env.iter().flatten() {
            config = config.env(key.clone(), value.clone());
        }
        config
    }
}

impl McpConfigFile {
    /// Read an `mcp.json` from disk.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be read or does not parse.
    pub fn read(path: &std::path::Path) -> Result<Self, McpConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| {
            McpConfigError::Io {
                path: path.display().to_string(),
                source,
            }
        })?;
        Self::parse(&text, &path.display().to_string())
    }

    /// Parse an `mcp.json` from text.
    ///
    /// # Errors
    ///
    /// Fails when the text is not valid JSON or does not have the
    /// expected shape.
    pub fn parse(text: &str, path: &str) -> Result<Self, McpConfigError> {
        serde_json::from_str(text).map_err(|source| McpConfigError::Parse {
            path: path.to_string(),
            source,
        })
    }

    /// The declared servers, in a stable order.
    ///
    /// The map's own iteration order is arbitrary; a boot that
    /// registers servers in that order would log them in a
    /// different sequence on every run.
    #[must_use]
    pub fn sorted_servers(&self) -> Vec<(&String, &McpServerEntry)> {
        let mut out: Vec<(&String, &McpServerEntry)> =
            self.servers.iter().collect();
        out.sort_by(|left, right| left.0.cmp(right.0));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_industry_shape_parses() {
        let text = r#"{
            "mcpServers": {
                "filesystem": {
                    "command": "npx",
                    "args": ["-y", "server-filesystem", "/srv"],
                    "env": { "LOG_LEVEL": "info" }
                },
                "off": { "command": "/bin/true", "enabled": false }
            }
        }"#;
        let file = McpConfigFile::parse(text, "mcp.json").expect("a config");
        assert_eq!(file.servers.len(), 2);
        let entry = &file.servers["filesystem"];
        assert_eq!(entry.command, "npx");
        assert_eq!(entry.args.as_ref().map(Vec::len), Some(3));
        assert!(entry.is_enabled());
        assert!(!file.servers["off"].is_enabled());
    }

    #[test]
    fn a_missing_servers_map_is_empty() {
        let file = McpConfigFile::parse("{}", "mcp.json").expect("a config");
        assert!(file.servers.is_empty());
    }

    #[test]
    fn a_broken_file_is_an_error() {
        let error =
            McpConfigFile::parse("{", "mcp.json").expect_err("an error");
        assert!(matches!(error, McpConfigError::Parse { .. }));
    }

    #[test]
    fn the_servers_come_back_in_a_stable_order() {
        let text =
            r#"{"mcpServers":{"b":{"command":"b"},"a":{"command":"a"}}}"#;
        let file = McpConfigFile::parse(text, "mcp.json").expect("a config");
        let names: Vec<&str> = file
            .sorted_servers()
            .into_iter()
            .map(|(name, _)| name.as_str())
            .collect();
        assert_eq!(names, vec!["a", "b"]);
    }
}
