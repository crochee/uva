//! The task declarations and the schedule store.
//!
//! A task has **two layers**:
//!
//! - the **declaration** — a `tasks/<name>/task.md` file in the
//!   two-layer `~/.agents` layout: what should run, how often, and
//!   with which prompt;
//! - the **runtime** — the schedule store, which owns the id, the
//!   next-fire time, the last-firing time and the status.
//!
//! The boot syncs the declarations into the store by **name**:
//!
//! - a declaration whose name matches a stored job updates the
//!   job's kind, payload and description, and keeps the stored id
//!   and schedule state (a running job keeps running);
//! - a declaration that matches nothing creates a new job;
//! - a stored job with no declaration is **exported** as a
//!   `task.md`, so a schedule created through the API does not
//!   vanish when the next boot reads the files.
//!
//! A hand-edited `task.md` therefore takes effect at the next
//! boot — there is no file watch.
use std::path::PathBuf;

use elysper::{
    core::Clock as _,
    scheduler::{
        Job,
        JobId,
        JobKind,
        task_file::{TaskDeclaration, TaskFrontmatter},
        trigger::CronTrigger,
    },
};

use crate::state::AppState;

/// Sync the task declarations into the schedule store.
///
/// # Errors
///
/// A declaration that cannot be read or parsed is skipped with a
/// warning; one broken task must not keep the other tasks from
/// being registered.
pub(crate) fn sync_task_declarations(state: &AppState) {
    let now = state.clock.now();
    let declarations = collect_declarations(&state.workspace_root);
    let mut declared: Vec<String> = Vec::new();
    for declaration in declarations {
        let name = declaration.name().to_string();
        let kind = match declaration.job_kind() {
            Ok(kind) => kind,
            Err(error) => {
                tracing::warn!(
                    task = %name,
                    %error,
                    "skipping malformed task declaration"
                );
                continue;
            }
        };
        declared.push(name.clone());
        match find_by_name(state, &name) {
            Some((id, stored)) => {
                update_stored(state, id, stored, &declaration, kind);
            }
            None => {
                create_stored(state, &declaration, kind, now);
            }
        }
    }
    export_undeclared(state, &declared);
}
/// Collect the task declarations the layout holds.
///
/// The sources come from [`elysper::core::layout`], so the tasks
/// obey the same precedence the skills and the agents obey.
fn collect_declarations(
    workspace_root: &std::path::Path,
) -> Vec<TaskDeclaration> {
    let mut out = Vec::new();
    for source in elysper::core::layout::sources(
        workspace_root,
        elysper::core::layout::Domain::Tasks,
    ) {
        let Ok(entries) = std::fs::read_dir(&source.path) else {
            continue;
        };
        let mut directories: Vec<PathBuf> = entries
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .map(|entry| entry.path())
            .collect();
        directories.sort();
        for directory in directories {
            match TaskDeclaration::read(&directory) {
                Ok(declaration) => out.push(declaration),
                Err(error) => {
                    tracing::warn!(
                        path = %directory.display(),
                        %error,
                        "skipping malformed task declaration"
                    );
                }
            }
        }
    }
    out
}
/// The stored job with the given name, if there is one.
fn find_by_name(state: &AppState, name: &str) -> Option<(JobId, Job)> {
    state
        .schedules
        .list()
        .into_iter()
        .find(|job| job.name == name)
        .map(|job| (job.id.clone(), job))
}
/// Update a stored job from a declaration.
///
/// The id, the next-fire time, the last-firing time and the status
/// are the **runtime's** — a declaration says what the job is, not
/// when it last ran.
fn update_stored(
    state: &AppState,
    id: JobId,
    stored: Job,
    declaration: &TaskDeclaration,
    kind: JobKind,
) {
    let description = declaration.description().to_string();
    let payload = declaration.payload();
    let result = state.schedules.update(&id, |job| {
        job.kind = kind.clone();
        job.payload = payload.clone();
        job.description = description.clone();
    });
    match result {
        Ok(Some(_)) => {
            tracing::info!(
                task = %declaration.name(),
                "updated the schedule from its task declaration"
            );
        }
        Ok(None) => {
            tracing::warn!(
                task = %declaration.name(),
                "the schedule disappeared while it was being updated"
            );
        }
        Err(error) => {
            tracing::warn!(
                task = %declaration.name(),
                %error,
                "could not update the schedule from its task declaration"
            );
        }
    }
    let _ = stored;
}
/// Create a stored job from a declaration.
fn create_stored(
    state: &AppState,
    declaration: &TaskDeclaration,
    kind: JobKind,
    now: chrono::DateTime<chrono::Utc>,
) {
    let next_fire_at = first_fire_at(&kind, now);
    let job = Job::new(
        declaration.name().to_string(),
        kind,
        declaration.payload(),
        next_fire_at,
    )
    .with_description(declaration.description().to_string());
    match state.schedules.add(job) {
        Ok(()) => {
            tracing::info!(
                task = %declaration.name(),
                "registered a schedule from its task declaration"
            );
        }
        Err(error) => {
            tracing::warn!(
                task = %declaration.name(),
                %error,
                "could not register the schedule from its task declaration"
            );
        }
    }
}
/// When a job of this kind first fires.
fn first_fire_at(
    kind: &JobKind,
    now: chrono::DateTime<chrono::Utc>,
) -> chrono::DateTime<chrono::Utc> {
    match kind {
        JobKind::Cron { expr } => {
            CronTrigger::first_after(expr, now).unwrap_or(now)
        }
        JobKind::Once | JobKind::Interval { .. } => now,
    }
}
/// Export the stored jobs that have no declaration.
///
/// The export is the migration direction: a schedule created
/// through the API becomes a `task.md`, so the next boot finds it
/// and keeps it.
fn export_undeclared(state: &AppState, declared: &[String]) {
    let tasks_dir = state
        .workspace_root
        .join(elysper::core::layout::AGENTS_DIR)
        .join("tasks");
    for job in state.schedules.list() {
        if declared.iter().any(|name| name == &job.name) {
            continue;
        }
        let directory = tasks_dir.join(&job.name);
        if directory
            .join(elysper::scheduler::task_file::TASK_FILE)
            .is_file()
        {
            continue;
        }
        let Some(declaration) =
            TaskDeclaration::from_job(&job, directory.clone())
        else {
            continue;
        };
        match elysper::scheduler::task_file::write(&declaration) {
            Ok(()) => {
                tracing::info!(
                    task = %job.name,
                    path = %directory.display(),
                    "exported the schedule as a task declaration"
                );
            }
            Err(error) => {
                tracing::warn!(
                    task = %job.name,
                    %error,
                    "could not export the schedule as a task declaration"
                );
            }
        }
    }
}
/// The frontmatter a declaration would be written with, for the
/// export path.
#[allow(dead_code)]
fn frontmatter_of(declaration: &TaskDeclaration) -> &TaskFrontmatter {
    &declaration.frontmatter
}
