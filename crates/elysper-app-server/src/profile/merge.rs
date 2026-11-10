//! Layer resolution and patch application.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use super::model::{
    Entry,
    EntryPatch,
    Patch,
    Profile,
    ProfileError,
    ProfileSource,
    Segment,
    segment_of,
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
/// A later document replaces a row with the same id whole and adopts its
/// `hmr` flag. Rows the document does not mention keep their position.
fn merge_profile_layer(
    resolved: &mut Profile,
    layer: &Profile,
) -> Result<(), ProfileError> {
    layer.validate_unique()?;
    resolved.hmr = layer.hmr;
    for entry in layer.core.iter().chain(layer.ui.iter()) {
        replace_row(resolved, entry.clone());
    }
    Ok(())
}

/// Insert or replace one row, preserving its segment position when the
/// segment does not change.
fn replace_row(profile: &mut Profile, entry: Entry) {
    let destination = segment_of(&entry);
    let existing = profile.position(&entry.id);
    let same_segment = matches!(
        existing,
        Some((segment, _)) if segment == destination
    );
    let index = existing.map(|(_, index)| index);
    remove_row(profile, &entry.id);
    let target = segment_mut(profile, destination);
    let at = if same_segment {
        index.unwrap_or(target.len()).min(target.len())
    } else {
        target.len()
    };
    target.insert(at, entry);
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
    for entry in &patch.insert {
        if profile.contains(&entry.id) {
            return Err(ProfileError::DuplicateId {
                id: entry.id.clone(),
            });
        }
        segment_mut(profile, segment_of(entry)).push(entry.clone());
    }
    disable_rows(profile, &patch.disabled)?;
    Ok(())
}

/// Replace the fields one `[[replace]]` row provides.
fn apply_replacement(
    profile: &mut Profile,
    replacement: &EntryPatch,
) -> Result<(), ProfileError> {
    let Some((segment, index)) = profile.position(&replacement.id) else {
        return Err(ProfileError::UnknownPatchId {
            id: replacement.id.clone(),
        });
    };
    let mut entry = remove_row(profile, &replacement.id)
        .expect("the position lookup just found the row");
    if let Some(runtime) = replacement.runtime {
        entry.runtime = runtime;
    }
    if let Some(config) = &replacement.config {
        entry.config = config.clone();
    }
    if let Some(enabled) = &replacement.enabled {
        entry.enabled = enabled.clone();
    }
    let destination = segment_of(&entry);
    let target = segment_mut(profile, destination);
    let at = if destination == segment {
        index.min(target.len())
    } else {
        target.len()
    };
    target.insert(at, entry);
    Ok(())
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

fn segment_mut(profile: &mut Profile, segment: Segment) -> &mut Vec<Entry> {
    match segment {
        Segment::Core => &mut profile.core,
        Segment::Ui => &mut profile.ui,
    }
}

fn remove_row(profile: &mut Profile, id: &str) -> Option<Entry> {
    if let Some(index) = profile.core.iter().position(|entry| entry.id == id) {
        return Some(profile.core.remove(index));
    }
    profile
        .ui
        .iter()
        .position(|entry| entry.id == id)
        .map(|index| profile.ui.remove(index))
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

/// Environment variable naming an explicit home-level patch file.
pub const HOME_PATCH_ENV: &str = "ELYSPER_HOME_PROFILE";

/// Discover the home-level patch file.
///
/// `ELYSPER_HOME_PROFILE` wins when set; otherwise this checks
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
