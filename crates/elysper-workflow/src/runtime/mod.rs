//! The runtime: admission, execution and journaling.
//!
//! A run is one pass over a [`WorkflowPlan`]. Steps run in order; a
//! fan-out's items and a `best_of` step's candidates run concurrently
//! under two bounds (the run's `Semaphore` and the step's own, when it
//! set one); a pipeline's stages run in order, each fed the previous
//! stage's text. A fan-out reports every text it produced; a `best_of`
//! step reports only its winner's.
//!
//! A `best_of` step then makes the one decision a document cannot
//! express: which candidate won. It is the host's
//! ([`WorkflowHost::select_candidate`]), and it may decline by
//! returning `Ok(None)`, leaving the built-in rule — the first
//! candidate that settled as a success, in item order — in place. The
//! host is asked only for a selection this run ran candidates for: a
//! step answered entirely from the journal reuses the decision that
//! run recorded, so a replay never asks again. Whichever way the
//! decision was made, it is written to the journal, and the candidates
//! it passed over are reported as ones that lost.
//!
//! Every call goes through the same four decisions, in this order:
//!
//! 1. **Replay** — a call inside the journal's matching prefix is
//!    answered from disk and re-recorded, and never spawns.
//! 2. **Control** — a skipped step is reported without spawning; a
//!    paused run parks before it takes a slot.
//! 3. **Admission** — the semaphore bounds how many agents are live at
//!    once, and an abort drains whatever is queued.
//! 4. **Effect** — the host spawns the agent, and then runs the step's
//!    gate if the agent succeeded.
//!
//! The journal is written after each call settles, success or failure.
//! Its only effect on the run is the replay decision in step 1: an
//! append that fails is logged and forgotten.
//!
//! # Module layout
//!
//! - `executor`: the per-run `Executor` that walks the plan.
//! - `execution`: how one attempt settles and how one step's calls
//!   are shaped into its result.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use ulid::Ulid;

use crate::{
    concurrency::{Semaphore, default_max_concurrency},
    control::WorkflowControl,
    error::WorkflowError,
    host::WorkflowHost,
    journal::{JournalEntry, read_journal, replay_lookup},
    plan::{WorkflowCaps, WorkflowPlan},
    result::{CallRun, CallStatus, WorkflowRun},
    scorer::Scorer,
    spec::WorkflowSpec,
};

mod execution;
mod executor;

use executor::Executor;

/// Journal message for a call live control skipped.
pub(crate) const SKIPPED_MESSAGE: &str = "skipped by user";

/// Runs workflow documents against one [`WorkflowHost`].
///
/// The runtime holds no runtime-specific state: it is driven by awaiting
/// [`Self::run`], and everything it does in the world crosses the host.
/// Configure it with the builder methods, then run as many documents as
/// you like; the same semaphore bound applies to each.
pub struct WorkflowRuntime<H> {
    host: H,
    caps: WorkflowCaps,
    max_concurrency: usize,
    journal_path: Option<PathBuf>,
    control: WorkflowControl,
    cwd: Option<PathBuf>,
    /// Scorer a document names as `heuristic`. Documents that ask for
    /// it without registering one error at run time with
    /// [`WorkflowError::MctsScorer`].
    heuristic_scorer: Option<Arc<dyn Scorer>>,
}

impl<H: WorkflowHost> WorkflowRuntime<H> {
    /// A runtime with default caps, the default concurrency bound, no
    pub fn new(host: H) -> Self {
        Self {
            host,
            caps: WorkflowCaps::default(),
            max_concurrency: default_max_concurrency(),
            journal_path: None,
            control: WorkflowControl::new(),
            cwd: None,
            heuristic_scorer: None,
        }
    }

    /// Register the scorer a document names as `heuristic`.
    ///
    /// Documents that name [`MctsScorer::Heuristic`](crate::scorer::MctsScorer::Heuristic)
    /// will use this
    /// scorer; without registration, the run errors with
    /// [`WorkflowError::MctsScorer`].
    pub fn with_heuristic_scorer(mut self, scorer: Arc<dyn Scorer>) -> Self {
        self.heuristic_scorer = Some(scorer);
        self
    }

    /// Replace the caps documents are planned against.
    pub fn with_caps(mut self, caps: WorkflowCaps) -> Self {
        self.caps = caps;
        self
    }

    /// Replace the admission bound. Zero is clamped to one.
    pub fn with_max_concurrency(mut self, max_concurrency: usize) -> Self {
        self.max_concurrency = max_concurrency.max(1);
        self
    }

    /// Read the resume journal from `path`, and append to it as calls
    /// settle. A missing file means "nothing to replay".
    pub fn with_journal(mut self, path: impl Into<PathBuf>) -> Self {
        self.journal_path = Some(path.into());
        self
    }

    /// Install a control handle. The caller keeps a clone to steer the
    /// run; by default the runtime owns a handle nobody else can reach.
    pub fn with_control(mut self, control: WorkflowControl) -> Self {
        self.control = control;
        self
    }

    /// Working directory handed to the host with every request and gate.
    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// The control handle this runtime reads.
    pub fn control(&self) -> &WorkflowControl {
        &self.control
    }

    /// Run one workflow document to completion.
    ///
    /// # Errors
    ///
    /// Planning errors ([`WorkflowError::InvalidSpec`],
    /// [`WorkflowError::CapExceeded`]) before anything spawns, and
    /// [`WorkflowError::Host`] / [`WorkflowError::Gate`] when the host
    /// cannot answer. [`WorkflowError::Selection`] when it answers a
    /// `best_of` step with a candidate that cannot win. An agent that
    /// fails is a settled call, not an error.
    pub async fn run(
        &self,
        spec: &WorkflowSpec,
    ) -> Result<WorkflowRun, WorkflowError> {
        let plan = spec.plan(&self.caps)?;
        let run_id = format!("wf_{}", Ulid::generate());
        let entries = match self.journal_path.as_deref() {
            Some(path) => {
                prepare_journal(path);
                read_journal(path)
            }
            None => Vec::new(),
        };
        let replay = self.replay_map(&plan, &entries);
        tracing::debug!(
            run_id = %run_id,
            workflow = %plan.id(),
            calls = plan.call_count(),
            replayed = replay.len(),
            "workflow run starting",
        );

        let executor = Executor {
            host: &self.host,
            control: &self.control,
            run_id: &run_id,
            cwd: self.cwd.as_deref(),
            journal_path: self.journal_path.as_deref(),
            semaphore: Semaphore::new(self.max_concurrency),
            plan: &plan,
            replay: &replay,
            heuristic_scorer: self.heuristic_scorer.as_ref(),
        };
        let mut execution = executor.drive().await?;
        // An aborted run stops walking; the calls it never reached are
        // reported as such, so the result still lines up with the plan.
        for call in plan.calls().iter().skip(execution.calls.len()) {
            execution.calls.push(CallRun::aborted(call));
        }

        let replayed = execution
            .calls
            .iter()
            .filter(|call| call.status == CallStatus::Replayed)
            .count();
        let spawned = execution
            .calls
            .iter()
            .filter(|call| call.status.crossed_host())
            .count();
        tracing::debug!(
            run_id = %run_id,
            replayed,
            spawned,
            "workflow run finished",
        );
        Ok(WorkflowRun {
            run_id,
            output: execution.output,
            calls: execution.calls,
            replayed,
            spawned,
        })
    }

    /// The journal entries this run may answer calls from.
    fn replay_map(
        &self,
        plan: &WorkflowPlan,
        entries: &[JournalEntry],
    ) -> BTreeMap<usize, JournalEntry> {
        let prefix = plan.replay_prefix(entries);
        let lookup = replay_lookup(entries);
        let mut replay = BTreeMap::new();
        for call in plan.calls().iter().take(prefix) {
            // A pending retry means "run this one live", so it ends the
            // replay there instead of being overridden by the journal.
            if self.control.has_retry(&call.step_id) {
                break;
            }
            if let Some(entry) = lookup.get(&call.position) {
                replay.insert(call.position, JournalEntry::clone(entry));
            }
        }
        replay
    }
}

/// Make sure the journal's directory exists. A failure here is a
/// warning: every append is allowed to fail on its own.
fn prepare_journal(path: &Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    if parent.as_os_str().is_empty() {
        return;
    }
    if let Err(error) = std::fs::create_dir_all(parent) {
        tracing::warn!(
            path = %parent.display(),
            error = %error,
            "workflow journal directory could not be created",
        );
    }
}

#[cfg(test)]
mod tests;
