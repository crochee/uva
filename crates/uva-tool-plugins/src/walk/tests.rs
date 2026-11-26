//! The walk's own tests: the glob dialect and the files it finds.

use std::path::PathBuf;

use super::*;

#[test]
fn a_star_matches_inside_one_segment() {
    assert!(matches_glob("*.rs", "main.rs"));
    assert!(
        matches_glob("*.rs", "src/main.rs"),
        "the name matches at any depth"
    );
    assert!(!matches_glob("*.rs", "main.rs.bak"));
    assert!(
        !matches_glob("*.rs", "main/rs"),
        "a star does not cross a separator"
    );
}

#[test]
fn a_double_star_crosses_segments() {
    assert!(matches_glob("**/*.rs", "main.rs"));
    assert!(matches_glob("**/*.rs", "src/deep/main.rs"));
    assert!(matches_glob("src/**/*.ts", "src/a/b/c.ts"));
    assert!(!matches_glob("src/**/*.ts", "lib/a.ts"));
}

#[test]
fn a_question_mark_matches_one_character() {
    assert!(matches_glob("?.rs", "a.rs"));
    assert!(!matches_glob("?.rs", "ab.rs"));
    assert!(!matches_glob("?.rs", ".rs"));
}

#[test]
fn a_pattern_with_a_separator_matches_the_whole_path() {
    assert!(matches_glob("src/main.rs", "src/main.rs"));
    assert!(!matches_glob("src/main.rs", "lib/main.rs"));
    assert!(matches_glob("src/*.rs", "src/main.rs"));
    assert!(!matches_glob("src/*.rs", "src/deep/main.rs"));
}

#[test]
fn an_empty_pattern_matches_nothing() {
    assert!(!matches_glob("", "main.rs"));
    assert!(!matches_glob("   ", "main.rs"));
}

/// The walk finds the files and skips the VCS metadata.
#[test]
fn the_walk_finds_files_and_skips_git() {
    let dir = tempfile::tempdir().expect("a temp workspace");
    std::fs::create_dir_all(dir.path().join("src")).expect("the src dir");
    std::fs::create_dir_all(dir.path().join(".git")).expect("the git dir");
    std::fs::write(dir.path().join("src/main.rs"), "fn main() {}")
        .expect("the file");
    std::fs::write(dir.path().join(".git/config"), "[core]")
        .expect("the git file");
    std::fs::write(dir.path().join("README.md"), "readme").expect("the readme");

    let walk = files_under(dir.path());
    let mut names: Vec<String> = walk
        .files
        .iter()
        .map(|file| file.relative.clone())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec!["README.md".to_string(), "src/main.rs".to_string()],
        "the walk finds the files and leaves the VCS metadata out"
    );
    assert!(!walk.truncated, "a small tree is not truncated");
}

/// A symlinked directory is not followed: the walk would have to
/// track the visited set to stay acyclic.
#[test]
fn the_walk_does_not_follow_symlinked_directories() {
    let dir = tempfile::tempdir().expect("a temp workspace");
    let other = tempfile::tempdir().expect("another temp workspace");
    std::fs::write(other.path().join("file.txt"), "text").expect("the file");
    std::os::unix::fs::symlink(other.path(), dir.path().join("link"))
        .expect("the link");

    let walk = files_under(dir.path());
    assert!(
        walk.files.is_empty(),
        "the link is not followed: {:?}",
        walk.files
    );
}

/// The paths are relative to the root and use `/` ✓.
#[test]
fn the_paths_are_relative_to_the_root() {
    let dir = tempfile::tempdir().expect("a temp workspace");
    std::fs::create_dir_all(dir.path().join("a/b")).expect("the dirs");
    std::fs::write(dir.path().join("a/b/file.txt"), "text").expect("the file");

    let walk = files_under(dir.path());
    assert_eq!(walk.files.len(), 1);
    assert_eq!(walk.files[0].relative, "a/b/file.txt");
    assert_eq!(
        walk.files[0].path,
        PathBuf::from(dir.path()).join("a/b/file.txt")
    );
}
