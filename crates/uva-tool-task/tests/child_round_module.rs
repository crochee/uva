//! A round of a child session has one owner.
//!
//! The delegation path — the `task` tool's dispatch — used to spell
//! the round out itself: it drained the peer, armed the round's tail,
//! mapped the end reason into the roster's vocabulary and landed the
//! tail. The wake path — the control surface's "another round" — used
//! to spell it out a second time. The round module owns that now, and
//! this test reads the sources to prove the flow has exactly one
//! home: the round's steps are called from one file and from nowhere
//! else.
//!
//! The gate matches **call shapes** (`RoundTail::new(`, …) and not
//! prose: a file that merely mentions a part in a comment is not a
//! second flow. The line comments and the `#[cfg(test)]` modules are
//! stripped before the scan, so neither a comment nor a test can own
//! a round.

use std::{collections::BTreeSet, fs, path::PathBuf};

/// The call shapes that spell out a round: the steps only the round
/// module may take.
const ROUND_FLOW: &[&str] = &[
    "RoundTail::new(",
    "drain_child_observed(",
    "outcome_of(",
    "L1Observer::new",
];

/// The file the round flow lives in.
const ROUND_OWNER: &str = "src/round.rs";

/// The two callers that assemble a round and ask the module to run
/// it.
const ROUND_CALLERS: &[&str] = &["src/tool.rs", "src/control/tools.rs"];

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

/// The production code of `path`: the line comments and the
/// `#[cfg(test)]` modules are gone, so the gate reads code and not
/// prose.
fn code(path: &str) -> String {
    strip_test_blocks(&strip_comments(&source(path)))
}

/// Strip the `//` comments from a source.
fn strip_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| match line.find("//") {
            Some(at) => &line[..at],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Strip the `#[cfg(test)]` modules from a source: a test may use the
/// round's parts without owning a round.
fn strip_test_blocks(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(at) = rest.find("#[cfg(test)]") {
        out.push_str(&rest[..at]);
        let after = &rest[at + "#[cfg(test)]".len()..];
        let line_end = after.find('\n').unwrap_or(after.len());
        let head = after[..line_end].trim();
        if head.ends_with(';') {
            // A file declaration: the module lives in another file.
            rest = &after[line_end..];
            continue;
        }
        // An inline module: the block runs to its closing brace.
        let Some(brace) = after.find('{') else {
            rest = &after[line_end..];
            continue;
        };
        let block = &after[brace..];
        let end = matching_brace(block);
        rest = &block[end..];
    }
    out.push_str(rest);
    out
}

/// The index just past the block that opens at the start of `source`.
fn matching_brace(source: &str) -> usize {
    let mut depth = 0usize;
    for (at, ch) in source.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return at + 1;
                }
            }
            _ => {}
        }
    }
    source.len()
}

/// Whether `code` calls `shape`.
///
/// The shape must appear as a call and not as the definition of it:
/// the module that owns the flow also defines the parts it calls.
fn calls(code: &str, shape: &str) -> bool {
    let mut rest = code;
    while let Some(at) = rest.find(shape) {
        if !rest[..at].ends_with("fn ") {
            return true;
        }
        rest = &rest[at + shape.len()..];
    }
    false
}

/// Whether the file at `path` is one of the crate's test modules.
fn is_test_module(path: &str) -> bool {
    path.ends_with("/tests.rs") || path.contains("/tests/")
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
        if is_test_module(&relative) {
            continue;
        }
        files.push(relative);
    }
}

/// The round's steps are called from one file and from nowhere else:
/// the flow has one home.
#[test]
fn the_round_flow_has_exactly_one_home() {
    let mut owners: BTreeSet<String> = BTreeSet::new();
    for path in production_sources() {
        let code = code(&path);
        if ROUND_FLOW.iter().any(|shape| calls(&code, shape)) {
            owners.insert(path);
        }
    }
    assert_eq!(
        owners,
        BTreeSet::from([ROUND_OWNER.to_string()]),
        "the round's steps are called from one file: {owners:?}"
    );
}

/// Both callers assemble the round's input and hand it to the round
/// module: they own the delegation's decisions, not the round.
#[test]
fn both_callers_ask_the_round_module_to_run_the_round() {
    for caller in ROUND_CALLERS {
        let code = code(caller);
        assert!(
            calls(&code, "RoundInput {"),
            "{caller} assembles the round's input"
        );
    }
}
