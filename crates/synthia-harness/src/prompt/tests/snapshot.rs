//! Tests for the `RuntimeContext` snapshot seam.
//!
//! The snapshot is the per-dispatch volatile-fact carrier
//! (cwd, today, model id, platform). It is rendered as a
//! user-role message in front of every iteration so the system
//! prompt stays byte-stable for prompt caching. The tests below
//! pin:
//!
//! - the supersession frame that opens the snapshot body
//!   (any wording change invalidates every prior cached
//!   prefix),
//! - the absence of a git line and the presence of a real
//!   model id, since both used to render `unknown` forever,
//! - the use of an *injected* clock so two consecutive
//!   `from_runtime` calls with the same `now` produce
//!   byte-identical snapshots — the loop-level invariant
//!   that lets the re-append step short-circuit when
//!   nothing changed.

use super::{super::RuntimeContext, support::fixed_runtime_context};

const NOW: chrono::DateTime<chrono::Utc> =
    chrono::DateTime::from_timestamp(1_767_225_600, 0)
        .expect("2026-01-01T00:00:00Z is a valid unix second");

// Frame wording is part of the prompt-cache contract, not free prose.
#[test]
fn runtime_context_renders_snapshot_with_supersession_frame() {
    let ctx = fixed_runtime_context();
    let snapshot = ctx.render_snapshot();

    assert!(
        snapshot.starts_with(
            "Current runtime context. This snapshot supersedes \
             earlier runtime-context snapshots."
        ),
        "snapshot must start with the supersession frame; \
         got:\n{snapshot}"
    );
    assert!(snapshot.contains("Working directory: /tmp/cwd"));
    assert!(snapshot.contains("Workspace root: /tmp/worktree"));
    assert!(snapshot.contains("Platform: linux"));
    assert!(snapshot.contains("Today: Mon Jan 1 2026"));
    assert!(snapshot.contains("Model: anthropic/claude-4.6"));
    assert!(
        !snapshot.contains("Is directory a git repo:"),
        "the harness has no git dependency, so a permanent `unknown` git \
         line is pure prompt cost; got:\n{snapshot}"
    );
}

#[test]
fn runtime_context_from_runtime_populates_required_fields() {
    let ctx = RuntimeContext::from_runtime("/tmp/worktree", "acme/warp", NOW);
    assert_eq!(ctx.worktree, "/tmp/worktree");
    assert!(!ctx.platform.is_empty(), "platform must be non-empty");
    assert!(!ctx.today.is_empty(), "today must be non-empty");
    assert_eq!(
        ctx.model_id.as_deref(),
        Some("acme/warp"),
        "from_runtime must carry the model id the loop already holds"
    );
    // cwd may be `unknown` if the test runner has no cwd;
    // either is fine — the contract is "non-panicking".
    let _ = ctx.cwd;
}

#[test]
fn runtime_context_from_runtime_uses_injected_clock() {
    let ctx = RuntimeContext::from_runtime("/tmp/worktree", "acme/warp", NOW);
    assert_eq!(ctx.today, "Thu Jan 1 2026");
}

#[test]
fn runtime_context_render_is_pure_and_deterministic() {
    let a = fixed_runtime_context();
    let b = fixed_runtime_context();
    assert_eq!(a.render_snapshot(), b.render_snapshot());
}

#[test]
fn runtime_context_with_same_clock_yields_identical_body() {
    let a = RuntimeContext::from_runtime("/tmp/worktree", "acme/warp", NOW);
    let b = RuntimeContext::from_runtime("/tmp/worktree", "acme/warp", NOW);
    assert_eq!(a.render_snapshot(), b.render_snapshot());
}

#[test]
fn runtime_context_with_different_model_yields_different_body() {
    let a = RuntimeContext::from_runtime("/tmp/worktree", "acme/warp", NOW);
    let b = RuntimeContext::from_runtime("/tmp/worktree", "acme/other", NOW);
    assert_ne!(a.render_snapshot(), b.render_snapshot());
    assert!(a.render_snapshot().contains("Model: acme/warp"));
    assert!(b.render_snapshot().contains("Model: acme/other"));
}
