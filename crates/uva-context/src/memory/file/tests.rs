use std::{fs, os::unix::fs::symlink, path::Path, sync::Arc};

use serde_json::json;
use tempfile::TempDir;
use ulid::Ulid;
use uva_provider::Message;

use super::*;
use crate::memory::{Memory, MemoryEntry, MemoryError};

fn temp() -> TempDir {
    tempfile::tempdir().expect("temp dir")
}

fn config(root: &Path, home: &Path) -> FileMemoryConfig {
    FileMemoryConfig::new(root, "tester")
        .with_user_base(home)
        .with_scopes(MemoryScope::ALL.to_vec())
}

fn memory(root: &Path, home: &Path) -> FileMemory {
    FileMemory::new(config(root, home)).expect("file memory")
}

fn name_of(entry: &MemoryEntry) -> String {
    let metadata = entry.metadata.clone().expect("metadata");
    metadata["name"].as_str().expect("name").to_string()
}

// -- scope resolution + precedence -------------------------------

#[test]
fn scope_dirs_follow_the_reference_layout() {
    let root = temp();
    let home = temp();
    let mem = memory(root.path(), home.path());
    assert_eq!(
        mem.dir(MemoryScope::Local),
        Some(root.path().join(".uva/memory-local/tester").as_path())
    );
    assert_eq!(
        mem.dir(MemoryScope::Project),
        Some(root.path().join(".uva/memory/tester").as_path())
    );
    assert_eq!(
        mem.dir(MemoryScope::User),
        Some(home.path().join(".uva/memory/tester").as_path())
    );
    assert_eq!(
        MemoryScope::ALL,
        [MemoryScope::Local, MemoryScope::Project, MemoryScope::User]
    );
    assert_eq!(MemoryScope::parse("project"), Some(MemoryScope::Project));
    assert_eq!(MemoryScope::parse("nope"), None);
    assert_eq!(MemoryKind::parse("feedback"), Some(MemoryKind::Feedback));
    assert_eq!(MemoryKind::parse("nope"), None);
}

#[test]
fn unconfigured_scopes_resolve_to_none() {
    let root = temp();
    let mem = FileMemory::new(
        FileMemoryConfig::new(root.path(), "tester")
            .with_scopes(vec![MemoryScope::Project]),
    )
    .expect("file memory");
    assert_eq!(
        mem.dir(MemoryScope::Project),
        Some(root.path().join(".uva/memory/tester").as_path())
    );
    assert_eq!(mem.dir(MemoryScope::Local), None);
    assert_eq!(mem.dir(MemoryScope::User), None);
}

#[test]
fn agent_names_outside_the_whitelist_are_refused() {
    let root = temp();
    let long = "x".repeat(MAX_AGENT_NAME_LEN + 1);
    let bad = [
        "",
        "..",
        "../etc",
        "a/b",
        "a.b",
        "a b",
        ".hidden",
        long.as_str(),
    ];
    for name in bad {
        let config = FileMemoryConfig::new(root.path(), name)
            .with_scopes(vec![MemoryScope::Project]);
        let error = FileMemory::new(config).expect_err("must refuse the name");
        assert!(
            matches!(error, FileMemoryError::InvalidAgentName(_)),
            "unexpected error for {name:?}: {error}"
        );
    }
    let ok = FileMemoryConfig::new(root.path(), "agent-1_X")
        .with_scopes(vec![MemoryScope::Project]);
    assert!(FileMemory::new(ok).is_ok());
}

#[test]
fn configuration_refusals_are_typed() {
    let root = temp();
    let error = FileMemory::new(
        FileMemoryConfig::new(root.path(), "tester").with_scopes(Vec::new()),
    )
    .expect_err("must refuse an empty scope list");
    assert_eq!(error, FileMemoryError::NoScopes);
}

#[tokio::test]
async fn nearer_scopes_shadow_later_ones() {
    let root = temp();
    let home = temp();
    let id = Ulid::from(7u128).to_string();
    let write_to = |scope: MemoryScope| {
        FileMemory::new(
            config(root.path(), home.path()).with_scopes(vec![scope]),
        )
        .expect("file memory")
    };
    let local = write_to(MemoryScope::Local);
    local
        .store(MemoryEntry::now(id.clone(), "alpha from local"))
        .await
        .expect("store local");
    let project = write_to(MemoryScope::Project);
    project
        .store(MemoryEntry::now(id.clone(), "alpha from project"))
        .await
        .expect("store project");
    // The default scope list is [Local, Project, User]: the
    // nearest copy wins on the id collision.
    let overlayed = memory(root.path(), home.path());
    let hits = overlayed.search("alpha", 10).expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].content, "alpha from local");
    let metadata = hits[0].metadata.clone().expect("metadata");
    assert_eq!(metadata["scope"], "local");
    // A project-only view sees its own copy.
    assert_eq!(
        project.search("alpha", 10).expect("search")[0].content,
        "alpha from project"
    );
}

// -- store / recall ---------------------------------------------

#[test]
fn store_recall_round_trip() {
    let root = temp();
    let home = temp();
    let mem = memory(root.path(), home.path());
    let id = mem
        .store_entry(&FileMemoryEntry {
            name: "Build steps".to_string(),
            description: "How to build the workspace".to_string(),
            kind: MemoryKind::Project,
            content: "Run cargo build.\nThen cargo test.".to_string(),
        })
        .expect("store");
    assert!(Ulid::from_string(&id).is_ok(), "not a ulid: {id}");
    let hits = mem.search("cargo", 10).expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, id);
    assert_eq!(hits[0].content, "Run cargo build.\nThen cargo test.");
    let metadata = hits[0].metadata.clone().expect("metadata");
    assert_eq!(metadata["name"], "Build steps");
    assert_eq!(metadata["description"], "How to build the workspace");
    assert_eq!(metadata["type"], "project");
    assert_eq!(metadata["scope"], "local");
    // A fresh tier over the same directories sees the entry:
    // the memory is on disk, not in the instance.
    let reopened = memory(root.path(), home.path());
    assert_eq!(reopened.search("cargo", 10).expect("search").len(), 1);
}

#[test]
fn recall_ranks_name_above_description_above_body() {
    let root = temp();
    let home = temp();
    let mem = memory(root.path(), home.path());
    let entry =
        |name: &str, description: &str, content: &str| FileMemoryEntry {
            name: name.to_string(),
            description: description.to_string(),
            kind: MemoryKind::Reference,
            content: content.to_string(),
        };
    mem.store_entry(&entry("alpha-notes", "notes", "unrelated body"))
        .expect("store name hit");
    mem.store_entry(&entry("notes-b", "alpha notes", "unrelated body"))
        .expect("store description hit");
    mem.store_entry(&entry("notes-c", "notes", "alpha body"))
        .expect("store body hit");
    mem.store_entry(&entry("notes-d", "notes", "nothing at all"))
        .expect("store miss");

    let hits = mem.search("alpha", 10).expect("search");
    let names: Vec<String> = hits.iter().map(name_of).collect();
    assert_eq!(names, ["alpha-notes", "notes-b", "notes-c"]);
    assert_eq!(mem.search("alpha", 2).expect("search").len(), 2);
    assert!(mem.search("zzz", 10).expect("search").is_empty());
}

#[tokio::test]
async fn score_ties_break_toward_the_newest_id() {
    let root = temp();
    let home = temp();
    let mem = memory(root.path(), home.path());
    let older = Ulid::from(1u128).to_string();
    let newer = Ulid::from(2u128).to_string();
    mem.store(MemoryEntry::now(older.clone(), "alpha older"))
        .await
        .expect("store older");
    mem.store(MemoryEntry::now(newer.clone(), "alpha newer"))
        .await
        .expect("store newer");
    let ids: Vec<String> = mem
        .search("alpha", 10)
        .expect("search")
        .iter()
        .map(|entry| entry.id.clone())
        .collect();
    assert_eq!(ids, [newer, older]);
}

#[test]
fn empty_query_matches_everything_up_to_the_limit() {
    let root = temp();
    let home = temp();
    let mem = memory(root.path(), home.path());
    for index in 0..3 {
        mem.store_entry(&FileMemoryEntry::new(
            format!("note-{index}"),
            format!("body {index}"),
        ))
        .expect("store");
    }
    assert_eq!(mem.search("", 10).expect("search").len(), 3);
    assert_eq!(mem.search("", 2).expect("search").len(), 2);
    assert_eq!(mem.search("", 0).expect("search").len(), 0);
    assert_eq!(mem.search("absent", 10).expect("search").len(), 0);
}

#[tokio::test]
async fn trait_store_maps_metadata_into_frontmatter() {
    let root = temp();
    let home = temp();
    let mem = memory(root.path(), home.path());
    let mut entry = MemoryEntry::now("", "alpha body\nsecond line");
    entry.metadata = Some(json!({
        "name": "From metadata",
        "description": "desc here",
        "type": "feedback",
    }));
    mem.store(entry).await.expect("store");

    let hits = mem.search("alpha", 10).expect("search");
    assert_eq!(hits.len(), 1);
    let metadata = hits[0].metadata.clone().expect("metadata");
    assert_eq!(metadata["name"], "From metadata");
    assert_eq!(metadata["description"], "desc here");
    assert_eq!(metadata["type"], "feedback");
    assert_eq!(hits[0].content, "alpha body\nsecond line");
    let raw = fs::read_to_string(
        mem.dir(MemoryScope::Local)
            .expect("primary dir")
            .join(ENTRIES_DIR)
            .join(format!("{}.md", hits[0].id)),
    )
    .expect("entry file");
    assert!(raw.contains("name: From metadata"), "{raw}");
    assert!(raw.contains("type: feedback"), "{raw}");
}

#[test]
fn entries_without_frontmatter_are_tolerated_and_foreign_names_ignored() {
    let root = temp();
    let home = temp();
    let mem = memory(root.path(), home.path());
    let entries = mem
        .dir(MemoryScope::Local)
        .expect("primary dir")
        .join(ENTRIES_DIR);
    fs::create_dir_all(&entries).expect("entries dir");
    fs::write(
        entries.join("notes.md"),
        "---\nname: hand written\n---\n\nalpha body\n",
    )
    .expect("foreign entry");
    let id = Ulid::from(42u128).to_string();
    fs::write(
        entries.join(format!("{id}.md")),
        "alpha without frontmatter\nmore text\n",
    )
    .expect("bare entry");

    let hits = mem.search("alpha", 10).expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, id);
    assert_eq!(hits[0].content, "alpha without frontmatter\nmore text");
    let metadata = hits[0].metadata.clone().expect("metadata");
    assert_eq!(metadata["name"], "alpha without frontmatter");
    assert_eq!(metadata["type"], "project");
}

// -- index ------------------------------------------------------

#[test]
fn reads_never_create_the_memory_directory() {
    let root = temp();
    let home = temp();
    let mem = memory(root.path(), home.path());
    assert!(mem.search("anything", 5).expect("search").is_empty());
    assert!(mem.index().expect("index").contains("Agent Memory"));
    assert!(mem.prompt_block().expect("block").contains("No memories"));
    assert!(!root.path().join(".uva").exists());
}

#[test]
fn store_refreshes_the_index() {
    let root = temp();
    let home = temp();
    let mem = memory(root.path(), home.path());
    let index_path = mem
        .dir(MemoryScope::Local)
        .expect("primary dir")
        .join(INDEX_FILE);
    assert!(!index_path.exists());
    let first = mem
        .store_entry(&FileMemoryEntry::new("first note", "body one"))
        .expect("store first");
    let index = fs::read_to_string(&index_path).expect("index");
    assert!(index.contains("first note"), "{index}");
    assert!(index.contains(&format!("entries/{first}.md")), "{index}");
    let second = mem
        .store_entry(&FileMemoryEntry::new("second note", "body two"))
        .expect("store second");
    let index = fs::read_to_string(&index_path).expect("index");
    assert!(index.contains(&format!("entries/{first}.md")), "{index}");
    assert!(index.contains(&format!("entries/{second}.md")), "{index}");
    assert_eq!(index.lines().count(), 3);
    assert_eq!(index, mem.index().expect("index"));
}

#[test]
fn index_is_bounded_to_the_line_cap() {
    let root = temp();
    let home = temp();
    let mem = memory(root.path(), home.path());
    for index in 0..(MAX_MEMORY_INDEX_LINES + 5) {
        mem.store_entry(&FileMemoryEntry::new(
            format!("note {index}"),
            format!("body {index}"),
        ))
        .expect("store");
    }
    let index = mem.index().expect("index");
    assert!(
        index.lines().count() <= MAX_MEMORY_INDEX_LINES,
        "index grew to {} lines",
        index.lines().count()
    );
    assert!(index.contains("older entries omitted"), "{index}");
    let disk = fs::read_to_string(
        mem.dir(MemoryScope::Local)
            .expect("primary dir")
            .join(INDEX_FILE),
    )
    .expect("index");
    assert!(disk.lines().count() <= MAX_MEMORY_INDEX_LINES);
    // Every dropped entry is still recallable from disk.
    assert_eq!(
        mem.search("note", MAX_MEMORY_INDEX_LINES + 5)
            .expect("search")
            .len(),
        MAX_MEMORY_INDEX_LINES + 5
    );
}

#[test]
fn prompt_block_carries_scope_directory_and_index() {
    let root = temp();
    let home = temp();
    let mem = memory(root.path(), home.path());
    let id = mem
        .store_entry(&FileMemoryEntry::new("build notes", "alpha body"))
        .expect("store");
    let block = mem.prompt_block().expect("block");
    assert!(block.contains("build notes"), "{block}");
    assert!(block.contains(&format!("entries/{id}.md")), "{block}");
    assert!(block.contains(".uva/memory-local/tester"), "{block}");
    assert!(block.contains("Memory scope: local"), "{block}");
    assert!(!block.contains("read-only"), "{block}");

    let frozen =
        FileMemory::new(config(root.path(), home.path()).with_read_only(true))
            .expect("file memory");
    let block = frozen.prompt_block().expect("block");
    assert!(block.contains("read-only"), "{block}");
    assert!(block.contains("do not create or modify"), "{block}");
}

// -- defenses ---------------------------------------------------

#[tokio::test]
async fn read_only_refuses_writes_and_keeps_recall() {
    let root = temp();
    let home = temp();
    let writable = memory(root.path(), home.path());
    writable
        .store_entry(&FileMemoryEntry::new("note", "alpha body"))
        .expect("store");
    let frozen =
        FileMemory::new(config(root.path(), home.path()).with_read_only(true))
            .expect("file memory");
    assert!(frozen.is_read_only());
    assert_eq!(frozen.search("alpha", 5).expect("search").len(), 1);
    let error = frozen
        .store_entry(&FileMemoryEntry::new("nope", "beta"))
        .expect_err("write must be refused");
    assert!(matches!(error, FileMemoryError::ReadOnly(_)));
    let stored = frozen.store(MemoryEntry::now("", "beta")).await;
    assert!(matches!(
        stored,
        Err(MemoryError::File(FileMemoryError::ReadOnly(_)))
    ));

    // A read-only tier over a fresh root creates nothing.
    let fresh = temp();
    let untouched = FileMemory::new(
        FileMemoryConfig::new(fresh.path(), "tester")
            .with_scopes(vec![MemoryScope::Project])
            .with_read_only(true),
    )
    .expect("file memory");
    assert!(
        untouched
            .store_entry(&FileMemoryEntry::new("nope", "beta"))
            .is_err()
    );
    assert!(!fresh.path().join(".uva").exists());
}

#[test]
fn symlinked_scope_directory_is_refused() {
    let root = temp();
    let home = temp();
    let elsewhere = temp();
    fs::create_dir_all(root.path().join(".uva")).expect("uva dir");
    symlink(elsewhere.path(), root.path().join(".uva/memory-local"))
        .expect("symlink");
    let mem = memory(root.path(), home.path());
    let write = mem
        .store_entry(&FileMemoryEntry::new("note", "alpha body"))
        .expect_err("write must be refused");
    assert!(matches!(write, FileMemoryError::Symlink(_)), "{write}");
    let read = mem.search("alpha", 5).expect_err("read must be refused");
    assert!(matches!(read, FileMemoryError::Symlink(_)), "{read}");
    assert!(
        !elsewhere.path().join("tester").exists(),
        "nothing may be written through the symlink"
    );
}

#[test]
fn symlinked_entry_and_index_files_are_refused() {
    let root = temp();
    let home = temp();
    let mem = memory(root.path(), home.path());
    let id = mem
        .store_entry(&FileMemoryEntry::new("note", "alpha body"))
        .expect("store");
    let scope = mem.dir(MemoryScope::Local).expect("primary dir");
    let outside = root.path().join("outside.md");
    fs::write(&outside, "---\nname: outside\n---\n\nalpha\n")
        .expect("outside file");

    let entry = scope.join(ENTRIES_DIR).join(format!("{id}.md"));
    fs::remove_file(&entry).expect("remove entry");
    symlink(&outside, &entry).expect("symlink entry");
    let read = mem.search("alpha", 5).expect_err("read must be refused");
    assert!(matches!(read, FileMemoryError::Symlink(_)), "{read}");
    fs::remove_file(&entry).expect("remove symlink");

    let index = scope.join(INDEX_FILE);
    fs::remove_file(&index).expect("remove index");
    symlink(&outside, &index).expect("symlink index");
    let write = mem
        .store_entry(&FileMemoryEntry::new("another", "beta"))
        .expect_err("write must be refused");
    assert!(matches!(write, FileMemoryError::Symlink(_)), "{write}");
    assert_eq!(
        fs::read_to_string(&outside).expect("outside intact"),
        "---\nname: outside\n---\n\nalpha\n"
    );
}

// -- delegation --------------------------------------------------

#[tokio::test]
async fn conversation_and_working_tiers_delegate_to_the_inner() {
    let root = temp();
    let home = temp();
    let mem = memory(root.path(), home.path());
    let session = mem.create_session().await.expect("session");
    mem.append(&session, Message::user("hello"))
        .await
        .expect("append");
    assert_eq!(mem.messages(&session).await.expect("messages").len(), 1);
    mem.set_context(&session, "task", json!("ship it"))
        .await
        .expect("set");
    assert_eq!(
        mem.get_context(&session, "task").await.expect("get"),
        Some(json!("ship it"))
    );
    assert_eq!(
        mem.list_sessions().await.expect("list"),
        vec![session.clone()]
    );
    mem.delete_session(&session).await.expect("delete");
    assert!(mem.messages(&session).await.expect("messages").is_empty());
}

#[tokio::test]
async fn supplied_inner_memory_backs_the_volatile_tiers() {
    let root = temp();
    let home = temp();
    let inner: Arc<dyn Memory> = Arc::new(super::super::InMemoryMemory::new());
    let mem = FileMemory::with_inner(config(root.path(), home.path()), inner)
        .expect("file memory");
    mem.append("s1", Message::user("through the inner"))
        .await
        .expect("append");
    assert_eq!(
        mem.messages("s1").await.expect("messages"),
        vec![Message::user("through the inner")]
    );
}
