//! Build-time embedding of the uva-app-server build identity.
//!
//! Reads three environment variables the release pipeline is
//! expected to set (`UVA_GIT_SHA`, `UVA_BUILD_TIME`,
//! `UVA_TARGET`) and emits them as `cargo:rustc-env=` keys
//! that `build_info.rs` reads back via `env!()`. The contract is
//! the same one `release.yml`'s "Verify baked identity" step and
//! `Dockerfile.server`'s build args implement; this file is the
//! glue that turns CI-provided values into compile-time constants.
//!
//! It also bakes a pre-computed `UVA_LONG_VERSION` string in the
//! exact format `build_info::long_version()` produces at the `HTTP`
//! `/version` route. The static-string path lets `clap`'s `version`
//! attribute (which only accepts `&'static str`) emit the same rich
//! line that `GET /version` returns, so an operator checking the
//! binary with `--version` and an orchestrator checking the running
//! process with `curl /version` see identical identity. Without this,
//! `clap` only sees the cargo package version and `--version`
//! prints the bare `uva-app-server 0.1.0`.
//!
//! Source priority for each value (env wins over fallback because
//! the Docker build context typically excludes `.git`, so the CI
//! pipeline is the only reliable source in container builds):
//!
//!   1. Explicit env (`UVA_GIT_SHA` etc.) — set by release.yml
//!      and Dockerfile.server's build args.
//!   2. `git` CLI fallback — used by `cargo build` from a developer
//!      checkout where `.git/` is present.
//!   3. Silently missing — `env!()` in `build_info.rs` is replaced
//!      with `option_env!()` so a missing value yields the
//!      documented `"unknown"` sentinel rather than a compile error.

use std::process::Command;

/// Emit one `cargo:rustc-env=` line for each identity value that
/// resolved to a non-empty string. Empty / missing values are
/// silently skipped so that `build_info.rs`'s `option_env!`
/// fallback fires at compile time.
fn main() {
    // Priority: explicit env wins over `git` fallback. The CI
    // pipeline (release.yml, Dockerfile.server) sets these from the
    // build context; `git` is only a convenience for local `cargo
    // build` from a clone. Without this ordering, a developer build
    // on a clean checkout would always emit the on-disk SHA even
    // when CI explicitly passed a different value (e.g. a SHA
    // cherry-picked for a backport build).
    emit_env(
        "UVA_GIT_SHA",
        std::env::var("UVA_GIT_SHA").ok().or_else(|| git_field(0)),
    );
    emit_env("UVA_BUILD_TIME", std::env::var("UVA_BUILD_TIME").ok());
    // UVA_TARGET: prefer the explicit env (full triple, set by
    // Dockerfile.server / release.yml), fall back to the OS component
    // Cargo provides via CARGO_CFG_TARGET_OS so a local `cargo build`
    // still emits a usable value (e.g. `linux`).
    let target = std::env::var("UVA_TARGET")
        .ok()
        .filter(|t| !t.is_empty())
        .or_else(|| std::env::var("CARGO_CFG_TARGET_OS").ok())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=UVA_TARGET={target}");

    // UVA_PROFILE: prefer explicit env (release.yml sets it), fall
    // back to CARGO_CFG_PROFILE (= `release` for `--release`, else
    // `debug`). Without this, every `cargo run` from a developer
    // checkout advertises the binary as `release` even when compiled
    // with the default (debug) profile.
    let profile = std::env::var("UVA_PROFILE")
        .ok()
        .filter(|p| !p.is_empty())
        .or_else(|| std::env::var("CARGO_CFG_PROFILE").ok())
        .unwrap_or_else(|| "release".to_string());
    println!("cargo:rustc-env=UVA_PROFILE={profile}");

    // UVA_LONG_VERSION: the exact `build_info::long_version()`
    // shape, baked as a `&'static str` so `clap` can consume it from
    // its `version = …` attribute. Computed here from the same
    // resolved values that go into UVA_GIT_SHA /
    // UVA_BUILD_TIME / UVA_TARGET / UVA_PROFILE so the
    // baked string stays in lock-step with the runtime path: the
    // /version HTTP route and the `--version` CLI flag can never
    // disagree.
    let sha = std::env::var("UVA_GIT_SHA")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| git_field(0))
        .unwrap_or_else(|| "unknown".to_string());
    let short_sha = if sha.len() >= 7 {
        &sha[..7]
    } else {
        sha.as_str()
    };
    let build_time = std::env::var("UVA_BUILD_TIME")
        .ok()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    // Package version from CARGO_PKG_VERSION (always set by Cargo for
    // the crate whose build.rs is running — i.e. uva-app-server).
    let pkg_version = std::env::var("CARGO_PKG_VERSION")
        .unwrap_or_else(|_| "unknown".to_string());
    let long_version =
        format!("{pkg_version} ({short_sha} {build_time}) {target} {profile}");
    // Cargo emits `cargo:rustc-env=KEY=VALUE`; the value side can
    // contain spaces, equals, and unicode, so it is passed verbatim.
    println!("cargo:rustc-env=UVA_LONG_VERSION={long_version}");

    // Re-run when any of the four CI-provided env vars changes.
    // `cargo:rerun-if-env-changed` is the documented hook; without
    // it, Cargo does not re-run build.rs when env changes between
    // builds (e.g. locally you build twice with and without
    // UVA_GIT_SHA set, expecting the binary to differ).
    println!("cargo:rerun-if-env-changed=UVA_GIT_SHA");
    println!("cargo:rerun-if-env-changed=UVA_BUILD_TIME");
    println!("cargo:rerun-if-env-changed=UVA_TARGET");
    println!("cargo:rerun-if-env-changed=UVA_PROFILE");

    stage_web_assets();
}

/// Stage the web front-end's built files for the `embed-web` feature.
///
/// The `embed-web` feature bakes `uva-web/dist` into the binary.
/// The macro that reads the folder needs a path inside the crate, so
/// the build copies the front-end's output into `web-dist/` first.
///
/// A build without the front-end compiled (the ordinary
/// `cargo build`, the CI lint job) must still succeed: an **empty**
/// `web-dist/` is a valid asset tree, and the gateway answers the
/// static paths with 404s while the RPC surface works normally.
fn stage_web_assets() {
    if std::env::var("CARGO_FEATURE_EMBED_WEB").is_err() {
        return;
    }
    let manifest = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR is set by Cargo");
    let source = std::path::Path::new(&manifest)
        .join("../../uva-web/dist")
        .to_path_buf();
    let target = std::path::Path::new(&manifest).join("web-dist");
    println!("cargo:rerun-if-changed={}", source.display());

    // A stale tree would embed the previous build's assets.
    let _ = std::fs::remove_dir_all(&target);
    if source.is_dir()
        && let Err(error) = copy_tree(&source, &target)
    {
        println!(
            "cargo:warning=could not stage {}: {error}",
            source.display()
        );
    }
    if std::fs::create_dir_all(&target).is_err() {
        println!("cargo:warning=could not create {}", target.display());
    }
}

/// Copy a directory tree, creating the destination as needed.
fn copy_tree(
    source: &std::path::Path,
    target: &std::path::Path,
) -> std::io::Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Print `cargo:rustc-env=<key>=<value>` if `value` is `Some` and
/// non-empty.
fn emit_env(key: &str, value: Option<String>) {
    if let Some(v) = value
        && !v.is_empty()
    {
        println!("cargo:rustc-env={key}={v}");
    }
}

/// Run `git log -1 --format=<fmt>` and return stdout, trimming the
/// trailing newline. `None` on any failure (no `.git/`, git not on
/// PATH, non-zero exit) — the env var is the primary source anyway.
fn git_field(fmt_idx: usize) -> Option<String> {
    let formats = ["%H", "%h", "%cd"];
    let fmt = formats.get(fmt_idx)?;
    let output = Command::new("git")
        .args(["log", "-1", &format!("--format={fmt}"), "--date=short"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8(output.stdout).ok()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `git_field` must return `None` (not panic) when the format
    /// index is out of range — the caller relies on graceful
    /// degradation so a future caller can pass any index without
    /// having to bounds-check.
    #[test]
    fn git_field_out_of_range_returns_none() {
        assert!(git_field(99).is_none());
    }

    /// `emit_env` must not panic on `None` or empty strings, and
    /// must produce no output when both apply. We assert via
    /// `panic = unwind` semantics: the function should return, not
    /// abort, for both degenerate inputs.
    #[test]
    fn emit_env_no_panic_on_degenerate_inputs() {
        emit_env("UVA_GIT_SHA", None);
        emit_env("UVA_GIT_SHA", Some(String::new()));
    }
}
