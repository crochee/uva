//! The `task.md` declaration file.
//!
//! A task is a **declaration**: what should run, how often, and
//! with which prompt. The file is:
//!
//! ```text
//! tasks/<name>/task.md
//! ```
//!
//! ```markdown
//! ---
//! name: nightly-tidy
//! description: Tidy the repository's stale branches.
//! kind: cron
//! cron_expr: 0 3 * * *
//! agent: maintainer
//! timeout_secs: 900
//! ---
//!
//! The repository has {count} stale branches. Remove the ones
//! older than a month.
//! ```
//!
//! The frontmatter declares the schedule; the body is the **prompt
//! template** the agent receives when the task fires. The template
//! keys are the same ones the runtime renders — `{now}`, `{date}`,
//! `{time}`, `{job_id}`, `{job_name}`.
//!
//! # Two layers, on purpose
//!
//! The declaration is the file; the **runtime** is the schedule
//! store, which owns the id, the next-fire time, the last-firing
//! time and the status. A boot reads the declarations and syncs
//! them into the store by name: a declaration that matches a stored
//! job updates its kind, payload and description and keeps the
//! stored id and schedule state; a declaration that matches nothing
//! creates a new job.
//!
//! The reverse direction exists too: a stored job with no
//! declaration is exported as a `task.md` on the first boot that
//! finds it, so a schedule created through the API does not vanish
//! when the next boot reads the files.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::{
    agent_action::{AgentRef, JobAction, PromptTemplate},
    job::{Job, JobKind},
};

/// The declaration file inside a task's directory.
pub const TASK_FILE: &str = "task.md";

/// The default wall-clock cap on one firing.
pub const DEFAULT_TIMEOUT_SECS: u64 = 60;

/// A failure while reading a task declaration.
#[derive(Debug, thiserror::Error)]
pub enum TaskFileError {
    /// The directory does not hold a `task.md`.
    #[error("no {TASK_FILE} in {0}")]
    MissingDeclaration(PathBuf),
    /// The `task.md` could not be read.
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
    #[error("task '{name}' lives in a directory named '{directory}'")]
    NameMismatch {
        /// The name the frontmatter declares.
        name: String,
        /// The directory's name.
        directory: String,
    },
    /// The declaration is missing a field its kind needs.
    #[error("task '{name}' is a {kind} task but has no {missing}")]
    MissingField {
        /// The task's name.
        name: String,
        /// The task's kind.
        kind: &'static str,
        /// The field the kind needs.
        missing: &'static str,
    },
}

/// How often a task fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    /// Fires exactly once.
    Once,
    /// Fires on a fixed interval.
    Interval,
    /// Fires on a cron expression.
    Cron,
}

impl TaskKind {
    /// The kind's name, for messages.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Interval => "interval",
            Self::Cron => "cron",
        }
    }
}

/// The YAML frontmatter of a `task.md`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskFrontmatter {
    /// The task's name. Must equal the directory's name.
    pub name: String,
    /// What the task does, for the operator's benefit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// How often the task fires.
    pub kind: TaskKind,
    /// The cron expression, for a `cron` task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron_expr: Option<String>,
    /// The interval in seconds, for an `interval` task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_secs: Option<u64>,
    /// The agent the task runs as.
    pub agent: String,
    /// The wall-clock cap on one firing. Defaults to
    /// [`DEFAULT_TIMEOUT_SECS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

/// One task declaration, read from disk.
#[derive(Debug, Clone)]
pub struct TaskDeclaration {
    /// The frontmatter.
    pub frontmatter: TaskFrontmatter,
    /// The prompt template the task fires with.
    pub prompt_template: PromptTemplate,
    /// The directory the declaration was read from.
    pub directory: PathBuf,
}

impl TaskDeclaration {
    /// The task's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.frontmatter.name
    }

    /// The description the operator sees.
    #[must_use]
    pub fn description(&self) -> &str {
        self.frontmatter.description.as_deref().unwrap_or("")
    }

    /// The wall-clock cap on one firing.
    #[must_use]
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(
            self.frontmatter
                .timeout_secs
                .unwrap_or(DEFAULT_TIMEOUT_SECS),
        )
    }

    /// The schedule kind.
    ///
    /// # Errors
    ///
    /// Fails when a `cron` task has no expression, when an
    /// `interval` task has no interval, and when an `interval` task's
    /// interval is zero.
    pub fn job_kind(&self) -> Result<JobKind, TaskFileError> {
        match self.frontmatter.kind {
            TaskKind::Once => Ok(JobKind::Once),
            TaskKind::Interval => {
                let Some(secs) = self.frontmatter.interval_secs else {
                    return Err(TaskFileError::MissingField {
                        name: self.name().to_string(),
                        kind: "interval",
                        missing: "interval_secs",
                    });
                };
                if secs == 0 {
                    return Err(TaskFileError::MissingField {
                        name: self.name().to_string(),
                        kind: "interval",
                        missing: "interval_secs",
                    });
                }
                Ok(JobKind::Interval {
                    interval: Duration::from_secs(secs),
                })
            }
            TaskKind::Cron => {
                let Some(expr) = self.frontmatter.cron_expr.clone() else {
                    return Err(TaskFileError::MissingField {
                        name: self.name().to_string(),
                        kind: "cron",
                        missing: "cron_expr",
                    });
                };
                Ok(JobKind::Cron { expr })
            }
        }
    }

    /// The payload the schedule store holds for the task.
    ///
    /// The payload is the agent-spawning action: which agent runs,
    /// with which prompt, under which deadline.
    #[must_use]
    pub fn payload(&self) -> serde_json::Value {
        let action = JobAction::Agent {
            agent_ref: AgentRef(self.frontmatter.agent.clone()),
            prompt_template: self.prompt_template.clone(),
            timeout: self.timeout(),
        };
        serde_json::to_value(action).unwrap_or(serde_json::Value::Null)
    }

    /// Read a task declaration from its directory.
    ///
    /// # Errors
    ///
    /// Fails when the directory holds no `task.md`, when the
    /// frontmatter is malformed, when the declared name does not
    /// match the directory's name, or when the frontmatter does not
    /// parse as YAML.
    pub fn read(directory: &Path) -> Result<Self, TaskFileError> {
        let path = directory.join(TASK_FILE);
        let text = std::fs::read_to_string(&path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                TaskFileError::MissingDeclaration(directory.to_path_buf())
            } else {
                TaskFileError::Io {
                    path: path.clone(),
                    source,
                }
            }
        })?;
        let (frontmatter, body) = split_frontmatter(&path, &text)?;
        let directory_name = directory
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if frontmatter.name != directory_name {
            return Err(TaskFileError::NameMismatch {
                name: frontmatter.name,
                directory: directory_name,
            });
        }
        Ok(Self {
            frontmatter,
            prompt_template: PromptTemplate(body),
            directory: directory.to_path_buf(),
        })
    }

    /// The declaration of an existing job, for the export direction.
    ///
    /// A stored job's payload is the agent action; the declaration
    /// is that action's fields, written back as a frontmatter.
    #[must_use]
    pub fn from_job(job: &Job, directory: PathBuf) -> Option<Self> {
        let action: JobAction =
            match serde_json::from_value(job.payload.clone()) {
                Ok(action) => action,
                Err(_) => return None,
            };
        let (agent_ref, prompt_template, timeout) = match action {
            JobAction::Agent {
                agent_ref,
                prompt_template,
                timeout,
            } => (agent_ref, prompt_template, timeout),
        };
        let (kind, cron_expr, interval_secs) = match &job.kind {
            JobKind::Once => (TaskKind::Once, None, None),
            JobKind::Interval { interval } => {
                (TaskKind::Interval, None, Some(interval.as_secs()))
            }
            JobKind::Cron { expr } => {
                (TaskKind::Cron, Some(expr.clone()), None)
            }
        };
        Some(Self {
            frontmatter: TaskFrontmatter {
                name: job.name.clone(),
                description: Some(job.description.clone()),
                kind,
                cron_expr,
                interval_secs,
                agent: agent_ref.as_str().to_string(),
                timeout_secs: Some(timeout.as_secs()),
            },
            prompt_template,
            directory,
        })
    }
}

/// Render a declaration back into its file.
///
/// # Errors
///
/// Fails when the directory cannot be created or the file cannot be
/// written.
pub fn write(declaration: &TaskDeclaration) -> Result<(), std::io::Error> {
    let directory = &declaration.directory;
    std::fs::create_dir_all(directory)?;
    let frontmatter = serde_yaml::to_string(&declaration.frontmatter)
        .map_err(std::io::Error::other)?;
    let document =
        format!("---\n{frontmatter}---\n\n{}", declaration.prompt_template.0);
    std::fs::write(directory.join(TASK_FILE), document)?;
    Ok(())
}

/// Split a `task.md` into its frontmatter and its body.
fn split_frontmatter(
    path: &Path,
    text: &str,
) -> Result<(TaskFrontmatter, String), TaskFileError> {
    let rest =
        text.strip_prefix("---")
            .ok_or_else(|| TaskFileError::NotAMapping {
                path: path.to_path_buf(),
            })?;
    let end = rest
        .find("\n---")
        .ok_or_else(|| TaskFileError::NotAMapping {
            path: path.to_path_buf(),
        })?;
    let yaml = &rest[..end];
    let body = rest[end + 4..].trim_start_matches('\n');
    let frontmatter: TaskFrontmatter =
        serde_yaml::from_str(yaml).map_err(|_| TaskFileError::NotAMapping {
            path: path.to_path_buf(),
        })?;
    if frontmatter.name.trim().is_empty() {
        return Err(TaskFileError::MissingName {
            path: path.to_path_buf(),
        });
    }
    Ok((frontmatter, body.trim_start_matches('\n').to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_task(root: &Path, name: &str, body: &str) -> PathBuf {
        let directory = root.join(name);
        std::fs::create_dir_all(&directory).expect("a directory");
        std::fs::write(directory.join(TASK_FILE), body).expect("the file");
        directory
    }

    #[test]
    fn an_interval_task_reads_its_fields() {
        let root = tempfile::tempdir().expect("a temp root");
        let directory = write_task(
            root.path(),
            "demo",
            "---\nname: demo\nkind: interval\ninterval_secs: 5\nagent: bot\n---\n\nDo the thing at {now}.",
        );
        let declaration =
            TaskDeclaration::read(&directory).expect("a declaration");
        assert_eq!(declaration.name(), "demo");
        assert_eq!(
            declaration.job_kind().expect("a kind"),
            JobKind::Interval {
                interval: Duration::from_secs(5)
            }
        );
        assert_eq!(declaration.timeout(), Duration::from_secs(60));
        assert!(declaration.prompt_template.0.contains("{now}"));
    }

    #[test]
    fn a_cron_task_needs_its_expression() {
        let root = tempfile::tempdir().expect("a temp root");
        let directory = write_task(
            root.path(),
            "demo",
            "---\nname: demo\nkind: cron\nagent: bot\n---\n\nBody.\n",
        );
        let declaration =
            TaskDeclaration::read(&directory).expect("a declaration");
        let error = declaration.job_kind().expect_err("an error");
        assert!(matches!(error, TaskFileError::MissingField { .. }));
    }

    #[test]
    fn the_name_must_match_the_directory() {
        let root = tempfile::tempdir().expect("a temp root");
        let directory = write_task(
            root.path(),
            "demo",
            "---\nname: other\nkind: once\nagent: bot\n---\n\nBody.\n",
        );
        let error = TaskDeclaration::read(&directory).expect_err("an error");
        assert!(matches!(error, TaskFileError::NameMismatch { .. }));
    }

    #[test]
    fn the_payload_is_the_agent_action() {
        let root = tempfile::tempdir().expect("a temp root");
        let directory = write_task(
            root.path(),
            "demo",
            "---\nname: demo\nkind: once\nagent: bot\ntimeout_secs: 30\n---\n\nBody.\n",
        );
        let declaration =
            TaskDeclaration::read(&directory).expect("a declaration");
        let payload = declaration.payload();
        assert_eq!(payload["kind"], "agent");
        assert_eq!(payload["agent_ref"], "bot");
        assert_eq!(payload["timeout"], 30);
    }
}
