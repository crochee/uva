//! The agent file format.
//!
//! An agent is a **directory**:
//!
//! ```text
//! agents/<name>/
//!   agent.md      — identity, persona and instructions
//!   config.json   — optional structured fields
//! ```
//!
//! `agent.md` is a YAML frontmatter block followed by a markdown
//! body:
//!
//! ```markdown
//! ---
//! name: reviewer
//! description: Reviews a change for correctness and clarity.
//! kind: react
//! version: 0.1.0
//! persona: You are a meticulous code reviewer.
//! display_name: Reviewer
//! ---
//!
//! You review changes. Read the diff, then report what is wrong.
//! ```
//!
//! The body is the agent's **instructions** — the system prompt.
//!
//! `config.json` carries the structured fields the frontmatter does
//! not:
//!
//! ```json
//! {
//!   "tools": ["read", "grep"],
//!   "capabilities": ["streaming"],
//!   "model": "claude-4.6",
//!   "max_iterations": 24,
//!   "handoffs": ["fixer"],
//!   "owner": "platform",
//!   "domain": "coding"
//! }
//! ```
//!
//! # Why two files
//!
//! The split is by **kind**: the frontmatter is the identity a human
//! writes (a name, a sentence about the role), the JSON is the
//! configuration a tool writes. They never share a key, so there is
//! no precedence to resolve and no merge order to get wrong.

use std::path::{Path, PathBuf};

use elysper_core::{AgentDescriptor, layout};
use serde::{Deserialize, Serialize};

/// The definition file inside an agent's directory.
pub const AGENT_FILE: &str = "agent.md";

/// The structured-fields file inside an agent's directory.
pub const AGENT_CONFIG_FILE: &str = "config.json";

/// The default agent paradigm.
pub const DEFAULT_KIND: &str = "react";

/// The default agent version.
pub const DEFAULT_VERSION: &str = "0.1.0";

/// A failure while reading an agent's files.
#[derive(Debug, thiserror::Error)]
pub enum AgentFileError {
    /// The directory does not hold an `agent.md`.
    #[error("no {AGENT_FILE} in {0}")]
    MissingDefinition(PathBuf),
    /// The `agent.md` could not be read.
    #[error("cannot read {path}: {source}")]
    Io {
        /// The file that could not be read.
        path: PathBuf,
        /// The underlying I/O failure.
        source: std::io::Error,
    },
    /// The frontmatter is not a YAML mapping.
    #[error("the frontmatter of {path} is not a YAML mapping")]
    NotAMapping {
        /// The file whose frontmatter is malformed.
        path: PathBuf,
    },
    /// The frontmatter has no `name`.
    #[error("the frontmatter of {path} has no `name`")]
    MissingName {
        /// The file whose frontmatter is malformed.
        path: PathBuf,
    },
    /// The `name` does not match the directory's name.
    #[error("agent '{name}' lives in a directory named '{directory}'")]
    NameMismatch {
        /// The name the frontmatter declares.
        name: String,
        /// The directory's name.
        directory: String,
    },
    /// The `config.json` is not valid JSON.
    #[error("cannot parse {path}: {source}")]
    Config {
        /// The file that could not be parsed.
        path: PathBuf,
        /// The underlying parse failure.
        source: serde_json::Error,
    },
}

/// The YAML frontmatter of an `agent.md`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentFrontmatter {
    /// The agent's name. Must equal the directory's name.
    pub name: String,
    /// One line about what the agent does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The agent paradigm. Defaults to [`DEFAULT_KIND`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The definition's version. Defaults to [`DEFAULT_VERSION`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// A short role-framing sentence surfaced verbatim to the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    /// The human-readable label, distinct from the programmatic id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// The structured fields of an agent, from `config.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Tool names the agent may call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    /// Capability tags for filtering and routing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<String>>,
    /// The preferred model identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The per-run iteration cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_iterations: Option<usize>,
    /// Agents this one may hand off to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoffs: Option<Vec<String>>,
    /// The owning team or service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// The functional domain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// When an orchestrator should route to this agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_hint: Option<String>,
    /// A JSON-schema reference for structured outputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<String>,
}

/// One agent definition, read from disk.
#[derive(Debug, Clone)]
pub struct AgentDefinition {
    /// The identity frontmatter.
    pub frontmatter: AgentFrontmatter,
    /// The structured fields.
    pub config: AgentConfig,
    /// The agent's instructions — the `agent.md` body.
    pub instructions: String,
    /// The directory the definition was read from.
    pub directory: PathBuf,
}

impl AgentDefinition {
    /// The agent's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.frontmatter.name
    }

    /// The composed descriptor.
    ///
    /// The frontmatter supplies the identity; the configuration
    /// supplies the structured fields. Neither shadows the other —
    /// they share no key — so the composition has no precedence to
    /// decide.
    #[must_use]
    pub fn descriptor(&self) -> AgentDescriptor {
        AgentDescriptor {
            name: self.frontmatter.name.clone(),
            description: self
                .frontmatter
                .description
                .clone()
                .unwrap_or_default(),
            kind: self
                .frontmatter
                .kind
                .clone()
                .unwrap_or_else(|| DEFAULT_KIND.to_string()),
            version: self
                .frontmatter
                .version
                .clone()
                .unwrap_or_else(|| DEFAULT_VERSION.to_string()),
            instructions: self.instructions.clone(),
            capabilities: self.config.capabilities.clone().unwrap_or_default(),
            tools: self.config.tools.clone().unwrap_or_default(),
            model_hint: self.config.model.clone(),
            handoffs: self.config.handoffs.clone().unwrap_or_default(),
            handoff_hint: self.config.handoff_hint.clone(),
            output_schema: self.config.output_schema.clone(),
            owner: self.config.owner.clone(),
            domain: self.config.domain.clone(),
            persona: self.frontmatter.persona.clone(),
            display_name: self.frontmatter.display_name.clone(),
            max_iterations: self.config.max_iterations,
        }
    }

    /// Build the file form of a descriptor, rooted in a workspace's
    /// project layer.
    ///
    /// The CRUD surface persists an agent by writing these files;
    /// this is the descriptor→files half of the round-trip
    /// [`Self::descriptor`] completes.
    #[must_use]
    pub fn for_descriptor(
        descriptor: &AgentDescriptor,
        workspace_root: &Path,
    ) -> Self {
        Self {
            frontmatter: AgentFrontmatter {
                name: descriptor.name.clone(),
                description: Some(descriptor.description.clone()),
                kind: Some(descriptor.kind.clone()),
                version: Some(descriptor.version.clone()),
                persona: descriptor.persona.clone(),
                display_name: descriptor.display_name.clone(),
            },
            config: AgentConfig {
                tools: Some(descriptor.tools.clone())
                    .filter(|tools| !tools.is_empty()),
                capabilities: Some(descriptor.capabilities.clone())
                    .filter(|caps| !caps.is_empty()),
                model: descriptor.model_hint.clone(),
                max_iterations: descriptor.max_iterations,
                handoffs: Some(descriptor.handoffs.clone())
                    .filter(|handoffs| !handoffs.is_empty()),
                owner: descriptor.owner.clone(),
                domain: descriptor.domain.clone(),
                handoff_hint: descriptor.handoff_hint.clone(),
                output_schema: descriptor.output_schema.clone(),
            },
            instructions: descriptor.instructions.clone(),
            directory: workspace_root
                .join(layout::AGENTS_DIR)
                .join("agents")
                .join(&descriptor.name),
        }
    }

    /// Read an agent definition from its directory.
    ///
    /// # Errors
    ///
    /// Fails when the directory holds no `agent.md`, when the
    /// frontmatter is malformed, when the declared name does not
    /// match the directory's name, or when `config.json` does not
    /// parse.
    pub fn read(directory: &Path) -> Result<Self, AgentFileError> {
        let path = directory.join(AGENT_FILE);
        let text = std::fs::read_to_string(&path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                AgentFileError::MissingDefinition(directory.to_path_buf())
            } else {
                AgentFileError::Io {
                    path: path.clone(),
                    source,
                }
            }
        })?;
        let (frontmatter, instructions) = split_frontmatter(&path, &text)?;
        let directory_name = directory
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if frontmatter.name != directory_name {
            return Err(AgentFileError::NameMismatch {
                name: frontmatter.name,
                directory: directory_name,
            });
        }
        let config = read_config(directory)?;
        Ok(Self {
            frontmatter,
            config,
            instructions,
            directory: directory.to_path_buf(),
        })
    }
}

/// Read the `config.json` beside an `agent.md`.
///
/// A missing file is not an error: the structured fields are
/// optional, and an agent may be nothing but its frontmatter and
/// its instructions.
fn read_config(directory: &Path) -> Result<AgentConfig, AgentFileError> {
    let path = directory.join(AGENT_CONFIG_FILE);
    if !path.is_file() {
        return Ok(AgentConfig::default());
    }
    let text = std::fs::read_to_string(&path).map_err(|source| {
        AgentFileError::Io {
            path: path.clone(),
            source,
        }
    })?;
    serde_json::from_str(&text)
        .map_err(|source| AgentFileError::Config { path, source })
}

/// Split an `agent.md` into its frontmatter and its body.
///
/// A file with no frontmatter at all is an error: the frontmatter
/// carries the name, and a nameless agent cannot be registered.
fn split_frontmatter(
    path: &Path,
    text: &str,
) -> Result<(AgentFrontmatter, String), AgentFileError> {
    let rest = text.strip_prefix("---").ok_or_else(|| {
        AgentFileError::NotAMapping {
            path: path.to_path_buf(),
        }
    })?;
    // The frontmatter's closing delimiter is the next `---` on a
    // line of its own.
    let end =
        rest.find("\n---")
            .ok_or_else(|| AgentFileError::NotAMapping {
                path: path.to_path_buf(),
            })?;
    let yaml = &rest[..end];
    let body = rest[end + 4..].trim_start_matches('\n');
    let frontmatter: AgentFrontmatter =
        serde_yaml::from_str(yaml).map_err(|_| {
            AgentFileError::NotAMapping {
                path: path.to_path_buf(),
            }
        })?;
    if frontmatter.name.trim().is_empty() {
        return Err(AgentFileError::MissingName {
            path: path.to_path_buf(),
        });
    }
    Ok((frontmatter, body.trim_start_matches('\n').to_string()))
}

/// Render an agent definition back into its two files.
///
/// The inverse of [`AgentDefinition::read`]: the frontmatter is
/// written as YAML, the body as markdown, and the structured fields
/// as JSON. Writing the files is how the CRUD surface persists an
/// agent.
///
/// # Errors
///
/// Fails when the directory cannot be created or a file cannot be
/// written.
pub fn write(definition: &AgentDefinition) -> Result<(), std::io::Error> {
    let directory = &definition.directory;
    std::fs::create_dir_all(directory)?;
    let frontmatter = serde_yaml::to_string(&definition.frontmatter)
        .map_err(std::io::Error::other)?;
    let document =
        format!("---\n{frontmatter}---\n\n{}", definition.instructions);
    std::fs::write(directory.join(AGENT_FILE), document)?;
    let config = serde_json::to_string_pretty(&definition.config)
        .map_err(std::io::Error::other)?;
    std::fs::write(directory.join(AGENT_CONFIG_FILE), format!("{config}\n"))?;
    Ok(())
}
