//! Layer resolution and patch application.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use super::model::{
    EntryPatch,
    Manifest,
    Patch,
    Profile,
    ProfileError,
    ProfileSource,
    Segment,
    children_from_config,
};


/// Resolve an ordered layer stack and an optional CLI overlay.
///
/// The layers are applied in slice order (empty → bundle patch → profile
/// patch → home patch, as the caller supplies them), then `overlay`.
/// A profile layer replaces a row with the same id **whole** — its
/// `config` is never deep-merged — and a patch replaces the fields it
/// provides. `insert` appends, `disabled` filters the final assembly.
///
/// # Errors
///
/// Unknown patch ids, duplicate ids inside one layer, an `insert` that
/// collides with an existing row, and structurally invalid documents
/// all fail.
pub fn resolve(
    layers: &[ProfileSource],
    overlay: Option<Patch>,
) -> Result<Profile, ProfileError> {
    let mut resolved = Profile::default();
    for layer in layers {
        match layer {
            ProfileSource::Profile(profile) => {
                merge_profile_layer(&mut resolved, profile)?;
            }
            ProfileSource::Patch(patch) => {
                apply_patch(&mut resolved, patch)?;
            }
        }
    }
    if let Some(patch) = overlay {
        apply_patch(&mut resolved, &patch)?;
    }
    Ok(resolved)
}

/// Merge one profile document into the assembly.
///
/// A later document replaces a row with the same id whole — at the top
/// level or nested in a group, in place — and adopts its `hmr` flag.
/// Rows the document does not mention keep their position.
fn merge_profile_layer(
    resolved: &mut Profile,
    layer: &Profile,
) -> Result<(), ProfileError> {
    layer.validate_unique()?;
    resolved.hmr = layer.hmr;
    for (rows, incoming) in [
        (&mut resolved.core, &layer.core),
        (&mut resolved.ui, &layer.ui),
    ] {
        for row in incoming {
            if !replace_in_tree(rows, row) {
                rows.push(row.clone());
            }
        }
    }
    Ok(())
}

/// Replace the row with `new`'s id at this level or in any group
/// below, in place. Returns `false` when no such row exists.
fn replace_in_tree(rows: &mut [Manifest], new: &Manifest) -> bool {
    for row in rows.iter_mut() {
        if row.id == new.id {
            *row = new.clone();
            return true;
        }
        if replace_in_tree(&mut row.children, new) {
            return true;
        }
    }
    false
}

/// Apply one patch document.
fn apply_patch(
    profile: &mut Profile,
    patch: &Patch,
) -> Result<(), ProfileError> {
    patch.validate_unique()?;
    for replacement in &patch.replace {
        apply_replacement(profile, replacement)?;
    }
    for insertion in &patch.insert {
        if profile.contains(&insertion.row.id) {
            return Err(ProfileError::DuplicateId {
                id: insertion.row.id.clone(),
            });
        }
        segment_mut(profile, insertion.segment).push(insertion.row.clone());
    }
    disable_rows(profile, &patch.disabled)?;
    Ok(())
}

/// Replace the fields one `[[replace]]` row provides, in place.
///
/// A replacement edits fields; it never moves a row between segments
/// or groups. On a group row, a `config` replacement replaces the
/// child list whole.
fn apply_replacement(
    profile: &mut Profile,
    replacement: &EntryPatch,
) -> Result<(), ProfileError> {
    for row in profile
        .core
        .iter_mut()
        .chain(profile.ui.iter_mut())
    {
        if patch_row(row, replacement)? {
            return Ok(());
        }
    }
    Err(ProfileError::UnknownPatchId {
        id: replacement.id.clone(),
    })
}

/// Apply one replacement to `row` or any descendant. `Ok(true)` — the
/// addressed id was found and patched; `Ok(false)` — not in this
/// subtree; `Err` — the replacement is invalid for the row it
/// addressed (a group's child list that does not parse).
fn patch_row(
    row: &mut Manifest,
    replacement: &EntryPatch,
) -> Result<bool, ProfileError> {
    if row.id == replacement.id {
        if let Some(name) = &replacement.name {
            row.name = name.clone();
        }
        if let Some(config) = &replacement.config {
            if row.group {
                row.children =
                    children_from_config(&row.id, Some(config), "the patch")?;
                row.config = serde_json::Value::Null;
            } else {
                row.config = config.clone();
            }
        }
        if let Some(disabled) = replacement.disabled {
            row.disabled = disabled;
        }
        if let Some(inject) = &replacement.inject {
            row.inject = Some(inject.clone());
        }
        return Ok(true);
    }
    for child in &mut row.children {
        if patch_row(child, replacement)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Remove every id in `disabled`, rejecting an id that matches nothing.
fn disable_rows(
    profile: &mut Profile,
    disabled: &[String],
) -> Result<(), ProfileError> {
    let mut unique = BTreeSet::new();
    for id in disabled {
        if !unique.insert(id.as_str()) {
            continue;
        }
        if !profile.contains(id) {
            return Err(ProfileError::UnknownPatchId { id: id.clone() });
        }
    }
    for id in unique {
        remove_row(profile, id);
    }
    Ok(())
}

fn segment_mut(profile: &mut Profile, segment: Segment) -> &mut Vec<Manifest> {
    match segment {
        Segment::Core => &mut profile.core,
        Segment::Ui => &mut profile.ui,
    }
}

/// Remove the row with `id` anywhere in the tree, top level or nested.
fn remove_row(profile: &mut Profile, id: &str) {
    remove_from_tree(&mut profile.core, id);
    remove_from_tree(&mut profile.ui, id);
}

fn remove_from_tree(rows: &mut Vec<Manifest>, id: &str) -> bool {
    if let Some(index) = rows.iter().position(|row| row.id == id) {
        rows.remove(index);
        return true;
    }
    rows.iter_mut().any(|row| remove_from_tree(&mut row.children, id))
}

/// Resolve the CLI's file layers, in the spec order.
///
/// `bundle`, `home`, and `overlay` are patch documents; `profile` is a
/// profile document. The layers are:
///
/// empty → bundle patch → profile document → home patch → overlay.
///
/// `None` for any optional path skips that layer; a path that exists
/// but does not parse is an error, never a silent ignore.
///
/// # Errors
///
/// Returns the parse/resolution errors of [`resolve`].
pub fn resolve_files(
    bundle: Option<&Path>,
    profile: Option<&Path>,
    home: Option<&Path>,
    overlay: Option<&Path>,
) -> Result<Profile, ProfileError> {
    let mut layers = Vec::new();
    if let Some(path) = bundle {
        layers.push(ProfileSource::Patch(Patch::from_path(path)?));
    }
    let base = match profile {
        Some(path) => Profile::from_path(path)?,
        None => Profile::default(),
    };
    layers.push(ProfileSource::Profile(base));
    if let Some(path) = home {
        layers.push(ProfileSource::Patch(Patch::from_path(path)?));
    }
    let overlay = overlay.map(Patch::from_path).transpose()?;
    resolve(&layers, overlay)
}

/// The project layer's profile document: `<workspace>/.agents/profile.toml`.
///
/// The document layer is the deployment's own profile, and it lives in
/// the project's configuration layer — next to
/// `<workspace>/.agents/config.toml` (the provider fallback) and the
/// seeded skills — not in the workspace root: the settings surface
/// rewrites the document whole, so a root-level file would be a stray
/// artefact in every source checkout that runs a server.
///
/// The global layer keeps its own file at `~/.agents/profile.toml` (a
/// **patch**, see [`discover_home_patch`]); the two layers are
/// distinguished by their root, not by their name.
#[must_use]
pub fn project_document(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".agents").join("profile.toml")
}

/// Environment variable naming an explicit home-level patch file.
pub const HOME_PATCH_ENV: &str = "UVA_HOME_PROFILE";

/// Discover the home-level patch file.
///
/// `UVA_HOME_PROFILE` wins when set; otherwise this checks
/// `$HOME/.agents/profile.toml` (falling back to `%USERPROFILE%` on
/// Windows). A path that does not exist is `None`, so a home-level
/// override is entirely optional.
#[must_use]
pub fn discover_home_patch() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os(HOME_PATCH_ENV) {
        let path = PathBuf::from(explicit);
        return path.exists().then_some(path);
    }
    let home =
        std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let path = PathBuf::from(home).join(".agents").join("profile.toml");
    path.exists().then_some(path)
}
