//! The `task` tool's description has one owner.
//!
//! The seam is the wire: what the model is told about the tool, and
//! what the repository records about it. The crate used to export a
//! tool definition nobody called while the mounted tool carried
//! another variant of the same words — two copies, and nobody knew
//! which one counted. The description is written down once now, in
//! the declaration module, and the mounted tool publishes exactly
//! those words.
//!
//! The recorded copy is read out of the repository's own source here,
//! so "what the model sees" is compared against the sentence a reader
//! of this repository finds — not against a second copy the test
//! keeps alive.

use std::{fs, path::PathBuf, sync::Arc};

use uva_agent::AgentRegistry;
use uva_tool::{ToolEntry, ToolRegistry};
use uva_tool_task::{TASK_TOOL_NAME, TaskTool};

/// The marker the declaration module writes the recorded description
/// behind.
const DESCRIPTION_MARKER: &str = "const DESCRIPTION: &str = \"";

/// The crate root, so the test reads the sources it is about.
fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The source of `path`, relative to the crate root.
fn source(path: &str) -> String {
    let file = crate_root().join(path);
    fs::read_to_string(&file).unwrap_or_else(|error| {
        panic!("{} is readable: {error}", file.display())
    })
}

/// The production source files of the crate, relative to the crate
/// root.
fn production_sources() -> Vec<String> {
    let root = crate_root();
    let mut files = Vec::new();
    collect(&root.join("src"), &root, &mut files);
    files.sort();
    files
}

/// Collect the `.rs` files under `dir`, relative to `root`.
fn collect(
    dir: &std::path::Path,
    root: &std::path::Path,
    files: &mut Vec<String>,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, root, files);
            continue;
        }
        if path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .expect("the path is under the crate root")
            .to_string_lossy()
            .replace('\\', "/");
        if relative.ends_with("/tests.rs") || relative.contains("/tests/") {
            continue;
        }
        files.push(relative);
    }
}

/// The description the repository records: the one copy of the tool's
/// description, read out of the declaration module's source.
fn recorded_description() -> String {
    let source = unfold(&source("src/args.rs"));
    let start = source.find(DESCRIPTION_MARKER).unwrap_or_else(|| {
        panic!(
            "the declaration module records the description behind \
             `{DESCRIPTION_MARKER}`"
        )
    });
    let rest = &source[start + DESCRIPTION_MARKER.len()..];
    let end = rest.find('"').unwrap_or_else(|| {
        panic!("the recorded description is one closed string literal")
    });
    rest[..end].to_string()
}

/// Unfold the line continuations of a Rust string literal: a
/// backslash at the end of a line swallows the newline and the
/// indentation after it.
fn unfold(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(at) = rest.find("\\\n") {
        out.push_str(&rest[..at]);
        rest = rest[at + 2..].trim_start();
    }
    out.push_str(rest);
    out
}

/// The description the model receives: the mounted tool, filed in a
/// registry, described the way the registry describes it to the
/// model.
fn published_description() -> String {
    let tool = TaskTool::new(Arc::new(AgentRegistry::new()));
    let registry = ToolRegistry::new();
    assert!(
        registry.register_core_entry(ToolEntry::new(Arc::new(tool))),
        "the task tool is registered",
    );
    let descriptors = registry.descriptors();
    let task = descriptors
        .iter()
        .find(|descriptor| descriptor.name == TASK_TOOL_NAME)
        .expect("the registry describes the task tool");
    task.description.clone()
}

/// The `pub use` statements of a source: the names a crate publishes.
fn published_statements(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if current.is_empty() {
            if !trimmed.starts_with("pub use ") {
                continue;
            }
            current = trimmed.to_string();
        } else {
            current.push(' ');
            current.push_str(trimmed);
        }
        if current.ends_with(';') {
            out.push(std::mem::take(&mut current));
        }
    }
    out
}

/// The model reads the sentence the repository records — word for
/// word, with no second copy in between.
#[test]
fn the_description_the_model_reads_is_the_one_the_repository_records() {
    assert_eq!(
        published_description(),
        recorded_description(),
        "the model reads the description the repository records",
    );
}

/// The sentence is written down once: the declaration module holds
/// it, and nothing else in the crate repeats it.
///
/// The scan has the two blind spots any text scan has: a copy the
/// compiler assembles — `concat!` over segments, or a string built
/// at runtime — is not on the page and is not found; and a comment
/// that quotes the whole sentence is on the page and is counted,
/// though it publishes nothing. The first is why the wire test
/// compares the published description against the recorded copy
/// itself, not against a count; the second is a false positive the
/// scan is content to report.
#[test]
fn the_description_is_written_down_once() {
    let recorded = recorded_description();
    let mut homes: Vec<(String, usize)> = Vec::new();
    for path in production_sources() {
        let count = unfold(&source(&path)).matches(&recorded).count();
        if count > 0 {
            homes.push((path, count));
        }
    }
    assert_eq!(
        homes,
        vec![("src/args.rs".to_string(), 1)],
        "the description is written down once, in the declaration module",
    );
}

/// The crate publishes no definition of its own: the mounted tool is
/// the one thing that carries a description to the model, so there is
/// no second copy to keep in step with it.
///
/// The first half is a proxy by name: it reads the `pub use`
/// statements of `lib.rs` — the crate's whole public surface, which
/// lists every published name — and asks whether any of them
/// publishes something called a definition. A definition published
/// under a name that does not say "definition" would pass that half;
/// the second half is the one that would catch it, and it has its own
/// limit: it reads a source one line at a time, so a public item
/// whose signature wraps onto the next line is not seen.
#[test]
fn the_crate_publishes_no_definition_of_its_own() {
    for statement in published_statements(&source("src/lib.rs")) {
        assert!(
            !statement.contains("definition"),
            "the crate publishes no tool definition of its own — the \
             mounted tool is the one that carries a description: \
             {statement}",
        );
    }
    for path in production_sources() {
        for line in public_lines(&source(&path)) {
            assert!(
                !line.contains("ToolDefinition"),
                "{path} publishes a tool definition of its own: {line}",
            );
        }
    }
}

/// The lines of a source that open a public item — `pub(crate)` and
/// other restricted forms excluded, because they publish nothing.
fn public_lines(source: &str) -> Vec<String> {
    source
        .lines()
        .filter(|line| line.trim_start().starts_with("pub "))
        .map(str::to_string)
        .collect()
}
