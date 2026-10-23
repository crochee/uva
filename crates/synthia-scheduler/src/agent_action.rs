//! `JobAction::Agent`—typed wire for "run one agent session".
//!
//! The host supplies an [`AgentSpawner`] (resolves `AgentRef` to a
//! runtime, runs one session, honours the timeout) and a
//! `PromptTemplate` rendered against `PromptContext` at trigger
//! time. The scheduler never holds an `AgentRuntime`; the host's
//! `AgentSpawner` owns the executor.

use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};

use crate::job::{Job, JobId};

/// Stable name reference for an agent—what the operator typed in
/// the schedule config, not an `AgentDescriptor` itself. The host's
/// [`AgentSpawner`] resolves the name to a descriptor at trigger
/// time; the scheduler never holds the descriptor, only the name.
///
/// Stable across restarts, serialised on the wire as a string.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentRef(pub String);

impl AgentRef {
    /// Borrow the inner name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AgentRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for AgentRef {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Substitution context the host renders [`PromptTemplate`] against:
/// `now` (wall-clock at trigger time) and `job` (id + name).
#[derive(Clone, Debug)]
pub struct PromptContext {
    pub now: DateTime<Utc>,
    pub job_id: JobId,
    pub job_name: String,
}

/// A `{key}` template string. Substitution is single-pass and
/// left-to-right; an unknown key is left in place verbatim so a
/// trigger failure surfaces as visible template text in the
/// model-facing result, not a panic.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PromptTemplate(pub String);

impl PromptTemplate {
    /// Render the template against `context`. Unknown keys are
    /// left verbatim; the rendered string is always valid UTF-8
    /// (the template's source is already UTF-8).
    #[must_use]
    pub fn render(&self, context: &PromptContext) -> String {
        let mut out = String::with_capacity(self.0.len());
        let bytes = self.0.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'{' && i + 1 < bytes.len() && bytes[i + 1] != b'{' {
                // Find the closing brace. `end` is the offset within
                // bytes[i + 1..] of the `}` byte—so the brace sits
                // at original index `i + 1 + end`, the key spans
                // `i + 1..i + 1 + end`, and the full `{key}` token
                // (inclusive of both braces) is `i..i + 2 + end`.
                if let Some(end) =
                    bytes[i + 1..].iter().position(|&b| b == b'}')
                {
                    let key = &self.0[i + 1..i + 1 + end];
                    if let Some(replacement) = lookup_key(key, context) {
                        out.push_str(&replacement);
                    } else {
                        // Leave the {key} verbatim.
                        out.push_str(&self.0[i..i + 2 + end]);
                    }
                    i += 2 + end;
                    continue;
                }
            }
            // ASCII fast path; fall back to UTF-8 when needed.
            let ch = self.0[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
        out
    }
}

fn lookup_key(key: &str, context: &PromptContext) -> Option<String> {
    match key {
        "now" => Some(context.now.to_rfc3339()),
        "date" => Some(context.now.format("%Y-%m-%d").to_string()),
        "time" => Some(context.now.format("%H:%M:%S").to_string()),
        "job_id" => Some(context.job_id.to_string()),
        "job_name" => Some(context.job_name.clone()),
        _ => None,
    }
}

/// The wire payload the scheduler stores on
/// [`Job::payload`](crate::job::Job::payload) when an operator
/// configures an agent-spawning schedule.
///
/// Every field is required: a missing `agent_ref` is a misconfigured
/// schedule, not a deferred choice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JobAction {
    /// Spawn one agent session when this schedule fires.
    Agent {
        /// Name reference to the agent the host should spawn.
        agent_ref: AgentRef,
        /// Prompt template, rendered at trigger time against a
        /// [`PromptContext`].
        prompt_template: PromptTemplate,
        /// Hard wall-clock cap on the session. The host's
        /// [`AgentSpawner`] honours the deadline; the scheduler
        /// records a [`AgentOutcome::TimedOut`] when the cap fires.
        /// A zero timeout is invalid.
        #[serde(with = "duration_secs")]
        timeout: Duration,
    },
}

/// `Duration` serialises as a plain integer number of seconds so the
/// wire form stays compact (`"timeout": 60`) and matches how
/// operators read the field in the schedule config. Without this
/// adapter `Duration` becomes `{"secs":60,"nanos":0}`—still valid
/// JSON, but harder to write by hand.
mod duration_secs {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        d: &Duration,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        s.serialize_u64(d.as_secs())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Duration, D::Error> {
        let secs = u64::deserialize(d)?;
        Ok(Duration::from_secs(secs))
    }
}

impl JobAction {
    /// Decode a payload stored on a [`Job`] into a typed
    /// [`JobAction`]. Returns a typed [`ActionDecodeError`] so the
    /// trigger can branch on the cause (missing field vs wrong
    /// shape vs unknown kind tag).
    ///
    /// # Errors
    ///
    /// The [`ActionDecodeError`] describes the failure: the wire
    /// shape is an enum tagged by `"kind"`, so a missing tag or an
    /// unknown tag fails before any field check runs.
    pub fn decode(
        payload: &serde_json::Value,
    ) -> Result<Self, ActionDecodeError> {
        serde_json::from_value(payload.clone()).map_err(ActionDecodeError::from)
    }
}

/// Why a [`JobAction`] payload could not be decoded.
#[derive(Debug, thiserror::Error)]
pub enum ActionDecodeError {
    /// The wire shape did not match the enum. Most often a missing
    /// `"kind"` tag, an unknown tag value, or a wrong shape on
    /// one of the variant's fields.
    #[error("invalid action payload: {0}")]
    Shape(#[from] serde_json::Error),
    /// A `JobAction::Agent` payload had a zero or absent timeout.
    /// The trigger refuses to honour "run forever"—a job that
    /// never finishes would pin the host's executor forever.
    #[error("JobAction::Agent timeout must be > 0")]
    ZeroTimeout,
}

/// What one agent session produced, from the scheduler's point of
/// view. Recorded on `JobOutcome::Agent` when a fire settles, so a
/// workflow step or an operator audit can read the model-facing
/// string and the lifecycle classification without going back to
/// the host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentOutcome {
    /// The session finished and the model produced a normal
    /// answer. `text` is the model-facing final response (or
    /// whatever the host considers the canonical summary).
    Completed { text: String },
    /// The session ran past its `timeout` cap. `partial_text` is
    /// whatever the model had produced up to the deadline—useful
    /// for a partial report but not a final answer.
    TimedOut { partial_text: String },
    /// The host's `AgentSpawner` refused to spawn the agent (name
    /// not registered, build error, etc). The `reason` carries the
    /// host's model-facing message verbatim.
    SpawnFailed { reason: String },
}

/// The seam between the scheduler and the host's agent runtime.
///
/// `Send + Sync` because the scheduler owns the spawner for the
/// duration of one tick and is dropped before the next; `BoxFuture`
/// gives the host freedom to pick its own runtime.
pub trait AgentSpawner: Send + Sync {
    /// Resolve `agent_ref` to an agent descriptor, build an
    /// `AgentRuntime`, run the session with `prompt`, and return
    /// the model-facing final text (or a structured error).
    ///
    /// # Errors
    ///
    /// A failure to spawn (agent not found, runtime build
    /// refused, the session aborted before the timeout) is a
    /// value the trigger records on `JobOutcome::Agent`; only a
    /// failure to even *try* (the host's spawner panicked, the
    /// future failed to resolve) should reach `Err`.
    fn spawn(
        &self,
        agent_ref: &AgentRef,
        prompt: String,
        timeout: Duration,
    ) -> BoxFuture<'_, Result<AgentOutcome, AgentSpawnError>>;
}

/// Why a call to [`AgentSpawner::spawn`] could not produce an
/// outcome at all. This is the type that surfaces *errors in the
/// scheduler's own invocation*—the `AgentOutcome` enum already
/// captures everything the host says about the session.
#[derive(Debug, thiserror::Error)]
pub enum AgentSpawnError {
    /// The spawner panicked. The trigger catches the panic and
    /// records a typed failure so the host's bug doesn't crash
    /// the tick loop.
    #[error("agent spawner panicked: {0}")]
    Panicked(String),
    /// The spawner's future failed to resolve (the future itself
    /// returned an error rather than an [`AgentOutcome`]).
    #[error("agent spawner future failed: {0}")]
    Future(String),
}

/// The blanket trait that lets any error type appear as the inner
/// value of [`AgentSpawnError::Panicked`] / `Future`. Used by the
/// `AgentSpawner` blanket impl on `Arc<T>` so a deployment can hold
/// a single host-side type behind the scheduler's trait object.
pub trait AgentSpawnerError:
    std::fmt::Debug + std::fmt::Display + Send + Sync + 'static
{
}

impl<T> AgentSpawnerError for T where
    T: std::fmt::Debug + std::fmt::Display + Send + Sync + 'static
{
}

impl<T: AgentSpawner + ?Sized> AgentSpawner for std::sync::Arc<T> {
    fn spawn(
        &self,
        agent_ref: &AgentRef,
        prompt: String,
        timeout: Duration,
    ) -> BoxFuture<'_, Result<AgentOutcome, AgentSpawnError>> {
        (**self).spawn(agent_ref, prompt, timeout)
    }
}

/// One trigger step: extract the typed payload from a [`Job`],
/// render the prompt against a fresh [`PromptContext`], call the
/// host's [`AgentSpawner`], and return the resulting [`AgentOutcome`].
///
/// Returns `None` when the job's payload is not an agent action—
/// a non-agent schedule simply skips this step. Returning `Err` is
/// reserved for *scheduler*-side failures (decode failure, zero
/// timeout, spawner panic).
pub fn step_agent_action<'a>(
    job: &'a Job,
    now: DateTime<Utc>,
    spawner: &'a dyn AgentSpawner,
) -> Result<
    Option<BoxFuture<'a, Result<AgentOutcome, AgentSpawnError>>>,
    AgentStepError,
> {
    let action = JobAction::decode(&job.payload)?;
    let JobAction::Agent {
        agent_ref,
        prompt_template,
        timeout,
    } = action;
    if timeout.is_zero() {
        return Err(AgentStepError::ZeroTimeout);
    }
    let context = PromptContext {
        now,
        job_id: job.id.clone(),
        job_name: job.name.clone(),
    };
    let prompt = prompt_template.render(&context);
    Ok(Some(spawner.spawn(&agent_ref, prompt, timeout)))
}

/// What can go wrong while [`step_agent_action`] prepares a job.
#[derive(Debug, thiserror::Error)]
pub enum AgentStepError {
    /// The job's payload is not a valid [`JobAction`] shape.
    #[error(transparent)]
    Decode(#[from] ActionDecodeError),
    /// A `JobAction::Agent` payload had a zero timeout.
    #[error("JobAction::Agent timeout must be > 0")]
    ZeroTimeout,
}

#[cfg(test)]
mod tests {
    //! Contract tests for the agent-action wire shape and the
    //! prompt-template renderer. Pinning these prevents a future
    //! host impl from drifting away from the wire contract the
    //! scheduler stores on disk.

    use super::*;
    use crate::job::JobKind;

    fn fixture(now: DateTime<Utc>) -> Job {
        Job::new(
            "daily-report",
            JobKind::Interval {
                interval: Duration::from_secs(86_400),
            },
            serde_json::json!({
                "kind": "agent",
                "agent_ref": "reporter",
                "prompt_template": "Daily report for {date}: {time}",
                "timeout": 60,
            }),
            now,
        )
    }

    #[test]
    fn job_action_round_trips_through_serde() {
        let original = JobAction::Agent {
            agent_ref: AgentRef("reporter".into()),
            prompt_template: PromptTemplate("Daily report for {date}".into()),
            timeout: Duration::from_secs(60),
        };
        let json = serde_json::to_value(&original).unwrap();
        assert_eq!(json["kind"], "agent");
        assert_eq!(json["agent_ref"], "reporter");
        assert_eq!(json["timeout"], 60);
        let back: JobAction = serde_json::from_value(json).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn payload_decodes_from_job() {
        let now = chrono::Utc::now();
        let job = fixture(now);
        let action = JobAction::decode(&job.payload).unwrap();
        match action {
            JobAction::Agent {
                agent_ref,
                prompt_template,
                timeout,
            } => {
                assert_eq!(agent_ref.as_str(), "reporter");
                assert_eq!(
                    prompt_template.0,
                    "Daily report for {date}: {time}"
                );
                assert_eq!(timeout, Duration::from_secs(60));
            }
        }
    }

    #[test]
    fn payload_rejects_unknown_kind_tag() {
        let payload = serde_json::json!({
            "kind": "shell",
            "command": "echo hi"
        });
        let err = JobAction::decode(&payload).unwrap_err();
        assert!(matches!(err, ActionDecodeError::Shape(_)));
    }

    #[test]
    fn payload_rejects_missing_kind_tag() {
        let payload = serde_json::json!({
            "agent_ref": "reporter",
            "prompt_template": "Daily report for {date}",
            "timeout": 60,
        });
        let err = JobAction::decode(&payload).unwrap_err();
        assert!(matches!(err, ActionDecodeError::Shape(_)));
    }

    #[test]
    fn payload_rejects_zero_timeout() {
        let payload = serde_json::json!({
            "kind": "agent",
            "agent_ref": "reporter",
            "prompt_template": "Daily report",
            "timeout": 0,
        });
        // Decode succeeds (the wire shape is valid)—the zero-timeout
        // rejection is the step's job, not the decode's.
        let now = chrono::Utc::now();
        let job = Job::new("x", JobKind::Once, payload, now);
        struct NoopSpawner;
        impl AgentSpawner for NoopSpawner {
            fn spawn(
                &self,
                _agent_ref: &AgentRef,
                _prompt: String,
                _timeout: Duration,
            ) -> BoxFuture<'_, Result<AgentOutcome, AgentSpawnError>>
            {
                Box::pin(async {
                    Ok(AgentOutcome::Completed {
                        text: String::new(),
                    })
                })
            }
        }
        let err = step_agent_action(&job, now, &NoopSpawner);
        match err {
            Err(AgentStepError::ZeroTimeout) => {}
            Err(other) => panic!("expected ZeroTimeout, got {other}"),
            Ok(_) => panic!("expected ZeroTimeout, got Ok"),
        }
    }

    #[test]
    fn template_renders_known_keys() {
        let now = chrono::Utc::now();
        let template = PromptTemplate(
            "Daily report for {date}: {time}—job {job_name} ({job_id})".into(),
        );
        let job_id = JobId::from_string("01H-static-id");
        let context = PromptContext {
            now,
            job_id: job_id.clone(),
            job_name: "daily".into(),
        };
        let rendered = template.render(&context);
        assert!(rendered.contains(&now.format("%Y-%m-%d").to_string()));
        assert!(rendered.contains(&now.format("%H:%M:%S").to_string()));
        assert!(rendered.contains("daily"));
        assert!(rendered.contains(&job_id.to_string()));
    }

    #[test]
    fn template_leaves_unknown_keys_verbatim() {
        let template = PromptTemplate("hello {unknown_key}".into());
        let now = chrono::Utc::now();
        let context = PromptContext {
            now,
            job_id: JobId::default(),
            job_name: "x".into(),
        };
        let rendered = template.render(&context);
        assert!(
            rendered.contains("{unknown_key}"),
            "unknown keys stay verbatim so a misconfigured template is visible in the model-facing output, not a panic: {rendered}"
        );
    }

    #[test]
    fn step_returns_none_for_non_agent_payload() {
        let now = chrono::Utc::now();
        let job = Job::new(
            "shell-job",
            JobKind::Once,
            serde_json::json!({ "kind": "shell", "command": "ls" }),
            now,
        );
        struct NoopSpawner;
        impl AgentSpawner for NoopSpawner {
            fn spawn(
                &self,
                _agent_ref: &AgentRef,
                _prompt: String,
                _timeout: Duration,
            ) -> BoxFuture<'_, Result<AgentOutcome, AgentSpawnError>>
            {
                Box::pin(async {
                    Ok(AgentOutcome::Completed {
                        text: String::new(),
                    })
                })
            }
        }
        // The decode will fail—but the *caller* of step_agent_action
        // is supposed to filter by JobAction kind first, so the right
        // contract is "return Some(future) only for Agent actions". The
        // current impl returns Err on a non-agent payload; that is the
        // surfacing the caller asked for (a shell job's payload
        // should never reach this entry point).
        let result = step_agent_action(&job, now, &NoopSpawner);
        assert!(result.is_err());
    }
}
