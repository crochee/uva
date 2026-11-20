//! Skill management handlers.
//!
//! Skills are stored under
//! `<workspace>/.agents/skills/<name>/SKILL.md`.
//!
//! Surface:
//! - `GET    /api/v1/skills`         — list (cursor-paginated)
//! - `GET    /api/v1/skills/{name}`  — detail (frontmatter + body)
//! - `POST   /api/v1/skills`         — create from raw SKILL.md content
//! - `DELETE /api/v1/skills/{name}`  — remove skill directory
//! - `POST   /api/v1/skills/reload`  — rescan the skills directory
//!
//! CRUD coverage was restored in turn 13 of the 2026-08-15
//! optimization pass to address Task 3 ("实现skill.tool、agent、
//! model的全生命周期管理") of the active goal. The per-skill
//! enable toggle was removed alongside the `Settings` subsystem
//! (now deleted); all loaded skills are considered enabled.

use std::{
    cmp::Ordering,
    io::{ErrorKind, Write},
    path::Path as StdPath,
    sync::Arc,
};

use parking_lot::RwLock;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uva::core::Error;

use super::list::{Direction, SortKey};
use crate::{
    api::{List, PageQuery, validate_resource_name},
    rpc::RpcError,
    state::AppState,
};

#[derive(Serialize, JsonSchema)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
}

/// Full detail returned by `GET /api/skills/:id`.
///
/// In addition to the short `description` (used in list rows), the
/// detail payload exposes the raw frontmatter map and the markdown
/// body so the UI can render the SKILL.md as structured metadata
/// plus rendered markdown — no second round-trip needed.
#[derive(Serialize, JsonSchema)]
pub struct SkillDetail {
    pub name: String,
    pub description: String,
    pub path: String,
    /// Parsed YAML/JSON frontmatter of SKILL.md, with `name` /
    /// `description` / `metadata` keys flattened into a flat
    /// `key → value` map for easy table rendering on the client.
    pub frontmatter: serde_json::Map<String, serde_json::Value>,
    /// Markdown body (everything after the frontmatter closer).
    /// Empty string when the file has no body.
    pub body: String,
}

#[derive(Serialize)]
pub struct SkillDeleteResponse {
    pub name: String,
}

/// One cached skill detail: SKILL.md content + parsed
/// frontmatter + extracted description. The detail page renders
/// all three, so the cache collapses three `O(file_size)` parse
/// passes into one per file.
#[derive(Clone)]
struct CachedDetail {
    name: String,
    /// SKILL.md mtime at read time.
    mtime: std::time::SystemTime,
    description: String,
    frontmatter: serde_json::Map<String, serde_json::Value>,
    body: String,
}

/// Process-wide cache of full detail payloads. Frontend detail
/// pages hit this on every mount and when the user clicks a skill
/// card — without it the user re-pays the YAML parse + body extract
/// on every navigation.
static DETAIL_CACHE: std::sync::LazyLock<
    RwLock<std::collections::HashMap<String, CachedDetail>>,
> = std::sync::LazyLock::new(|| {
    parking_lot::const_rwlock(std::collections::HashMap::new())
});

/// Read or refresh the cached full-detail entry for one
/// SKILL.md. Returns `None` only when the file disappeared
/// between `exists()` and `read_to_string` (rare race); the
/// caller treats that as 404 just like a regular missing file.
fn cached_detail_for(skill_md: &StdPath, name: &str) -> Option<CachedDetail> {
    let mtime = std::fs::metadata(skill_md).ok()?.modified().ok()?;
    {
        let cache = DETAIL_CACHE.read();
        if let Some(entry) = cache.get(name)
            && entry.mtime == mtime
        {
            return Some(entry.clone());
        }
    }
    // Slow path: read SKILL.md, parse frontmatter, extract
    // description, capture body — all outside the lock so
    // concurrent `get_skill` calls for *different* skills don't
    // serialise on each other.
    let content = std::fs::read_to_string(skill_md).ok()?;
    let (frontmatter, body) = parse_skill_md(&content);
    // One definition of "the skill's description" for both the list and
    // the detail surface, preferring the authored frontmatter field.
    let description = skill_description(&content);
    let entry = CachedDetail {
        name: name.to_string(),
        mtime,
        description,
        frontmatter,
        body,
    };
    let mut cache = DETAIL_CACHE.write();
    // `HashMap::insert` overwrites in place — no separate
    // `retain + push` needed. The previous `Vec` version did
    // an O(N) scan per upsert; now it's O(1) hash + replace.
    cache.insert(entry.name.clone(), entry.clone());
    Some(entry)
}

/// Invalidate both the list and detail caches. Called after a
/// skill is created / deleted / reloaded so subsequent reads
/// re-scan. `reload_skills` uses this path; for `create_skill`
/// and `delete_skill` only the affected name changes, but a
/// wholesale clear is simpler than partial invalidation and keeps
/// the cache footprint bounded.
pub(crate) fn invalidate_skill_cache() {
    DETAIL_CACHE.write().clear();
}

/// The skill list's sort vocabulary: two keys, declared once.
///
/// `created_at` sorts by the definition file's mtime — a fact the wire
/// row does not carry, which is why the route sorts a
/// `(SkillInfo, SystemTime)` pair and projects the row afterwards. A
/// filesystem that cannot report an mtime reports `UNIX_EPOCH`, so the
/// skill sorts oldest rather than vanishing from the order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkillSort {
    Name,
    CreatedAt,
}

impl SortKey<(SkillInfo, std::time::SystemTime)> for SkillSort {
    const ALL: &'static [Self] = &[Self::Name, Self::CreatedAt];

    fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::CreatedAt => "created_at",
        }
    }

    fn compare(
        self,
        a: &(SkillInfo, std::time::SystemTime),
        b: &(SkillInfo, std::time::SystemTime),
    ) -> Ordering {
        match self {
            Self::Name => a.0.name.cmp(&b.0.name),
            Self::CreatedAt => a.1.cmp(&b.1),
        }
    }

    fn default_sort() -> (Self, Direction) {
        (Self::Name, Direction::Ascending)
    }
}

/// GET /api/v1/skills - List all skills with cursor pagination.
pub async fn list_skills(
    state: Arc<AppState>,
    page: PageQuery,
) -> Result<List<SkillInfo>, RpcError> {
    // The list is the four-source layout's answer, not one
    // directory's: project direct, project bundles, global direct,
    // global bundles, in precedence order (`discover_skills` merges
    // by name, so a project copy shadows the global one). The walk
    // + parse block the worker thread: a slow filesystem must not
    // stall the reactor.
    let workspace_root = state.workspace_root.clone();
    let skills = tokio::task::spawn_blocking(move || {
        let mut skills: Vec<(SkillInfo, std::time::SystemTime)> = Vec::new();
        for skill in uva::skill::discover_skills(&workspace_root) {
            // The sort key is the definition file's mtime; a
            // filesystem that cannot report one sorts it oldest.
            let mtime = std::fs::metadata(&skill.location)
                .and_then(|meta| meta.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            let description = skill.effective_description();
            skills.push((
                SkillInfo {
                    name: skill.name,
                    description,
                },
                mtime,
            ));
        }
        skills
    })
    .await
    .map_err(|e| {
        RpcError::from(Error::internal(format!("skill scan task failed: {e}")))
    })?;

    // Cut the page over the pair (the sort key is not on the wire
    // row), then project it: the cursor names the row by name, which
    // the projection keeps.
    Ok(SkillSort::sort_and_page(skills, &page, |(skill, _)| {
        skill.name.as_str()
    })?
    .map(|(skill, _)| skill))
}

/// Extract the SKILL.md content as `(frontmatter_json_map, body)`.
/// On any parse error (missing delimiters, malformed YAML/JSON),
/// falls back to `(empty_map, full_content)` so the UI still
/// receives something useful — a markdown body without
/// frontmatter is still a valid SKILL.md surface.
fn parse_skill_md(
    content: &str,
) -> (serde_json::Map<String, serde_json::Value>, String) {
    let parts: Vec<&str> = content.splitn(3, "---").collect();
    if parts.len() < 3 {
        return (serde_json::Map::new(), content.to_string());
    }
    let raw_fm = parts[1].trim();
    let body = parts[2].trim_start_matches('\n').to_string();
    let value = serde_yaml::from_str::<serde_json::Value>(raw_fm)
        .or_else(|_| serde_json::from_str::<serde_json::Value>(raw_fm));
    let map = match value {
        Ok(serde_json::Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    (map, body)
}

/// The skill's one-line description.
///
/// The authored `description:` in the YAML frontmatter wins. It is the
/// field the model reads when deciding whether to invoke the skill — see
/// `uva::skill::Skill::effective_description`, which the prompt
/// manifest and the `skill` tool both use — so the API must report the
/// same text. Deriving it from the body instead made the Skills page
/// disagree with what the model was told about the very same skill.
///
/// A skill with no frontmatter, or one whose `description:` is missing or
/// blank, falls back to the body's first non-empty line, matching the
/// Anthropic / OpenCode convention for an implicit description.
pub(crate) fn skill_description(content: &str) -> String {
    let (frontmatter, body) = parse_skill_md(content);
    if let Some(desc) = frontmatter
        .get("description")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return desc.to_string();
    }
    let source = if body.is_empty() { content } else { &body };
    source
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "Skill directory".to_string())
}

/// GET /api/v1/skills/{name} - Get a single skill.
pub async fn get_skill(
    state: Arc<AppState>,
    name: String,
) -> Result<SkillDetail, RpcError> {
    validate_resource_name(&name)?;
    // The winning definition, wherever the layout found it: a
    // project copy shadows a global one, a bundle's skill is a
    // skill like any other.
    let workspace_root = state.workspace_root.clone();
    let wanted = name.clone();
    let found = tokio::task::spawn_blocking(move || {
        uva::skill::discover_skills(&workspace_root)
            .into_iter()
            .find(|skill| skill.name == wanted)
    })
    .await
    .map_err(|e| {
        RpcError::from(Error::internal(format!("skill scan task failed: {e}")))
    })?;
    let Some(skill) = found else {
        return Err(RpcError::from(Error::not_found(format!(
            "skill '{name}'"
        ))));
    };
    let skill_path = skill.location;

    // The detail endpoint benefits from the same mtime-based
    // caching as the list endpoint: SKILL.md is read once and
    // frontmatter + body stay cached until either the file is
    // edited (mtime bump → re-read) or `create_skill` /
    // `delete_skill` / `reload_skills` clears the cache. Without
    // this cache, every detail-page mount re-reads the file and
    // re-parses the YAML frontmatter.
    let cached = cached_detail_for(&skill_path, &name).ok_or_else(|| {
        RpcError::from(Error::not_found(format!("skill '{name}'")))
    })?;
    Ok(SkillDetail {
        name: cached.name,
        description: cached.description,
        path: skill_path.to_string_lossy().to_string(),
        frontmatter: cached.frontmatter,
        body: cached.body,
    })
}

/// Request body for `POST /api/v1/skills` (create).
#[derive(Deserialize, validator::Validate)]
pub struct CreateSkillRequest {
    #[validate(length(min = 1, message = "must not be empty"))]
    pub name: String,
    /// SKILL.md body content (full file, with frontmatter).
    #[validate(length(min = 1, message = "must not be empty"))]
    pub content: String,
}

/// Response from `POST /api/v1/skills`.
#[derive(Serialize, JsonSchema)]
pub struct CreateSkillResponse {
    pub name: String,
    pub path: String,
}

/// POST /api/v1/skills - Create a new skill from raw SKILL.md content.
///
/// Writes the content to
/// `<workspace>/.agents/skills/<name>/SKILL.md`. The directory is
/// created if missing. Returns `409 Conflict` if the skill
/// already exists. The request body is the raw SKILL.md content
/// (frontmatter + markdown body), matching the on-disk format.
pub async fn create_skill(
    state: Arc<AppState>,
    req: CreateSkillRequest,
) -> Result<CreateSkillResponse, RpcError> {
    validate_resource_name(&req.name)?;
    let skill_dir = state
        .workspace_root
        .join(".agents")
        .join("skills")
        .join(&req.name);
    let skill_path = skill_dir.join("SKILL.md");

    // Skip the previous `skill_path.exists()` pre-check and rely
    // on `OpenOptions::create_new` (O_EXCL on POSIX) to atomically
    // refuse the write if the file already exists. That collapses
    // two syscalls (`exists()` + `write()`) into one syscall on
    // both the conflict path and the happy path — and is race-free
    // in a way the previous check-then-write was not.
    let dir = skill_dir.clone();
    let path = skill_path.clone();
    let content = req.content.clone();
    tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&dir)?;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        f.write_all(content.as_bytes())
    })
    .await
    .map_err(|e| {
        RpcError::from(Error::internal(format!("skill write task failed: {e}")))
    })?
    .map_err(|e| {
        if e.kind() == ErrorKind::AlreadyExists {
            RpcError::from(Error::already_exists(format!(
                "skill '{}'",
                req.name
            )))
        } else if e.kind() == ErrorKind::NotFound {
            RpcError::from(Error::internal(format!(
                "failed to create skill directory: {e}"
            )))
        } else {
            RpcError::from(Error::internal(format!(
                "failed to write SKILL.md: {e}"
            )))
        }
    })?;
    match Ok::<(), std::io::Error>(()) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            return Err(RpcError::from(Error::already_exists(format!(
                "skill '{}'",
                req.name
            ))));
        }
        Err(e) => {
            return Err(RpcError::from(Error::internal(format!(
                "failed to write SKILL.md: {e}"
            ))));
        }
    }
    // Drop any stale cache entry for this skill — the new SKILL.md
    // has a strictly-greater mtime so the next read would refresh
    // anyway, but clearing avoids the stat() round trip and also
    // drops any orphan entry for a previous failed create.
    invalidate_skill_cache();

    Ok(CreateSkillResponse {
        name: req.name,
        path: skill_path.to_string_lossy().to_string(),
    })
}

/// DELETE /api/v1/skills/{name} - Remove a skill.
///
/// Deletes `<workspace>/.agents/skills/<name>/` recursively.
/// Returns `404 Not Found` if the skill does not exist.
pub async fn delete_skill(
    state: Arc<AppState>,
    name: String,
) -> Result<SkillDeleteResponse, RpcError> {
    validate_resource_name(&name)?;
    let project_root = state.workspace_root.join(".agents").join("skills");
    let skill_dir = project_root.join(&name);
    // The writable layer is the project's own `skills/`. A name that
    // resolves to a global or a bundle definition cannot be deleted
    // from here: its file is not this deployment's to remove, and
    // removing a shadowing project copy instead of the one the
    // caller saw would be a silent no-op on what they asked for.
    let found_elsewhere = {
        let workspace_root = state.workspace_root.clone();
        let wanted = name.clone();
        tokio::task::spawn_blocking(move || {
            uva::skill::discover_skills(&workspace_root)
                .into_iter()
                .find(|skill| skill.name == wanted)
                .map(|skill| skill.location)
        })
        .await
        .ok()
        .flatten()
    };
    if let Some(location) = found_elsewhere
        && !location.starts_with(&project_root)
    {
        return Err(RpcError::from(Error::forbidden(format!(
            "skill '{name}' is defined at {} (a global or bundle layer); \
             edit that file instead",
            location.display()
        ))));
    }

    // Skip the previous `skill_dir.exists()` pre-check and let
    // `remove_dir_all` surface the NotFound error directly. That
    // collapses two syscalls (`exists()` + `remove_dir_all()`)
    // into one syscall, and maps the kernel's NotFound straight
    // to our 404 — no race window where a concurrent delete
    // could pass the pre-check and then trip the actual op.
    let dir = skill_dir.clone();
    let removed =
        tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&dir))
            .await
            .map_err(|e| {
                RpcError::from(Error::internal(format!(
                    "skill delete task failed: {e}"
                )))
            })?;
    match removed {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {
            return Err(RpcError::from(Error::not_found(format!(
                "skill '{name}'"
            ))));
        }
        Err(e) => {
            return Err(RpcError::from(Error::internal(format!(
                "failed to remove skill directory: {e}"
            ))));
        }
    }
    // The cache is keyed by file presence + mtime; with the
    // directory gone, `cached_list_entry_for` would simply not
    // find it on next read — but invalidating here keeps the
    // process memory footprint bounded on long-running servers
    // that see frequent delete churn.
    invalidate_skill_cache();

    Ok(SkillDeleteResponse { name })
}

/// Response from `POST /api/v1/skills/reload`.
#[derive(Serialize, JsonSchema)]
pub struct ReloadSkillsResponse {
    /// Number of skills found in the workspace after reload.
    pub count: usize,
}

/// POST /api/v1/skills/reload - Rescan the skills directory.
///
/// Re-reads `<workspace>/.agents/skills/` from disk so newly
/// added or deleted SKILL.md files are picked up. Returns the
/// count of skill directories found after the rescan so the
/// caller can detect drift between the on-disk state and what
/// the previous list response showed.
pub async fn reload_skills(
    state: Arc<AppState>,
) -> Result<ReloadSkillsResponse, RpcError> {
    let workspace_root = state.workspace_root.clone();
    let count = tokio::task::spawn_blocking(move || {
        uva::skill::discover_skills(&workspace_root).len()
    })
    .await
    .unwrap_or(0);
    // Explicit reload means the caller wants the next read to
    // re-scan disk; mtime-based invalidation would also fire
    // eventually but the user explicitly asked for a refresh.
    invalidate_skill_cache();
    Ok(ReloadSkillsResponse { count })
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::*;

    fn skill(name: &str, mtime: SystemTime) -> (SkillInfo, SystemTime) {
        (
            SkillInfo {
                name: name.to_string(),
                description: String::new(),
            },
            mtime,
        )
    }

    fn page(sort: Option<&str>) -> PageQuery {
        match sort {
            Some(raw) => PageQuery::new().with_sort(raw),
            None => PageQuery::new(),
        }
    }

    /// The route's observable answer for one page: the names, in order.
    /// Nothing here touches the filesystem — the pair's mtime is the
    /// fixture's.
    fn sorted(
        skills: Vec<(SkillInfo, SystemTime)>,
        sort: Option<&str>,
    ) -> Vec<String> {
        SkillSort::sort_and_page(skills, &page(sort), |(skill, _)| {
            skill.name.as_str()
        })
        .expect("the list")
        .data
        .into_iter()
        .map(|(skill, _)| skill.name)
        .collect()
    }

    fn three() -> Vec<(SkillInfo, SystemTime)> {
        vec![
            skill("zelda", UNIX_EPOCH + Duration::from_secs(30)),
            skill("alpha", UNIX_EPOCH + Duration::from_secs(10)),
            skill("mango", UNIX_EPOCH + Duration::from_secs(20)),
        ]
    }

    #[test]
    fn name_sorts_both_ways() {
        assert_eq!(sorted(three(), Some("name")), ["alpha", "mango", "zelda"]);
        assert_eq!(sorted(three(), Some("-name")), ["zelda", "mango", "alpha"]);
    }

    #[test]
    fn created_at_sorts_by_mtime_both_ways() {
        // alpha is the oldest definition file, zelda the newest.
        assert_eq!(
            sorted(three(), Some("created_at")),
            ["alpha", "mango", "zelda"]
        );
        assert_eq!(
            sorted(three(), Some("-created_at")),
            ["zelda", "mango", "alpha"]
        );
    }

    #[test]
    fn the_default_key_is_name_ascending() {
        assert_eq!(sorted(three(), None), ["alpha", "mango", "zelda"]);
    }

    #[test]
    fn name_and_created_at_can_differ() {
        // Names and mtimes are inversely related, so a key that
        // silently sorted by the wrong field would be visible.
        let make = || {
            vec![
                skill("c", UNIX_EPOCH + Duration::from_secs(10)),
                skill("b", UNIX_EPOCH + Duration::from_secs(20)),
                skill("a", UNIX_EPOCH + Duration::from_secs(30)),
            ]
        };
        assert_eq!(sorted(make(), Some("name")), ["a", "b", "c"]);
        assert_eq!(sorted(make(), Some("created_at")), ["c", "b", "a"]);
    }

    #[test]
    fn an_unknown_key_is_refused() {
        // `mtime` names the fact the key is derived from, not the key.
        let err = SkillSort::sort_and_page(
            three(),
            &page(Some("mtime")),
            |(skill, _)| skill.name.as_str(),
        )
        .err()
        .expect("`mtime` names the fact, not the key");
        assert!(matches!(err, Error::InvalidItem { .. }), "got {err:?}");
    }

    #[test]
    fn an_empty_input_is_an_empty_page() {
        assert!(sorted(Vec::new(), Some("name")).is_empty());
    }

    #[test]
    fn parse_skill_md_extracts_frontmatter_and_body() {
        let content = "---\nname: foo\ndescription: A foo skill\nmetadata:\n  category: tooling\n---\n# Foo\n\nBody paragraph.\n";
        let (fm, body) = parse_skill_md(content);
        assert_eq!(fm.get("name").unwrap(), &serde_json::json!("foo"));
        assert_eq!(
            fm.get("description").unwrap(),
            &serde_json::json!("A foo skill")
        );
        assert_eq!(
            fm.get("metadata").and_then(|v| v.get("category")).unwrap(),
            &serde_json::json!("tooling")
        );
        assert!(body.starts_with("# Foo"));
    }

    #[test]
    fn parse_skill_md_missing_delimiters_returns_full_content_as_body() {
        let content = "# Just a heading\n\nNo frontmatter here.\n";
        let (fm, body) = parse_skill_md(content);
        assert!(fm.is_empty());
        assert_eq!(body, content);
    }

    /// The authored frontmatter `description:` wins over the body's
    /// first line — the same field `Skill::effective_description`
    /// surfaces to the model, so the API and the prompt never disagree.
    #[test]
    fn skill_description_prefers_frontmatter() {
        let content = "---\nname: foo\ndescription: A foo skill\n---\n\n# Heading\n\nbody\n";
        assert_eq!(skill_description(content), "A foo skill");
    }

    /// With no `description:` in the frontmatter, the body's first
    /// non-empty line is the implicit description (Anthropic / OpenCode
    /// convention).
    #[test]
    fn skill_description_falls_back_to_the_body() {
        let content = "---\nname: foo\n---\n\n# Heading\n\nbody\n";
        assert_eq!(skill_description(content), "# Heading");
    }

    /// A blank `description:` counts as absent rather than yielding an
    /// empty string.
    #[test]
    fn skill_description_treats_a_blank_frontmatter_value_as_absent() {
        let content = "---\nname: foo\ndescription: \"\"\n---\n\n# Heading\n";
        assert_eq!(skill_description(content), "# Heading");
    }

    /// A frontmatter-only file (empty body, no `description:`) still
    /// yields a non-empty description rather than an empty string.
    #[test]
    fn skill_description_falls_back_when_body_is_empty() {
        let content = "---\nname: foo\n---\n";
        assert!(!skill_description(content).is_empty());
    }
}
