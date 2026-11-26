//! Walking the workspace: the one way the search tools see the files.
//!
//! The walk keeps two rules the reference harnesses' discovery tools
//! keep:
//!
//! - **VCS metadata stays out** — `.git` is never entered (dsh's
//!   `GLOB_VCS_EXCLUDES`).
//! - **No cycles** — a symlinked directory is not followed, so a
//!   self-referential link cannot make the walk run forever.
//!
//! The walk is **bounded**: it stops after
//! [`MAX_FILES_VISITED`] files and says so, rather than spending a
//! minute in a dependency tree. A discovery tool that hangs is worse
//! than one that admits it stopped early.

use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

/// The most files one walk visits before it stops.
///
/// The bound is the tool's honesty: the answer says how many files
/// were searched, so a truncated search is never mistaken for a
/// complete one.
pub(crate) const MAX_FILES_VISITED: usize = 20_000;

/// The directories the walk never enters.
const SKIPPED_DIRECTORIES: &[&str] = &[".git"];

/// A file the walk found.
#[derive(Debug)]
pub(crate) struct WalkedFile {
    /// The path as the walk saw it (absolute ✓).
    pub(crate) path: PathBuf,
    /// The path relative to the walk's root — the text the model
    /// sees ✓.
    pub(crate) relative: String,
    /// The file's modification time, when the filesystem knows it ✓.
    pub(crate) modified: Option<SystemTime>,
}

/// What one walk found.
pub(crate) struct Walk {
    /// The files, in the order the walk found them ✓.
    pub(crate) files: Vec<WalkedFile>,
    /// Whether the walk stopped at its bound ✓.
    pub(crate) truncated: bool,
}

/// Walk `root` and collect the files under it.
///
/// The relative paths use `/` on every platform ✓ — the text the model
/// reads is the same everywhere ✓.
pub(crate) fn files_under(root: &Path) -> Walk {
    let mut files = Vec::new();
    let mut truncated = false;
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            // A directory that cannot be read is skipped, not fatal:
            // the rest of the tree is still searchable ✓.
            continue;
        };
        let mut directories = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if file_type.is_dir() {
                if SKIPPED_DIRECTORIES.contains(&name.as_ref()) {
                    continue;
                }
                // A symlinked directory is not followed: the walk
                // would have to track the visited set to stay
                // acyclic, and a link is a place the model did not
                // ask to search.
                if file_type.is_symlink() {
                    continue;
                }
                directories.push(path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            if files.len() >= MAX_FILES_VISITED {
                truncated = true;
                break;
            }
            let relative = relative_to(root, &path);
            let modified = entry
                .metadata()
                .ok()
                .and_then(|metadata| metadata.modified().ok());
            files.push(WalkedFile {
                path,
                relative,
                modified,
            });
        }
        if truncated {
            break;
        }
        // The deepest directories first: a search that hits its bound
        // has then seen the most specific part of the tree.
        for directory in directories.into_iter().rev() {
            stack.push(directory);
        }
    }
    Walk { files, truncated }
}

#[cfg(test)]
mod tests;

/// The path relative to `root`, with `/` separators ✓.
fn relative_to(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<String>>()
        .join("/")
}

/// Whether `relative` matches the glob `pattern`.
///
/// The glob dialect is the one a model already knows from file
/// searches:
///
/// - `*` matches any run of characters inside one path segment ✓;
/// - `?` matches exactly one character ✓;
/// - `**` matches any number of path segments ✓;
/// - everything else is literal ✓.
///
/// A pattern **without** a `/` matches the file's name at any depth ✓
/// (the same rule ripgrep's `--glob` applies ✓), and a pattern with a
/// `/` matches the whole relative path ✓.
pub(crate) fn matches_glob(pattern: &str, relative: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return false;
    }
    if pattern.contains('/') {
        let pattern_parts: Vec<&str> = pattern.split('/').collect();
        let path_parts: Vec<&str> = relative.split('/').collect();
        return match_parts(&pattern_parts, &path_parts);
    }
    // A bare pattern matches the name, at any depth ✓.
    let name = relative.rsplit('/').next().unwrap_or(relative);
    match_segment(pattern, name)
}

/// Whether the path segments match the pattern's segments ✓.
fn match_parts(pattern: &[&str], path: &[&str]) -> bool {
    let Some((first, rest)) = pattern.split_first() else {
        return path.is_empty();
    };
    if *first == "**" {
        // `**` matches any number of segments — including none ✓.
        return (0..=path.len()).any(|skip| match_parts(rest, &path[skip..]));
    }
    let Some((head, tail)) = path.split_first() else {
        return false;
    };
    match_segment(first, head) && match_parts(rest, tail)
}

/// Whether one path segment matches one pattern segment ✓.
fn match_segment(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    // The classic two-pointer matcher with a backtrack point for `*` ✓.
    let (mut p, mut t) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut backtrack: usize = 0;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            backtrack = t;
            p += 1;
        } else if let Some(star) = star {
            // The `*` swallows one more character and retries ✓.
            p = star + 1;
            backtrack += 1;
            t = backtrack;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}
