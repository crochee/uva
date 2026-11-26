//! The one transactional profile write path, shared by the JSON-RPC
//! `profile/*` methods and the self-MCP `profile` tool.
//!
//! # The transaction
//!
//! 1. Take the single-writer lock.
//! 2. Read the document layer (a missing file materializes the raw
//!    profile, so a zero-config deployment's first write works).
//! 3. Apply the edit, serialize, and re-parse the text — a document the
//!    next boot could not read never reaches the disk.
//! 4. Re-resolve the whole layer stack in memory.
//! 5. `overridden`: a higher layer owns the requested field, so nothing
//!    is written or mounted.
//! 6. Try the mount ([`AppState::apply_resolved`] — build before unload,
//!    rollback on failure). A failure leaves both the document and the
//!    tree unchanged.
//! 7. Commit the document with an atomic rename.
//! 8. Audit and broadcast `profile/changed`.
//!
//! Step 6 before step 7 is the D21 order. If the rename fails after the
//! mount succeeded, the outcome is `failed` with the self-healing note
//! in its reasons: an `hmr = true` deployment re-resolves back to the
//! old file, an `hmr = false` one keeps the new tree until restart.

use serde_json::Value;
use uva::core::{Clock as _, registry::Registry as _};

use super::AppState;
use crate::{
    api::v1::profile::ProfileRow,
    profile::{
        CapabilityAction,
        CapabilityDomain,
        ChangeOutcome,
        ChangeState,
        Profile,
        ProfileAction,
        ProfileStore,
        RowEdit,
    },
};

impl AppState {
    /// Run one profile transaction.
    ///
    /// `actor` is `"ui"`, `"agent"`, or `"mcp"` — audit metadata, never
    /// an authorization decision (the self-MCP write tools carry MCP's
    /// `destructiveHint`, which the existing approval stance holds).
    pub async fn profile_apply(
        &self,
        actor: &str,
        action: ProfileAction,
    ) -> ChangeOutcome {
        let _guard = self.assembly.writer().lock().await;
        let Some(store) = self.assembly.store() else {
            return ChangeOutcome::failed(
                "no profile document is configured for this process",
            );
        };
        let ids = affected_ids(&action);
        let outcome = match action {
            ProfileAction::Edit(edit) => self.apply_row_edit(store, edit).await,
            ProfileAction::Capability(capability) => {
                self.apply_capability(capability).await
            }
        };
        self.audit_and_notify(actor, &outcome, &ids);
        outcome
    }

    /// Every profile row with its effective status: the settings page's
    /// data source (D26 — the interface is the profile read backwards).
    #[must_use]
    pub fn profile_rows(&self) -> Vec<ProfileRow> {
        self.assembly.rows()
    }

    /// The deployment's plugin-tree audit (dsh `auditRows`): the rows
    /// that are not usable, with why — `failed` never started or was
    /// rejected, `pending` waits for a service the composition does
    /// not supply (and names it).
    #[must_use]
    pub fn profile_audit(&self) -> Vec<crate::composition::kernel::RowAudit> {
        self.assembly.audit()
    }

    /// One profile row's detail.
    #[must_use]
    pub fn profile_row(&self, id: &str) -> Option<ProfileRow> {
        self.profile_rows().into_iter().find(|row| row.id == id)
    }

    pub(crate) async fn apply_row_edit(
        &self,
        store: &ProfileStore,
        edit: RowEdit,
    ) -> ChangeOutcome {
        let fallback = self.assembly.raw();
        let mut document = match store.load_doc(&fallback) {
            Ok(document) => document,
            Err(error) => {
                return ChangeOutcome::failed(format!(
                    "the profile document does not parse: {error}"
                ));
            }
        };
        if let Err(message) = ProfileStore::apply_edit(&mut document, &edit) {
            return ChangeOutcome::failed(message);
        }
        let text = match ProfileStore::to_toml(&document) {
            Ok(text) => text,
            Err(error) => return ChangeOutcome::failed(error),
        };
        if let Err(error) = Profile::from_toml_str(&text) {
            return ChangeOutcome::failed(format!(
                "the edited document does not parse: {error}"
            ));
        }
        let next = match store.resolve_with(&document) {
            Ok(next) => next,
            Err(error) => {
                return ChangeOutcome::failed(format!(
                    "the layer stack does not resolve: {error}"
                ));
            }
        };
        if !edit.is_satisfied_by(&next) {
            return ChangeOutcome::overridden(format!(
                "a higher layer owns `{}` on row `{}`; nothing was written \
                 or mounted",
                field_name(&edit),
                edit.id()
            ));
        }
        let reconciled = match self.apply_resolved_locked(next).await {
            Ok(reconciled) => reconciled,
            Err(error) => {
                return ChangeOutcome::failed(format!(
                    "the change could not be mounted, so nothing changed: \
                     {error}"
                ));
            }
        };
        let mut outcome = if reconciled.mcp_touched {
            ChangeOutcome::restart_required(
                "the `mcp` row changed; remote supervision needs a process \
                 restart",
            )
        } else {
            ChangeOutcome::applied(format!("row `{}` updated", edit.id()))
        }
        .with_diff(reconciled.diff);
        if let Err(error) = store.commit(&text) {
            return ChangeOutcome::failed(format!(
                "the tree changed but `{}` could not be replaced: {error}; an \
                 `hmr = true` deployment re-resolves on its next poll, an \
                 `hmr = false` one keeps the new tree until restart",
                store.doc.display()
            ));
        }
        outcome
            .reasons
            .push(format!("committed `{}`", store.doc.display()));
        // dsh's self-disable write-back: a row that lost its provider
        // can no longer run, so the document records `disabled = true`
        // for it. The write happens after the commit above, so the
        // write-back transaction reads the document this one wrote.
        for id in reconciled.dep_lost {
            let write_back = RowEdit::SetDisabled {
                id: id.clone(),
                disabled: true,
            };
            let mut result =
                Box::pin(self.apply_row_edit(store, write_back)).await;
            outcome.reasons.append(&mut result.reasons);
            outcome.reasons.push(format!(
                "row `{id}` lost its provider; the document write-back \
                 ({:?})",
                result.state
            ));
        }
        outcome
    }

    async fn apply_capability(
        &self,
        action: CapabilityAction,
    ) -> ChangeOutcome {
        match action {
            CapabilityAction::Create { domain, definition } => match domain {
                CapabilityDomain::Agent => {
                    self.create_agent_file(definition).await
                }
                CapabilityDomain::Skill => {
                    self.create_skill_file(definition).await
                }
            },
            CapabilityAction::Delete { domain, id } => match domain {
                CapabilityDomain::Agent => self.delete_agent_file(&id).await,
                CapabilityDomain::Skill => self.delete_skill_file(&id).await,
            },
        }
    }

    async fn create_agent_file(&self, definition: Value) -> ChangeOutcome {
        let descriptor: uva::core::agent::AgentDescriptor =
            match serde_json::from_value(definition) {
                Ok(descriptor) => descriptor,
                Err(error) => {
                    return ChangeOutcome::failed(format!(
                        "the agent definition does not parse: {error}"
                    ));
                }
            };
        if descriptor.name.trim().is_empty()
            || descriptor.description.trim().is_empty()
        {
            return ChangeOutcome::failed(
                "an agent definition needs a name and a description",
            );
        }
        // The name becomes a directory under `.agents/agents/`: a name
        // with a path separator (or an absolute path) would write
        // outside the workspace. The skills path already refuses it.
        if !valid_definition_name(&descriptor.name) {
            return ChangeOutcome::failed(format!(
                "`{}` is not a valid agent name",
                descriptor.name
            ));
        }
        if crate::routes::agents::is_protected(&descriptor.name) {
            return ChangeOutcome::failed(format!(
                "agent `{}` is protected",
                descriptor.name
            ));
        }
        let definition = uva::agents::AgentDefinition::for_descriptor(
            &descriptor,
            &self.workspace_root,
        );
        if let Err(error) = uva::agents::write(&definition) {
            return ChangeOutcome::failed(format!(
                "the agent definition could not be written: {error}"
            ));
        }
        super::super::boot::register_file_agents(self).await;
        ChangeOutcome::applied(format!("created agent `{}`", descriptor.name))
    }

    async fn delete_agent_file(&self, id: &str) -> ChangeOutcome {
        // The id becomes a directory under `.agents/agents/`, and the
        // directory is removed recursively: a traversal would delete
        // whatever it points at.
        if !valid_definition_name(id) {
            return ChangeOutcome::failed(format!(
                "`{id}` is not a valid agent name"
            ));
        }
        if crate::routes::agents::is_protected(id) {
            return ChangeOutcome::failed(format!("agent `{id}` is protected"));
        }
        if let Err(error) = self.agent_registry.delete(id).await
            && !matches!(error, uva::core::Error::NotFound { .. })
        {
            return ChangeOutcome::failed(format!(
                "agent `{id}` could not be unregistered: {error}"
            ));
        }
        let directory = self
            .workspace_root
            .join(uva::core::layout::AGENTS_DIR)
            .join("agents")
            .join(id);
        if directory.join(uva::agents::AGENT_FILE).is_file()
            && let Err(error) = std::fs::remove_dir_all(&directory)
        {
            return ChangeOutcome::failed(format!(
                "agent `{id}` was unregistered but its file remained: {error}"
            ));
        }
        ChangeOutcome::applied(format!("deleted agent `{id}`"))
    }

    async fn create_skill_file(&self, definition: Value) -> ChangeOutcome {
        let Some(name) = definition.get("name").and_then(Value::as_str) else {
            return ChangeOutcome::failed("create requires `name`");
        };
        let Some(content) = definition.get("content").and_then(Value::as_str)
        else {
            return ChangeOutcome::failed("create requires `content`");
        };
        if !valid_definition_name(name) {
            return ChangeOutcome::failed(format!(
                "`{name}` is not a valid definition name"
            ));
        }
        let directory = self
            .workspace_root
            .join(".agents")
            .join("skills")
            .join(name);
        if let Err(error) = std::fs::create_dir_all(&directory) {
            return ChangeOutcome::failed(format!(
                "the skill directory could not be created: {error}"
            ));
        }
        let path = directory.join("SKILL.md");
        let write = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .and_then(|mut file| {
                use std::io::Write as _;
                file.write_all(content.as_bytes())
            });
        if let Err(error) = write {
            let _ = std::fs::remove_dir_all(&directory);
            return ChangeOutcome::failed(format!(
                "skill `{name}` could not be written: {error}"
            ));
        }
        crate::routes::skills::invalidate_skill_cache();
        if let Err(error) = self.remount_row("skills").await {
            let _ = std::fs::remove_dir_all(&directory);
            crate::routes::skills::invalidate_skill_cache();
            return ChangeOutcome::failed(format!(
                "the skills row could not be remounted, so the file was \
                 removed: {error}"
            ));
        }
        ChangeOutcome::applied(format!("created skill `{name}`"))
    }

    async fn delete_skill_file(&self, id: &str) -> ChangeOutcome {
        if !valid_definition_name(id) {
            return ChangeOutcome::failed(format!(
                "`{id}` is not a valid definition name"
            ));
        }
        let directory =
            self.workspace_root.join(".agents").join("skills").join(id);
        if !directory.is_dir() {
            return ChangeOutcome::failed(format!("skill `{id}` not found"));
        }
        let backup = directory
            .with_file_name(format!(".{id}.removed-{}", std::process::id()));
        if let Err(error) = std::fs::rename(&directory, &backup) {
            return ChangeOutcome::failed(format!(
                "skill `{id}` could not be removed: {error}"
            ));
        }
        crate::routes::skills::invalidate_skill_cache();
        if let Err(error) = self.remount_row("skills").await {
            let _ = std::fs::rename(&backup, &directory);
            crate::routes::skills::invalidate_skill_cache();
            return ChangeOutcome::failed(format!(
                "the skills row could not be remounted, so the removal was \
                 rolled back: {error}"
            ));
        }
        let _ = std::fs::remove_dir_all(&backup);
        ChangeOutcome::applied(format!("deleted skill `{id}`"))
    }

    /// Remount one already-enabled row from the current effective
    /// assembly. A capability write changes the files a row builds
    /// from, not its config, so the diff cannot name it.
    async fn remount_row(&self, id: &str) -> Result<(), String> {
        self.assembly
            .remount_row(&self.assembly_context(), id)
            .await
    }

    fn audit_and_notify(
        &self,
        actor: &str,
        outcome: &ChangeOutcome,
        ids: &[String],
    ) {
        tracing::info!(
            target: "uva::profile",
            actor,
            ids = ?ids,
            state = ?outcome.state,
            reasons = ?outcome.reasons,
            "profile change"
        );
        // `profile/changed` means the profile moved. An overridden or
        // failed attempt wrote nothing, so it is audited but not
        // broadcast — a subscriber must never re-read on a no-op.
        if !matches!(
            outcome.state,
            ChangeState::Applied | ChangeState::RestartRequired
        ) {
            return;
        }
        // A write that asked for the value a row already holds
        // changed nothing: the tree is identical, so the broadcast
        // would send every subscriber to re-read for the same rows.
        if outcome.diff.as_ref().is_some_and(|diff| diff.is_empty()) {
            return;
        }
        // The payload is a named type, so the emitter derives the
        // frontend's `ProfileChangedEvent` from the same declaration this
        // broadcast serializes — the notification cannot drift from the
        // type the client subscribes with.
        let event = crate::api::v1::profile::ProfileChangedEvent {
            actor: actor.to_owned(),
            state: outcome.state.into(),
            ids: ids.to_vec(),
            at: Some(self.clock.now()),
        };
        self.rpc.outbound().notify(
            crate::rpc::notify::PROFILE_CHANGED,
            // `ProfileChangedEvent` holds a string, a plain enum, a
            // string list and an optional timestamp: serialization
            // cannot fail, and a `null` payload would tell every
            // subscriber something false. The invariant, stated.
            serde_json::to_value(&event)
                .expect("ProfileChangedEvent is infallibly serializable"),
        );
    }
}

fn affected_ids(action: &ProfileAction) -> Vec<String> {
    match action {
        ProfileAction::Edit(edit) => vec![edit.id().to_owned()],
        ProfileAction::Capability(CapabilityAction::Create {
            definition,
            ..
        }) => definition
            .get("name")
            .and_then(Value::as_str)
            .map(|name| vec![name.to_owned()])
            .unwrap_or_default(),
        ProfileAction::Capability(CapabilityAction::Delete { id, .. }) => {
            vec![id.clone()]
        }
    }
}

fn field_name(edit: &RowEdit) -> &'static str {
    match edit {
        RowEdit::SetDisabled { .. } => "disabled",
        RowEdit::SetConfig { .. } => "config",
        RowEdit::Set {
            disabled: Some(_),
            config: Some(_),
            ..
        } => "disabled/config",
        RowEdit::Set {
            disabled: Some(_), ..
        } => "disabled",
        RowEdit::Set { .. } => "config",
    }
}

/// A definition name: non-empty, no path separators, no traversal.
fn valid_definition_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['/', '\\'])
        && name != "."
        && name != ".."
}
