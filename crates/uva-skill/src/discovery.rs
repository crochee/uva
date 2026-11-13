//! Skill discovery — walk the workspace + user home for
//! `SKILL.md` files.
//!
//! Mirrors `opencode/packages/opencode/src/skill/index.ts`:
//!
//! 1. **Project skills** at `<workspace>/.agents/skills/**/SKILL.md`
//!    and inside the project's plugin bundles
//!    (`<workspace>/.agents/plugins/<bundle>/skills/`)
//! 2. **Global skills** at `~/.agents/skills/**/SKILL.md`, inside
//!    the global plugin bundles, and at the legacy
//!    `~/.uva/skills/**/SKILL.md`
//!
//! The project roots win over the global roots when two skills
//! share a name (matches the opencode precedence: project overrides
//! personal). The `~/.claude/skills` compatibility root is gone.
//! A user-skill that fails to parse is silently dropped — same
//! leniency opencode / Grok Build apply to broken third-party
//! skills so a malformed file never poisons the whole registry.
//!
//! Discovery is cheap (`read_dir` + an existence check), and
//! is invoked by:
//!
//! - `AppState::new` once at server startup, to seed the
//!   `<available_skills>` prompt block.
//! - `POST /api/v1/skills/reload` on demand.
//!
//! The `Skill` value type is the canonical surface — callers
//! see only `name`, `description`, `location`, `content`.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use uva_core::layout::{self, Domain};

use crate::skill::{SKILL_FILE, Skill};

/// Project-level skills root (relative to the workspace root).
///
/// Matches OpenCode's `<workspace>/.agents/skills/` convention;
/// Uva uses `.agents/` so the directory is shared with other
/// agent definitions (subagents, tasks, MCP declarations).
pub const PROJECT_SKILLS_DIR: &str = ".agents/skills";

/// Discover all skills visible to the given workspace.
///
/// `workspace_root` is the project root the agent runs in. The
/// sources come from [`uva_core::layout`], so the skills obey
/// exactly the precedence the layout defines: the project's own
/// `skills/` directory, the project's plugin bundles, the global
/// `~/.agents/skills/` directory, the global plugin bundles, and —
/// last — the legacy `~/.uva/skills/` directory.
///
/// Returned order: the sources in precedence order, and within a
/// source the `read_dir` order. The caller is free to sort before
/// rendering the `<available_skills>` block.
pub fn discover_skills(workspace_root: &Path) -> Vec<Skill> {
    let mut found = Vec::new();
    for source in layout::sources(workspace_root, Domain::Skills) {
        let paths = walk_skill_dirs(&source.path);
        let label = source_label(&source);
        for skill in collect_skills(paths, label) {
            found.push((skill.name.clone(), skill));
        }
    }
    // The first source that offers a name wins: the layout already
    // ordered the sources by precedence.
    layout::merge_by_name(found, |(name, _)| name.as_str())
        .into_iter()
        .map(|(_, skill)| skill)
        .collect()
}

/// The label a source's skills are collected under, for the
/// malformed-file warning.
fn source_label(source: &layout::SourceDir) -> &'static str {
    match (&source.layer, &source.plugin) {
        (layout::Layer::Project, None) => "project",
        (layout::Layer::Project, Some(_)) => "project plugin",
        (layout::Layer::Global, None) => "global",
        (layout::Layer::Global, Some(_)) => "global plugin",
    }
}

/// Parse a batch of `SKILL.md` paths into [`Skill`] values,
/// dropping malformed files (with a `tracing::warn!`) and
/// intra-batch name duplicates. `source` is a short label
/// ("project" / "user") used only in the warning message.
fn collect_skills(paths: Vec<PathBuf>, source: &'static str) -> Vec<Skill> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<Skill> = Vec::new();
    for path in paths {
        match Skill::from_path(&path) {
            Ok(skill) => {
                if seen.insert(skill.name.clone()) {
                    out.push(skill);
                }
            }
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "skipping malformed SKILL.md during {source} discovery"
                );
            }
        }
    }
    out
}

/// Walk a `skills/` directory recursively, returning every
/// `SKILL.md` it finds.
///
/// Bounded to a depth of 5 (matches grok-build's
/// `MAX_SKILL_WALK_DEPTH`) so a runaway symlink loop cannot
/// pin a server thread.
fn walk_skill_dirs(root: &Path) -> Vec<PathBuf> {
    const MAX_DEPTH: usize = 5;
    let mut out: Vec<PathBuf> = Vec::new();
    if !root.is_dir() {
        return out;
    }
    walk_recursive(root, 0, MAX_DEPTH, &mut out);
    out.sort();
    out
}

fn walk_recursive(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    out: &mut Vec<PathBuf>,
) {
    if depth > max_depth {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let skill_md = path.join(SKILL_FILE);
            if skill_md.is_file() {
                out.push(skill_md);
            }
            walk_recursive(&path, depth + 1, max_depth, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::with_scoped_home;

    fn write_skill(dir: &Path, name: &str, body: &str) -> PathBuf {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        let skill_md = skill_dir.join(SKILL_FILE);
        std::fs::write(&skill_md, body).unwrap();
        skill_md
    }

    /// Discovery MUST walk a project `.agents/skills/` tree
    /// recursively and surface every well-formed SKILL.md —
    /// not just the immediate children.
    #[test]
    fn discovers_nested_project_skills() {
        with_scoped_home(|_home| {
            let dir = tempfile::tempdir().unwrap();
            let skills_root = dir.path().join(PROJECT_SKILLS_DIR);
            write_skill(
                &skills_root,
                "code-review",
                "---\nname: code-review\ndescription: Review.\n---\n\nBody.\n",
            );
            let nested_root = skills_root.join("nested");
            write_skill(
                &nested_root,
                "deeper",
                "---\nname: deeper\ndescription: Deep.\n---\n\nDeep body.\n",
            );

            let skills = discover_skills(dir.path());
            let names: Vec<&str> =
                skills.iter().map(|s| s.name.as_str()).collect();
            assert!(names.contains(&"code-review"), "got: {names:?}");
            assert!(names.contains(&"deeper"), "got: {names:?}");
        });
    }

    /// A malformed SKILL.md (missing `---` delimiters) MUST
    /// NOT poison the whole registry — discovery continues
    /// and the broken file is dropped with a warning.
    #[test]
    fn malformed_skills_are_skipped_not_fatal() {
        with_scoped_home(|_home| {
            let dir = tempfile::tempdir().unwrap();
            let skills_root = dir.path().join(PROJECT_SKILLS_DIR);
            write_skill(
                &skills_root,
                "good",
                "---\nname: good\ndescription: Good.\n---\n\nBody.\n",
            );
            write_skill(&skills_root, "bad", "no frontmatter here at all\n");
            let skills = discover_skills(dir.path());
            let names: Vec<&str> =
                skills.iter().map(|s| s.name.as_str()).collect();
            assert_eq!(names, vec!["good"]);
        });
    }

    /// Discovery MUST terminate on runaway nesting (e.g. a
    /// recursive symlink) rather than stack-overflow. Bound
    /// pinned at 5 levels (matches grok-build) so level
    /// index 5 and beyond are dropped.
    #[test]
    fn depth_bound_is_respected() {
        with_scoped_home(|_home| {
            let dir = tempfile::tempdir().unwrap();
            let mut root = dir.path().join(PROJECT_SKILLS_DIR);
            for level in 0..7 {
                root = root.join(format!("level-{level}"));
                write_skill(
                    &root,
                    &format!("deep-{level}"),
                    &format!(
                        "---\nname: deep-{level}\ndescription: Deep {level}.\n---\n\nBody.\n"
                    ),
                );
            }
            let skills = discover_skills(dir.path());
            let names: Vec<&str> =
                skills.iter().map(|s| s.name.as_str()).collect();
            // levels 0..=4 are reachable (5 levels of nesting
            // inside the skills root); level 5+ are dropped
            // by the depth bound.
            for level in 0..=4 {
                let n = format!("deep-{level}");
                assert!(
                    names.contains(&n.as_str()),
                    "{n} missing from {names:?}"
                );
            }
            assert!(
                !names.contains(&"deep-5"),
                "deep-5 should be past the depth bound; got: {names:?}"
            );
        });
    }

    /// A skill reachable through **both** a global root and a bundle
    /// MUST appear once: each source's walk only dedups within
    /// itself, so the cross-source pass is what keeps the
    /// `<available_skills>` manifest — and anything else indexing
    /// discovery output — free of duplicate rows.
    #[test]
    fn a_skill_visible_through_two_sources_appears_once() {
        with_scoped_home(|home| {
            let dir = tempfile::tempdir().unwrap();
            write_skill(
                &home.join(".agents").join("skills"),
                "shared",
                "---\nname: shared\ndescription: Shared.\n---\n\nBody.\n",
            );
            // The bundle declares the same name.
            write_skill(
                &dir.path().join(".agents/plugins/bundle/skills"),
                "shared",
                "---\nname: shared\ndescription: Shared.\n---\n\nBody.\n",
            );

            let skills = discover_skills(dir.path());
            let shared: Vec<&str> = skills
                .iter()
                .map(|s| s.name.as_str())
                .filter(|n| *n == "shared")
                .collect();
            assert_eq!(
                shared.len(),
                1,
                "a skill declared by two sources must appear once, got: {:?}",
                skills.iter().map(|s| &s.name).collect::<Vec<_>>()
            );
        });
    }

    /// Global skills at `$HOME/.agents/skills/` MUST be
    /// discoverable alongside project skills, and the project layer
    /// MUST win on a name collision.
    ///
    /// The `~/.claude/skills` compatibility root is **gone**: the
    /// global layer is `~/.agents` (canonical) and `~/.uva`
    /// (legacy), and nothing else.
    #[test]
    fn discovers_global_skills_from_home() {
        with_scoped_home(|home| {
            let dir = tempfile::tempdir().unwrap();

            write_skill(
                &home.join(".agents").join("skills"),
                "user-agents",
                "---\nname: user-agents\ndescription: From .agents.\n---\n\nBody.\n",
            );
            // The retired compatibility root: a skill here must NOT
            // be discovered.
            write_skill(
                &home.join(".claude").join("skills"),
                "user-claude",
                "---\nname: user-claude\ndescription: From .claude.\n---\n\nBody.\n",
            );
            write_skill(
                &dir.path().join(PROJECT_SKILLS_DIR),
                "project",
                "---\nname: project\ndescription: Project.\n---\n\nBody.\n",
            );

            let skills = discover_skills(dir.path());
            let names: Vec<&str> =
                skills.iter().map(|s| s.name.as_str()).collect();
            assert!(names.contains(&"user-agents"), "got: {names:?}");
            assert!(names.contains(&"project"), "got: {names:?}");
            assert!(
                !names.contains(&"user-claude"),
                "the .claude compatibility root is gone: {names:?}"
            );
        });
    }
}
