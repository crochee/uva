//! The assembly-boundary evaluator: turn a resolved profile whose rows
//! carry an `enabled` bit into the row set that will actually mount and
//! render.
//!
//! The decision is made **before** `build_plugins` / `mount_all` /
//! `load_ui_tree`, so a disabled row never builds a plugin and a
//! disabled `ui-bundle` row never reaches the frontend schema. Enable
//! flips then reuse the existing presence semantics of
//! [`diff_profiles`](super::diff_profiles): a row filtered out here is
//! `removed` from the next diff, a row filtered in is `added` — the HMR
//! watcher needs no knowledge of the field.
//!
//! # Fail-closed
//!
//! A delegated check whose judge is absent, unknown, or erroring is
//! **unavailable**, and the row stays out. The enablement decision was
//! delegated; when the delegate cannot answer, the conservative reading
//! is "off".

use std::sync::Arc;

use serde_json::Value;

use super::{Enabled, Entry, PluginFactories, Profile};

/// Where the host asks judges.
///
/// Implemented over the mounted kernel context in this crate, over
/// fixtures in tests.
pub trait CheckSource {
    /// Ask one judge.
    fn check(&self, service: &str, method: &str, arg: &Value) -> CheckAnswer;
}

/// One judge's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckAnswer {
    /// The judge says the row assembles.
    Enabled,
    /// The judge says the row stays out.
    Disabled,
    /// No judge answered; the row stays out (fail-closed).
    Unavailable(String),
}

/// One row the effective set dropped, with the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedRow {
    /// The row's id.
    pub id: String,
    /// `"core"` or `"ui"` — which segment the row belongs to.
    pub segment: &'static str,
    /// Why the row stayed out (`disabled`, or the check's answer).
    pub reason: String,
}

/// The row set that mounts / renders, plus every row that did not.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveAssembly {
    /// The profile that will mount / render: `On` rows plus checks
    /// that answered enabled.
    pub profile: Profile,
    /// Off and fail-closed rows, with reasons, for `profile/list`.
    pub skipped: Vec<SkippedRow>,
}

/// Filter one resolved profile to its effective row set.
///
/// `On` keeps the row; `Off` skips it with reason `disabled`;
/// `Check { service, check, arg }` is kept only when the judge answers
/// `Enabled`. `Disabled` and `Unavailable` both skip the row — the
/// latter is the fail-closed rule. Both segments are filtered; a
/// `ui-bundle` row's check is answered by the same core context.
#[must_use]
pub fn effective(
    profile: &Profile,
    checks: &dyn CheckSource,
) -> EffectiveAssembly {
    let mut skipped = Vec::new();
    let core = filter_rows(&profile.core, "core", checks, &mut skipped);
    let ui = filter_rows(&profile.ui, "ui", checks, &mut skipped);
    EffectiveAssembly {
        profile: Profile {
            core,
            ui,
            hmr: profile.hmr,
        },
        skipped,
    }
}

fn filter_rows(
    rows: &[Entry],
    segment: &'static str,
    checks: &dyn CheckSource,
    skipped: &mut Vec<SkippedRow>,
) -> Vec<Entry> {
    let mut kept = Vec::with_capacity(rows.len());
    for entry in rows {
        match &entry.enabled {
            Enabled::On => kept.push(entry.clone()),
            Enabled::Off => skipped.push(SkippedRow {
                id: entry.id.clone(),
                segment,
                reason: "disabled".to_owned(),
            }),
            Enabled::Check(check) => {
                let answer =
                    checks.check(&check.service, &check.check, &check.arg);
                match answer {
                    CheckAnswer::Enabled => kept.push(entry.clone()),
                    CheckAnswer::Disabled => skipped.push(SkippedRow {
                        id: entry.id.clone(),
                        segment,
                        reason: format!(
                            "check `{}`/`{}` answered disabled",
                            check.service, check.check
                        ),
                    }),
                    CheckAnswer::Unavailable(reason) => {
                        skipped.push(SkippedRow {
                            id: entry.id.clone(),
                            segment,
                            reason: format!(
                                "check `{}`/`{}` is unavailable: {reason}",
                                check.service, check.check
                            ),
                        });
                    }
                }
            }
        }
    }
    kept
}

/// Answer enable checks from the currently mounted kernel context.
///
/// A judge is provided as `Arc<dyn EnableChecks>` under its service
/// key — the same shape `memory` uses for `Arc<dyn Memory>` — so the
/// lookup is an ordinary [`Context::get`](uva::kernel::Context::get)
/// of that `Arc`.
pub(crate) struct KernelChecks<'a>(pub &'a uva::kernel::Context);

impl CheckSource for KernelChecks<'_> {
    fn check(&self, service: &str, method: &str, arg: &Value) -> CheckAnswer {
        let judge = match self
            .0
            .get::<Arc<dyn uva::core::enable::EnableChecks>>(service)
        {
            Ok(Some(judge)) => judge,
            Ok(None) => {
                return CheckAnswer::Unavailable(format!(
                    "no enable-check service `{service}` is mounted"
                ));
            }
            Err(error) => {
                return CheckAnswer::Unavailable(format!(
                    "the enable-check service `{service}` could not be \
                     resolved: {error}"
                ));
            }
        };
        match judge.check(method, arg) {
            Some(true) => CheckAnswer::Enabled,
            Some(false) => CheckAnswer::Disabled,
            None => CheckAnswer::Unavailable(format!(
                "judge `{service}` does not answer `{method}`"
            )),
        }
    }
}

/// Split a profile's core rows into the boot's two mount waves.
///
/// Wave 1 mounts first; wave 2 is every row with a delegated check plus
/// every static row that injects a service a wave-2 row provides
/// (transitively). A static dependent of a judge row must mount *after*
/// its provider, and the provider's liveness is only known once the
/// judges are mounted.
///
/// A profile without check rows degenerates to a single wave:
/// `(profile, empty)`.
#[must_use]
pub fn partition_waves(
    profile: &Profile,
    factories: &PluginFactories,
) -> (Profile, Profile) {
    use std::collections::BTreeSet;

    let mut wave2: BTreeSet<String> = profile
        .core
        .iter()
        .filter(|entry| matches!(entry.enabled, Enabled::Check(_)))
        .map(|entry| entry.id.clone())
        .collect();
    if wave2.is_empty() {
        return (profile.clone(), empty_core(profile));
    }
    // Fixed point over the manifest graph: a static row whose `inject`
    // hits a key a wave-2 row provides joins wave 2.
    loop {
        let mut provided: BTreeSet<String> = BTreeSet::new();
        for entry in &profile.core {
            if !wave2.contains(&entry.id) {
                continue;
            }
            if let Some(factory) = factories.get(&entry.id) {
                let manifest = factory.manifest();
                for key in &manifest.provides {
                    provided.insert(key.clone());
                }
            }
        }
        let mut grew = false;
        for entry in &profile.core {
            if wave2.contains(&entry.id) {
                continue;
            }
            let Some(factory) = factories.get(&entry.id) else {
                continue;
            };
            if factory
                .manifest()
                .inject
                .iter()
                .any(|key| provided.contains(key))
            {
                wave2.insert(entry.id.clone());
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    let mut first = empty_core(profile);
    let mut second = empty_core(profile);
    for entry in &profile.core {
        if wave2.contains(&entry.id) {
            second.core.push(entry.clone());
        } else {
            first.core.push(entry.clone());
        }
    }
    (first, second)
}

fn empty_core(profile: &Profile) -> Profile {
    Profile {
        core: Vec::new(),
        ui: Vec::new(),
        hmr: profile.hmr,
    }
}
