//! The harness's public docs must state three facts a consumer can
//! check against the code.
//!
//! 1. **What a consumer writes.** A harness-only consumer declares
//!    `uva-harness` + `uva-core` + `uva-provider` +
//!    `uva-tool` (plus `async-trait`), because the harness
//!    re-exports neither `ModelProvider` nor `Tool` nor `Spawner`.
//! 2. **Which names are promised.** The curated facade — the names
//!    re-exported at the crate root — is the promised surface; the
//!    deep paths under `uva_harness::agent::…` / `::events::…` are
//!    not.
//! 3. **The dependency counts.** The numbers the docs quote are the
//!    numbers the code and `cargo tree` print.
//!
//! The counts are derived here from the code (the crate root's `pub
//! use` blocks) and from the consumer fixture's `[dependencies]`
//! table, so a doc that drifts fails this test instead of misleading
//! the next reader.

use std::{
    fs,
    path::{Path, PathBuf},
};

/// The docs that carry the three claims.
const DOCS: &[&str] = &[
    "crates/uva-harness/README.md",
    "crates/uva-harness/src/lib.rs",
    "docs/tutorial/complete-agent.md",
];

/// Keywords that introduce the curated-facade count.
const CURATED_KEYS: &[&str] = &["curated", "精选"];

/// Keywords that introduce the deep-path count.
const DEEP_KEYS: &[&str] = &["deep path", "deep-path", "深路径"];

/// Keywords that introduce the direct-dependency count.
const DEPS_KEYS: &[&str] = &["direct dep", "直接依赖"];

/// The number of public names only a deep path reaches.
///
/// Derived from the crate, once, with the rustdoc JSON the tool
/// itself emits:
///
/// ```text
/// cargo +nightly rustdoc --ignore-rust-version -p uva-harness \
///   --lib -- -Z unstable-options --output-format json
/// ```
///
/// Then over the `paths` map of `target/doc/uva_harness.json`
/// (rustdoc JSON format 57): the entries whose `crate_id` is the
/// crate's own (0) and whose path is `uva_harness::agent::…` or
/// `uva_harness::events::…` with at least one module segment
/// below `agent` / `events` number **28** (modules, items and enum
/// variants). Of those, **17** are the path of one of the crate's
/// 59 facade names — the same name set
/// `scripts/check-frozen-api.sh --print` lists — so **11** names
/// live only behind a deep path.
///
/// Three vocabularies used to live behind those deep paths and moved
/// out. The events vocabulary contributed 33 paths (its five types
/// plus their variants): it moved to `uva-events` wholesale and
/// `events` is now a re-export module, so rustdoc records none of its
/// names under the harness. The interception contract contributed the
/// five hook/gate types: it moved to `uva-hook`, whose names arrive
/// here through `agent::host` and the crate-root use tree. The
/// approval vocabulary (the old `agent::approval` module, plus `Risk`
/// re-exported from `uva-tool` with no path of its own inside the
/// harness — the one cross-crate deep name, which is why the old count
/// carried a `+1`) moved to `uva-approval` too; every approval name
/// now reaches the harness facade through the crate-root use tree. The
/// agent vocabulary contributed four `agent`-level names (`Agent`,
/// `AgentRegistry`, `AgentEntry`, `PromptSection`): it moved to
/// `uva-agent`, and the `agent` module's re-exports carry them. The
/// two-path host surface went away with the same cutover: `Plugin`,
/// `PluginHandle`, `Arbitration` at its old `agent::host` path, the
/// three `DiagnosticKind` variants (`GateFailed`, `TransformerFailed`,
/// `UnsupportedDecision`) and the delegation vocabulary
/// (`DelegatingTool`, `DelegationCall`) no longer have a path inside
/// the harness — eight names, with no replacement.
///
/// The seven mount-point keys used to be the one sanctioned
/// cross-crate deep-only set: `uva_harness::seams` re-exported
/// `uva-hook`'s keys. ADR 0028 folded every interception registration
/// into `uva-hook`'s `MountPoint` enum, the keys went away, and the
/// count dropped to the 11 names that live under `agent` alone.
const DEEP_PATH_NAMES: u64 = 11;

/// The cross-crate deep-only names the docs count on purpose. The
/// mount-point keys that used to sit here are gone with the string
/// keys they spelled (ADR 0028); any cross-crate deep-only name is a
/// regression.
const SANCTIONED_DEEP_CROSS_CRATE: &[&str] = &[];

/// The manifest spelling that keeps a crate's default feature set
/// (and the runtime it carries) out of a consumer's tree.
const NO_DEFAULT_FEATURES: &str = "default-features = false";

/// The crate whose default feature set carries the runtime — the one
/// the "no runtime in your tree" promise is about.
const FEATURE_SET_CARETAKER: &str = "uva-provider";

/// The crates a harness-only consumer declares.
const CONSUMER_CRATES: &[&str] =
    &["uva-harness", "uva-core", "uva-provider", "uva-tool"];

/// The seam traits the harness deliberately does not re-export.
const SEAM_TRAITS: &[&str] = &["ModelProvider", "Tool", "Spawner"];

/// The repository root, or `None` when the crate is packaged on its
/// own — crates.io carries no `docs/` tree to check.
fn repo_root() -> Option<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.parent()?.parent()?.to_path_buf();
    if root.join("docs").is_dir() {
        Some(root)
    } else {
        None
    }
}

/// One doc's text.
fn doc_text(root: &Path, rel: &str) -> String {
    let path = root.join(rel);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} is readable: {e}", path.display()))
}

/// The names the crate root re-exports — the curated facade.
fn curated_names(root: &Path) -> Vec<String> {
    let path = root.join("crates/uva-harness/src/lib.rs");
    let src = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} is readable: {e}", path.display()));
    let mut names = Vec::new();
    let mut rest = src.as_str();
    while let Some(at) = rest.find("pub use ") {
        if !starts_a_line(&rest[..at]) {
            rest = &rest[at + "pub use ".len()..];
            continue;
        }
        let stmt = &rest[at..];
        let end = statement_end(stmt);
        let statement = &stmt[..end];
        if let Some(open) = statement.find('{') {
            let close = statement.rfind('}').unwrap_or(statement.len());
            names.extend(leaf_names(&statement[open + 1..close], ""));
        }
        rest = &stmt[end..];
    }
    names
}

/// Whether `text` ends with the start of a line — a `pub use` inside a
/// doc comment is prose, not a re-export.
fn starts_a_line(text: &str) -> bool {
    let line = match text.rfind('\n') {
        Some(at) => &text[at + 1..],
        None => text,
    };
    line.trim().is_empty()
}

/// The offset of the `;` that ends a `pub use` statement.
fn statement_end(stmt: &str) -> usize {
    let mut depth = 0usize;
    for (i, ch) in stmt.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ';' if depth == 0 => return i,
            _ => {}
        }
    }
    stmt.len()
}

/// The names a `pub use` group re-exports, one per leaf.
fn leaf_names(text: &str, prefix: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in split_top_level(text) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let open = part.find('{');
        let close = part.rfind('}');
        match (open, close) {
            (Some(open), Some(close)) if open < close => {
                let head = part[..open].trim().trim_end_matches(':');
                let next = if prefix.is_empty() {
                    head.to_string()
                } else {
                    format!("{prefix}::{head}")
                };
                out.extend(leaf_names(&part[open + 1..close], &next));
            }
            _ => {
                let leaf = part.split("::").last().unwrap_or(part).trim();
                if !leaf.is_empty() {
                    if prefix.is_empty() {
                        out.push(leaf.to_string());
                    } else {
                        out.push(format!("{prefix}::{leaf}"));
                    }
                }
            }
        }
    }
    out
}

/// Split on the commas that sit outside any brace group.
fn split_top_level(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (i, ch) in text.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

/// The crates the consumer fixture declares.
fn fixture_dependencies(root: &Path) -> Vec<String> {
    fixture_dependency_specs(root)
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

/// The crates the consumer fixture declares, with the manifest
/// spelling each one carries — the flags are part of the claim.
fn fixture_dependency_specs(root: &Path) -> Vec<(String, String)> {
    let path = root.join("docs/examples/harness-only-consumer/Cargo.toml");
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} is readable: {e}", path.display()));
    let mut deps = Vec::new();
    let mut in_deps = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_deps = line == "[dependencies]";
            continue;
        }
        if !in_deps || line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((name, spec)) = line.split_once('=') {
            deps.push((name.trim().to_string(), spec.trim().to_string()));
        }
    }
    deps
}

/// The counts a doc states next to any of `keywords`.
fn stated_counts(text: &str, keywords: &[&str]) -> Vec<u64> {
    let mut out = Vec::new();
    for keyword in keywords {
        out.extend(numbers_after(text, keyword));
    }
    out
}

/// The text of every `pub use` statement in a source file.
fn public_use_statements(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("pub use ") {
        if !starts_a_line(&rest[..at]) {
            rest = &rest[at + "pub use ".len()..];
            continue;
        }
        let stmt = &rest[at..];
        let end = statement_end(stmt);
        let statement = stmt[..end].trim();
        let statement = statement
            .strip_prefix("pub use ")
            .unwrap_or(statement)
            .trim();
        out.push(statement.to_string());
        rest = &stmt[end..];
    }
    out
}

/// The public names the harness re-exports from *other* crates, with
/// the file each one lives in.
///
/// A cross-crate re-export has no path of its own inside the harness:
/// rustdoc records it at its defining path, in the other crate. It is
/// still a public name of this crate — reachable through the module
/// that re-exports it — so the deep-path count has to carry it.
fn cross_crate_reexports(root: &Path) -> Vec<(String, String)> {
    let src_dir = root.join("crates/uva-harness/src");
    let mut out = Vec::new();
    let mut stack = vec![src_dir];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            for statement in public_use_statements(&text) {
                if statement.contains('*') {
                    continue;
                }
                let head = statement.split("::").next().unwrap_or("");
                if !head.starts_with("uva_") || head == "uva_harness" {
                    continue;
                }
                let leaves = match (statement.find('{'), statement.rfind('}')) {
                    (Some(open), Some(close)) if open < close => {
                        split_top_level(&statement[open + 1..close])
                    }
                    _ => vec![statement.as_str()],
                };
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                for leaf in leaves {
                    let leaf = leaf.trim();
                    if leaf.is_empty() {
                        continue;
                    }
                    let name = leaf.split("::").last().unwrap_or(leaf).trim();
                    out.push((rel.clone(), name.to_string()));
                }
            }
        }
    }
    out
}

/// The counts a line states next to `keyword`.
///
/// The count is the number closest to the keyword on that line, so a
/// sentence may put it on either side ("a curated facade of 59
/// names", "7 direct dependencies"). Section references (`§7`),
/// version numbers (`v0.3`) and decimals are not counts and are
/// skipped.
fn numbers_after(text: &str, keyword: &str) -> Vec<u64> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut from = 0usize;
        while let Some(at) = line[from..].find(keyword) {
            let start = from + at;
            let end = start + keyword.len();
            if let Some((count, _)) = closest_count(line, start, end) {
                out.push(count);
            }
            from = end;
        }
    }
    out
}

/// The count on `line` closest to the span `start..end`, and how far
/// away it sits.
fn closest_count(line: &str, start: usize, end: usize) -> Option<(u64, usize)> {
    let chars: Vec<char> = line.chars().collect();
    let mut best: Option<(u64, usize)> = None;
    let mut i = 0usize;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let prev = if i > 0 { chars[i - 1] } else { ' ' };
        let mut j = i;
        while j < chars.len() && chars[j].is_ascii_digit() {
            j += 1;
        }
        let decimal = chars.get(j) == Some(&'.')
            && chars.get(j + 1).is_some_and(|c| c.is_ascii_digit());
        let labelled = matches!(prev, 'v' | 'V' | '§' | '#');
        if !decimal && !labelled {
            let digits: String = chars[i..j].iter().collect();
            if let Ok(n) = digits.parse::<u64>() {
                let distance = if j <= start {
                    start.saturating_sub(j)
                } else if i >= end {
                    i.saturating_sub(end)
                } else {
                    0
                };
                let closer = match best {
                    Some((_, best_distance)) => distance < best_distance,
                    None => true,
                };
                if closer {
                    best = Some((n, distance));
                }
            }
        }
        i = j;
    }
    best
}

#[test]
fn every_doc_names_the_crates_a_consumer_writes() {
    let Some(root) = repo_root() else {
        eprintln!("no repository tree next to the packaged crate — skipped");
        return;
    };
    for rel in DOCS {
        let text = doc_text(&root, rel);
        for name in CONSUMER_CRATES {
            assert!(
                text.contains(name),
                "{rel} must name the crate a consumer writes: {name}"
            );
        }
        assert!(
            text.contains("async-trait"),
            "{rel} must name `async-trait` — the proc macro the seam \
             traits are declared with"
        );
    }
}

#[test]
fn the_harness_re_exports_none_of_the_seam_traits() {
    let Some(root) = repo_root() else {
        eprintln!("no repository tree next to the packaged crate — skipped");
        return;
    };
    let curated = curated_names(&root);
    for trait_name in SEAM_TRAITS {
        let leaked = curated
            .iter()
            .any(|name| name.rsplit("::").next() == Some(trait_name));
        assert!(
            !leaked,
            "the harness must not re-export `{trait_name}` — the traits \
             stay owned by their own crates"
        );
    }
}

#[test]
fn every_doc_states_the_curated_facade_count() {
    let Some(root) = repo_root() else {
        eprintln!("no repository tree next to the packaged crate — skipped");
        return;
    };
    let expected = curated_names(&root).len() as u64;
    for rel in DOCS {
        let text = doc_text(&root, rel);
        let stated = stated_counts(&text, CURATED_KEYS);
        assert!(
            !stated.is_empty(),
            "{rel} must state how many curated names the crate root \
             re-exports"
        );
        for count in stated {
            assert_eq!(
                count, expected,
                "{rel} states a curated count the crate root does not match"
            );
        }
    }
}

#[test]
fn every_doc_states_the_true_deep_path_count() {
    let Some(root) = repo_root() else {
        eprintln!("no repository tree next to the packaged crate — skipped");
        return;
    };
    let mut counts: Vec<Vec<u64>> = Vec::new();
    for rel in DOCS {
        let text = doc_text(&root, rel);
        let stated = stated_counts(&text, DEEP_KEYS);
        assert!(
            !stated.is_empty(),
            "{rel} must state how many names live behind the deep paths"
        );
        for count in &stated {
            assert_eq!(
                count, &DEEP_PATH_NAMES,
                "{rel} states a deep-path count the crate does not match"
            );
        }
        counts.push(stated);
    }
    for (rel, stated) in DOCS.iter().zip(&counts) {
        assert_eq!(
            stated, &counts[0],
            "{rel} disagrees with the other docs about the deep-path count"
        );
    }
}

/// No public name the harness re-exports from another crate is
/// reachable only by a deep path.
///
/// A cross-crate re-export has no path of its own inside the
/// harness: rustdoc records it in the crate that defines it. The
/// approval vocabulary (`agent::approval` and `Risk`) moved to
/// `uva-approval`, and the seven mount-point keys that used to be
/// re-exported inside `uva_harness::seams` are gone with ADR 0028 —
/// `uva-hook`'s `MountPoint` replaces them, so no cross-crate name is
/// sanctioned as deep-only any more. Should any such name appear, it
/// is a regression.
#[test]
fn no_cross_crate_name_is_reachable_only_by_a_deep_path() {
    let Some(root) = repo_root() else {
        eprintln!("no repository tree next to the packaged crate — skipped");
        return;
    };
    let facade: Vec<String> = curated_names(&root)
        .iter()
        .map(|name| name.rsplit("::").next().unwrap_or(name).to_string())
        .collect();
    let mut deep_only: Vec<String> = cross_crate_reexports(&root)
        .into_iter()
        .filter(|(_, name)| !facade.contains(name))
        .map(|(_, name)| name)
        .collect();
    deep_only.sort();
    let sanctioned: Vec<String> = SANCTIONED_DEEP_CROSS_CRATE
        .iter()
        .filter(|key| !facade.iter().any(|name| name == *key))
        .map(|key| (*key).to_string())
        .collect();
    assert_eq!(
        deep_only, sanctioned,
        "cross-crate deep-only names must be exactly the sanctioned \
         mount-point keys the deep-path count carries"
    );
}

/// The docs must state the premise the "no runtime in your tree"
/// promise rests on.
#[test]
fn every_doc_states_the_provider_feature_premise() {
    let Some(root) = repo_root() else {
        eprintln!("no repository tree next to the packaged crate — skipped");
        return;
    };
    for rel in DOCS {
        let text = doc_text(&root, rel);
        assert!(
            text.contains(NO_DEFAULT_FEATURES),
            "{rel} must state the premise: {FEATURE_SET_CARETAKER} is \
             taken with `default-features = false`"
        );
        for carried in ["reqwest", "tokio"] {
            assert!(
                text.contains(carried),
                "{rel} must name what {FEATURE_SET_CARETAKER}'s default \
                 features carry into the consumer's tree: {carried}"
            );
        }
    }
}

/// The fixture is the proof of the premise — its provider line must
/// turn the default features off.
#[test]
fn the_consumer_fixture_takes_the_provider_without_its_default_features() {
    let Some(root) = repo_root() else {
        eprintln!("no repository tree next to the packaged crate — skipped");
        return;
    };
    let specs = fixture_dependency_specs(&root);
    let (_, spec) = specs
        .iter()
        .find(|(name, _)| name == FEATURE_SET_CARETAKER)
        .unwrap_or_else(|| {
            panic!("the fixture declares {FEATURE_SET_CARETAKER}")
        });
    assert!(
        spec.contains(NO_DEFAULT_FEATURES),
        "the fixture must take {FEATURE_SET_CARETAKER} with \
         `default-features = false` — that flag is why its tree carries \
         no runtime"
    );
}

#[test]
fn every_doc_states_the_fixture_dependency_count() {
    let Some(root) = repo_root() else {
        eprintln!("no repository tree next to the packaged crate — skipped");
        return;
    };
    let expected = fixture_dependencies(&root).len() as u64;
    for rel in DOCS {
        let text = doc_text(&root, rel);
        let stated = stated_counts(&text, DEPS_KEYS);
        assert!(
            !stated.is_empty(),
            "{rel} must state how many direct dependencies the consumer \
             fixture declares"
        );
        for count in stated {
            assert_eq!(
                count, expected,
                "{rel} states a dependency count the fixture does not match"
            );
        }
    }
}

#[test]
fn the_consumer_fixture_declares_the_four_crates_and_no_runtime() {
    let Some(root) = repo_root() else {
        eprintln!("no repository tree next to the packaged crate — skipped");
        return;
    };
    let deps = fixture_dependencies(&root);
    for name in CONSUMER_CRATES {
        assert!(
            deps.iter().any(|dep| dep == name),
            "the consumer fixture must declare `{name}`"
        );
    }
    assert!(
        !deps.iter().any(|dep| dep.contains("tokio")),
        "the consumer fixture must not declare a runtime crate — the \
         harness closure is runtime-free"
    );
}
