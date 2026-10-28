//! Cache-stable runtime-context snapshots: per-dispatch volatile facts ride a **user-role snapshot message** with a fixed supersession frame, appended only when the rendered text differs from the previous one — never in the system message, which would invalidate the provider prefix cache every turn. `RuntimeContext::from_runtime` takes an injected clock so tests can pin `today`.
use chrono::{DateTime, Utc};

// Prefix prepended to every snapshot body. The model reads the frame as a
// *supersession marker*, not a one-off user turn. Changing the wording is a
// breaking prompt-cache contract — every prior cached prefix becomes stale.
const SNAPSHOT_FRAME: &str = "Current runtime context. This snapshot supersedes \
     earlier runtime-context snapshots.";

#[derive(Clone, Debug)]
pub(crate) struct RuntimeContext {
    pub cwd: String,
    pub worktree: String,
    pub platform: String,
    pub today: String,
    pub model_id: Option<String>,
}

impl RuntimeContext {
    pub(crate) fn from_runtime(
        workspace_root: &str,
        model_id: &str,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            cwd: std::env::current_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| String::from("unknown")),
            worktree: workspace_root.to_string(),
            platform: std::env::consts::OS.to_string(),
            today: now.format("%a %b %-d %Y").to_string(),
            model_id: Some(model_id.to_string()),
        }
    }

    pub(crate) fn render_snapshot(&self) -> String {
        let model = self.model_id.as_deref().unwrap_or("unknown");
        format!(
            "{frame}\n\n  Working directory: {cwd}\n  \
             Workspace root: {worktree}\n  \
             Platform: {platform}\n  \
             Today: {today}\n  \
             Model: {model}",
            frame = SNAPSHOT_FRAME,
            cwd = self.cwd,
            worktree = self.worktree,
            platform = self.platform,
            today = self.today,
            model = model,
        )
    }
}
