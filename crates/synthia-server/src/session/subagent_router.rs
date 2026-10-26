//! Sub-agent session router: every delegated child runs in its own
//! first-class session instead of inlining its trace into the parent.
//!
//! The `task` tool forwards a child's events into the parent run's
//! stream wrapped in `AgentEvent::Agent(AgentMeta, inner)`. Before
//! this module existed the controller persisted those wrapped events
//! into the **parent's** log and broadcast them on the parent's SSE —
//! the parent transcript became a flat dump of every child's tool
//! calls, and the child had no addressable session of its own.
//!
//! The router inverts the ownership: a wrapped event arriving at
//! [`ControllerInner::persist_and_broadcast`](super::controller) is
//! handed here and never reaches the parent's log or broadcast.
//! Instead, per child (keyed by `AgentMeta::child_session_id`, a ULID
//! the task tool mints):
//!
//! 1. a `subagent_enter` header row + the dispatch prompt (carried on
//!    the child's first event, see `AgentMeta::prompt`) open the
//!    child's own session log — created through the same
//!    [`SessionRegistry`] every top-level session uses, so the child
//!    shows up in `GET /api/v1/sessions` and replays through
//!    `GET /api/v1/sessions/{id}` exactly like a user conversation;
//! 2. the child's model facts — one `assistant_message` row per
//!    turn, one `tool_result` row per result — are written by the
//!    child's **own harness**, through the child session's writer
//!    this module resolves for it
//!    ([`SubagentWriters`](synthia::harness::SubagentWriters)); the
//!    router copies none of the child's events (none is durable),
//!    so the child's transcript folds with the standard
//!    machinery exactly like a top-level run's;
//! 3. a child `SessionEnded` closes the child log with a
//!    `subagent_exit` footer;
//! 4. the **parent's** log gets only the two lightweight
//!    `subagent_enter` / `subagent_exit` marker rows — enough for the
//!    parent transcript to link the delegation, without any of the
//!    child's content;
//! 5. if a controller for the child session exists (a client opened
//!    its SSE stream), the inner event is broadcast through it, so
//!    the child session streams live like any other.
//!
//! Every row the router writes — the child's own and the parent's
//! markers alike — goes through that session's one writer, so each
//! log keeps one ordinal sequence and every row is stamped with its
//! ordinal. The child's writer is flushed here before the child's
//! live tail fires: the child has no run of its own to flush for it.
//! The parent's markers ride the parent run's flush cadence (each
//! durable parent event flushes, and the run task flushes at the
//! tail).
//!
//! Nested delegation (a child delegating a grandchild) arrives as
//! `Agent(m1, Agent(m2, inner))`. The recursion keeps each level's
//! trace in its own session: the `Agent(m2, …)` wrapper row lands in
//! child `m1`'s log (its record of having delegated), `inner` lands
//! in child `m2`'s log, and only the depth-1 markers reach the root
//! parent's `RunLog`.

use std::sync::Arc;

use dashmap::DashSet;
use synthia::{
    harness::{AgentEvent, AgentMeta, SystemEvent},
    session::{manager::SessionRegistry, subagent_enter, subagent_exit},
};

use super::controller::RunLog;
use crate::state::ActiveSessions;

/// Where a level's marker rows go. Only the ROOT parent's ledgered
/// writer receives them: deeper levels' delegations are already
/// recorded in their parent's log as the wrapped `Agent(meta, …)`
/// row, and a second marker row there would say the same thing
/// twice.
enum ParentSink<'a> {
    Root(&'a mut RunLog),
    Deeper,
}

/// Routes delegated sub-agent events into per-child sessions.
pub struct SubagentRouter {
    registry: Arc<SessionRegistry>,
    active: Arc<ActiveSessions>,
    /// Child session ids whose own header + prompt rows are
    /// written — by [`Self::open_child_session`] when the run
    /// carried a writer source, otherwise by [`Self::route`] on
    /// the first event. Ids are ULIDs minted per delegation, so an
    /// id is never legitimately reused.
    opened: DashSet<String>,
    /// Child session ids whose `subagent_enter` marker has been
    /// written to the parent's log.
    marked: DashSet<String>,
}

impl SubagentRouter {
    /// Build a router over the process-wide session registry and
    /// controller cache.
    #[must_use]
    pub fn new(
        registry: Arc<SessionRegistry>,
        active: Arc<ActiveSessions>,
    ) -> Self {
        Self {
            registry,
            active,
            opened: DashSet::new(),
            marked: DashSet::new(),
        }
    }

    /// Open the child session `meta` names and return its writer.
    ///
    /// Called by the delegation runner (through
    /// [`ChildSessionWriters`]) before the child run starts: the
    /// enter header and dispatch prompt are stamped now, so the
    /// child's own first row — its `request_header`, its first
    /// assistant turn — lands after them. The rows take the
    /// writer's synchronous path; the child's first flush lands
    /// them.
    pub(crate) fn open_child_session(
        &self,
        user_id: &str,
        parent_session_id: &str,
        meta: &AgentMeta,
    ) -> Arc<synthia::session::LogWriter> {
        let child_id = meta.child_session_id.trim().to_string();
        let writer = self.registry.writer(user_id, &child_id);
        if !is_safe_session_id(&child_id) {
            return writer;
        }
        if self.opened.insert(child_id.clone()) {
            let depth: u32 = meta.parent_depth.try_into().unwrap_or(u32::MAX);
            let agent_name = meta.agent_name.as_deref().unwrap_or("unknown");
            if let Ok(header) = serde_json::to_value(subagent_enter(
                &child_id,
                parent_session_id,
                depth,
                agent_name,
            )) {
                writer.append_nowait(header);
            }
            if let Some(prompt) = meta.prompt.as_ref().filter(|p| !p.is_empty())
            {
                writer.append_nowait(serde_json::json!({
                    "type": "UserInput",
                    "data": { "text": prompt },
                }));
            }
        }
        writer
    }

    /// Route one wrapped child event (and, recursively, any deeper
    /// wrapped levels) into the child sessions it belongs to.
    ///
    /// `root_log` is the ROOT parent's ledgered writer: only the
    /// depth-1 markers go through it, because it is the one log a
    /// compaction checkpoint validates provenance against.
    pub(crate) async fn route(
        &self,
        user_id: &str,
        parent_session_id: &str,
        meta: &AgentMeta,
        event: &AgentEvent,
        root_log: &mut RunLog,
    ) {
        if let Err(error) = self
            .route_level(
                user_id,
                parent_session_id,
                meta,
                event,
                ParentSink::Root(root_log),
            )
            .await
        {
            tracing::warn!(
                target: "synthia.session",
                parent_session_id,
                child_session_id = %meta.child_session_id,
                %error,
                "subagent router: a write failure left the child's \
                 rows owed to the log; the next flush writes them"
            );
        }
    }

    /// One level of the recursion. See the module docs for the row
    /// layout; errors bubble as `anyhow` because every failure mode
    /// is a sink write the caller can only log.
    async fn route_level(
        &self,
        user_id: &str,
        parent_session_id: &str,
        meta: &AgentMeta,
        event: &AgentEvent,
        parent: ParentSink<'_>,
    ) -> anyhow::Result<()> {
        // Async recursion must be boxed; the depth is bounded by
        // `MAX_SUBAGENT_DEPTH`, so one box per level is bounded too.
        Box::pin(self.route_level_inner(
            user_id,
            parent_session_id,
            meta,
            event,
            parent,
        ))
        .await
    }

    async fn route_level_inner(
        &self,
        user_id: &str,
        parent_session_id: &str,
        meta: &AgentMeta,
        event: &AgentEvent,
        mut parent: ParentSink<'_>,
    ) -> anyhow::Result<()> {
        let child_id = meta.child_session_id.trim().to_string();
        if !is_safe_session_id(&child_id) {
            return Ok(());
        }
        let depth: u32 = meta.parent_depth.try_into().unwrap_or(u32::MAX);
        let agent_name = meta.agent_name.as_deref().unwrap_or("unknown");
        // The child's rows go through the child session's writer —
        // a different session, so a different writer, but the same
        // rule: one writer per session, one ordinal source. The
        // writer stamps each row's `seq` and `ts`.
        let writer = self.registry.writer(user_id, &child_id);

        if self.marked.insert(child_id.clone()) {
            let header = serde_json::to_value(subagent_enter(
                &child_id,
                parent_session_id,
                depth,
                agent_name,
            ))?;
            // The child's own header + prompt were pre-written by
            // `open_child_session` when the run carried a writer
            // source; without one this route is the only writer
            // they have, so they land here.
            if !self.opened.contains(&child_id) {
                writer.append(header.clone()).await?;
                if let Some(prompt) =
                    meta.prompt.as_ref().filter(|p| !p.is_empty())
                {
                    writer
                        .append(serde_json::json!({
                            "type": "UserInput",
                            "data": { "text": prompt },
                        }))
                        .await?;
                }
            }
            self.append_to_parent(&mut parent, header).await?;
        }

        match event {
            AgentEvent::Agent(inner_meta, inner_event) => {
                // The wrapped level below is THIS child's record
                // having delegated; the innermost events belong to
                // the grandchild's own session. The wrapped row
                // itself is not copied: the child's model facts
                // come from the child's own writer (see
                // [`SubagentWriters`]), never from this event tap.
                self.route_level(
                    user_id,
                    &child_id,
                    inner_meta,
                    inner_event,
                    ParentSink::Deeper,
                )
                .await?;
            }
            inner => {
                // Nothing to copy: no `AgentEvent` is durable any
                // more — the child's harness writes its own rows
                // through the child session's writer. Only the
                // exit footer is the router's to record.
                if let AgentEvent::System(SystemEvent::SessionEnded {
                    reason,
                }) = inner
                {
                    let footer = serde_json::to_value(subagent_exit(
                        &child_id,
                        depth,
                        reason.as_str(),
                    ))?;
                    writer.append(footer.clone()).await?;
                    self.append_to_parent(&mut parent, footer).await?;
                }
            }
        }

        // The child's rows are durable before the session is read or
        // streamed: the child has no run of its own to flush for it,
        // so this level's flush is the one that lands them.
        // A failed flush does not drop the rows — they wait in the
        // writer's spill — and the child's last event has no later
        // flush to lean on, so one retry catches a transient write
        // error before the failure is surfaced.
        let flushed = match writer.flush().await {
            Ok(()) => Ok(()),
            Err(first) => {
                tracing::warn!(
                    target: "synthia.session",
                    child_session_id = %child_id,
                    %first,
                    "subagent router: retrying the child's flush"
                );
                writer.flush().await
            }
        };
        if let Err(error) = flushed {
            return Err(anyhow::anyhow!(
                "the child's rows stay owed to the log after a failed \
                 flush: {error}"
            ));
        }

        // Live tail: a client that opened the child session's stream
        // (which materialised a controller through the normal
        // get-or-create path) sees the event on that session's own
        // broadcast, exactly like a top-level run's subscribers do.
        if let Some(controller) =
            self.active.get(&(user_id.to_string(), child_id.clone()))
        {
            controller.broadcast_event(event);
        }
        Ok(())
    }

    /// Write a marker row to the root parent's ledgered log. Deeper
    /// levels pass [`ParentSink::Deeper`] and write nothing — see
    /// the enum's docs.
    async fn append_to_parent(
        &self,
        parent: &mut ParentSink<'_>,
        row: serde_json::Value,
    ) -> anyhow::Result<()> {
        if let ParentSink::Root(log) = parent {
            log.append_typed(row).await?;
        }
        Ok(())
    }

    /// Session registry the router creates child sinks through.
    #[must_use]
    pub fn registry(&self) -> &Arc<SessionRegistry> {
        &self.registry
    }
}

/// [`SubagentWriters`](synthia::harness::SubagentWriters) bound to
/// one parent run: a delegated child's harness writes its model
/// facts through the child session's own writer, opened here
/// through the router so the child's log opens in order.
pub struct ChildSessionWriters {
    router: Arc<SubagentRouter>,
    user_id: String,
    parent_session_id: String,
}

impl ChildSessionWriters {
    /// Open child sessions for `user_id`'s delegations out of
    /// `parent_session_id`.
    pub fn new(
        router: Arc<SubagentRouter>,
        user_id: String,
        parent_session_id: String,
    ) -> Self {
        Self {
            router,
            user_id,
            parent_session_id,
        }
    }
}

impl synthia::harness::SubagentWriters for ChildSessionWriters {
    fn open_child(&self, meta: &AgentMeta) -> Arc<synthia::session::LogWriter> {
        self.router.open_child_session(
            &self.user_id,
            &self.parent_session_id,
            meta,
        )
    }
}

/// The same shape [`SessionRegistry`] validates ids with, checked
/// here defensively: the child id reaches `sessions_root`'s path via
/// `sink()`, which does not validate on its own. ULIDs always pass;
/// this guards a future delegator that mints something else.
fn is_safe_session_id(id: &str) -> bool {
    !id.is_empty()
        && !id.contains('/')
        && !id.contains('\\')
        && !id.contains('\0')
        && !id.starts_with('.')
}

#[cfg(test)]
mod tests {
    use synthia::provider::{ContentPart, TextContent};

    use super::*;

    fn text_event(text: &str) -> AgentEvent {
        AgentEvent::Model(ContentPart::Text(TextContent {
            text: text.to_string(),
            cache_control: None,
        }))
    }

    fn ended_event() -> AgentEvent {
        AgentEvent::System(SystemEvent::SessionEnded {
            reason: synthia::harness::SessionEndReason::Completed,
        })
    }

    fn row_types(events: &[serde_json::Value]) -> Vec<String> {
        events
            .iter()
            .map(|ev| {
                ev.get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("?")
                    .to_string()
            })
            .collect()
    }

    /// The contract the whole design hangs on: a delegated child's
    /// durable events land in the child's own session log, while the
    /// parent's log keeps only the enter / exit markers — the child's
    /// trace never inlines into the parent transcript.
    #[tokio::test]
    async fn child_events_land_in_their_own_session() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = Arc::new(SessionRegistry::new(dir.path().to_path_buf()));
        let router = SubagentRouter::new(
            Arc::clone(&registry),
            Arc::new(ActiveSessions::new()),
        );
        let parent_sink = registry.sink("u1", "parent-1");
        let mut log = RunLog::new(registry.writer("u1", "parent-1"));

        let meta = AgentMeta::new("parent-1", "child-1", 1)
            .with_agent_name(Some("scout".to_string()));
        // First forwarded event carries the dispatch prompt.
        let first_meta = meta.clone().with_prompt(Some("map the repo".into()));
        // The child's run opens its own session before its first
        // row: the opener stamps the header + prompt, and the
        // child's harness writes its turn as one assistant row
        // through the same writer — the shape this test then
        // expects the routing below to fit around.
        let child_writer =
            router.open_child_session("u1", "parent-1", &first_meta);
        child_writer.append_nowait(serde_json::json!({
            "type": "assistant_message",
            "data": {"role": "assistant",
                     "content": {"Multi": [
                         {"type": "text", "text": "step one"}
                     ]}},
        }));
        router
            .route(
                "u1",
                "parent-1",
                &first_meta,
                &text_event("step one"),
                &mut log,
            )
            .await;
        router
            .route("u1", "parent-1", &meta, &ended_event(), &mut log)
            .await;
        // The parent's markers ride the parent run's flush cadence;
        // this test has no run, so the flush is the test's.
        registry
            .writer("u1", "parent-1")
            .flush()
            .await
            .expect("parent flush");
        let child_events = registry
            .sink("u1", "child-1")
            .read()
            .await
            .expect("child read");
        assert_eq!(
            row_types(&child_events),
            vec![
                "subagent_enter",
                "UserInput",
                "assistant_message",
                "subagent_exit"
            ],
            "child log: header, prompt, the child's own assistant \
             row, footer — the child's model facts come from the \
             child's writer, never inlined from the parent's stream"
        );
        assert_eq!(
            child_events[1]
                .pointer("/data/text")
                .and_then(|t| t.as_str()),
            Some("map the repo")
        );
        assert_eq!(
            child_events[0]
                .pointer("/data/agent_name")
                .and_then(|a| a.as_str()),
            Some("scout")
        );
        assert_eq!(
            child_events[0]
                .pointer("/data/parent_session_id")
                .and_then(|p| p.as_str()),
            Some("parent-1")
        );

        let parent_events = parent_sink.read().await.expect("parent read");
        assert_eq!(
            row_types(&parent_events),
            vec!["subagent_enter", "subagent_exit"],
            "parent log: markers only, none of the child's content"
        );
    }

    /// A grandchild delegation arrives as `Agent(m1, Agent(m2, …))`;
    /// each level's trace stays in its own session and the root
    /// parent still sees only its direct child's markers.
    #[tokio::test]
    async fn nested_delegation_splits_per_level() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = Arc::new(SessionRegistry::new(dir.path().to_path_buf()));
        let router = SubagentRouter::new(
            Arc::clone(&registry),
            Arc::new(ActiveSessions::new()),
        );
        let parent_sink = registry.sink("u1", "root");
        let mut log = RunLog::new(registry.writer("u1", "root"));

        let m1 = AgentMeta::new("root", "child-1", 1)
            .with_agent_name(Some("scout".to_string()));
        let m2 = AgentMeta::new("child-1", "child-2", 2)
            .with_agent_name(Some("deep".to_string()));
        // The grandchild's run opens its own session and writes
        // its turn through it, exactly one level down.
        let grandchild_writer = router.open_child_session("u1", "child-1", &m2);
        grandchild_writer.append_nowait(serde_json::json!({
            "type": "assistant_message",
            "data": {"role": "assistant",
                     "content": {"Multi": [
                         {"type": "text", "text": "deep work"}
                     ]}},
        }));
        let nested = AgentEvent::Agent(m2, Box::new(text_event("deep work")));
        router
            .route(
                "u1",
                "root",
                &m1.clone().with_prompt(Some("outer".into())),
                &nested,
                &mut log,
            )
            .await;
        registry
            .writer("u1", "root")
            .flush()
            .await
            .expect("parent flush");

        let grandchild = registry
            .sink("u1", "child-2")
            .read()
            .await
            .expect("grandchild read");
        assert_eq!(
            row_types(&grandchild),
            vec!["subagent_enter", "assistant_message"],
            "grandchild log: its own header + its own assistant row, \
             written by the grandchild's writer"
        );
        assert_eq!(
            grandchild[0]
                .pointer("/data/parent_session_id")
                .and_then(|p| p.as_str()),
            Some("child-1"),
            "grandchild attribution names the child, not the root"
        );

        let child = registry
            .sink("u1", "child-1")
            .read()
            .await
            .expect("child read");
        assert_eq!(
            row_types(&child),
            vec!["subagent_enter", "UserInput"],
            "child log: header + prompt; the wrapped grandchild \
             trace is not inlined here"
        );
        let parent_events = parent_sink.read().await.expect("parent read");
        assert_eq!(
            row_types(&parent_events),
            vec!["subagent_enter"],
            "root parent: only its direct child's enter marker"
        );
    }
}
