//! The shell's entire input surface, as pure functions.
//!
//! Parsing and resource **ordering** live here; the IO (reading
//! `std::env`, `Path::is_file`, `current_exe`, `resource_dir`) stays
//! with the caller in [`crate::run`], which is what makes every
//! decision here unit-testable without touching disk or process
//! state.

use std::path::{Path, PathBuf};

/// The usage text `--help` and parse errors print.
const USAGE: &str = "\
usage: uva-desktop [--directory DIR] [--profile PATH]\
 [--bundle PATH] [--patch PATH]
                    [--app-server PATH] [--web-dir PATH]\
 [--dev-url URL]

  --directory DIR    workspace the gateway serves (default: .)
  --profile PATH     plugin profile TOML (passed to serve)
  --bundle PATH      bundle seed TOML (lowest layer)
  --patch PATH       patch TOML layered over the profile
  --app-server PATH  the uva-app-server binary\
 (env: UVA_APP_SERVER)
  --web-dir PATH     the web front-end dist\
 (default: resource dir, then the dev checkout)
  --dev-url URL      the URL a debug window opens\
 (default: http://localhost:5173)";

/// Everything the shell needs to launch: the pass-through surface of
/// `uva-app-server serve` plus the shell's own resource and
/// window knobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// The workspace directory (passed through as `serve --directory`;
    /// design D10: defaults to the process working directory).
    pub directory: PathBuf,
    /// The plugin profile TOML overriding the default lookup.
    pub profile: Option<PathBuf>,
    /// The bundle seed TOML (the lowest layer), if any.
    pub bundle: Option<PathBuf>,
    /// The patch TOML layered over the profile, if any.
    pub patch: Option<PathBuf>,
    /// `--app-server` or the env `UVA_APP_SERVER`.
    pub app_server: Option<PathBuf>,
    /// `--web-dir`: an explicit override for the dist location.
    pub web_dir: Option<PathBuf>,
    /// `--dev-url`: the address a debug-build window loads (defaults
    /// to the Vite dev server, design D2).
    pub dev_url: Option<String>,
}

/// The known environment inputs, injected so tests never read the
/// process environment.
#[derive(Debug, Clone, Default)]
pub struct Env {
    /// `UVA_APP_SERVER` — same variable, same meaning as the TUI.
    pub app_server: Option<String>,
    /// `UVA_BACKEND_PORT` — the port the Vite proxy targets.
    pub backend_port: Option<String>,
}

impl Options {
    /// Parse the command line (without `argv[0]`) against the known
    /// environment. `--help` and unknown flags come back as `Err`
    /// carrying the usage text; the caller prints and exits.
    pub fn parse_from(args: &[String], env: &Env) -> Result<Self, String> {
        let mut options = Self {
            directory: PathBuf::from("."),
            profile: None,
            bundle: None,
            patch: None,
            app_server: None,
            web_dir: None,
            dev_url: None,
        };
        let mut args = args.iter();
        while let Some(flag) = args.next() {
            let value = match flag.as_str() {
                "--help" => return Err(USAGE.to_owned()),
                "--directory" | "--profile" | "--bundle" | "--patch"
                | "--app-server" | "--web-dir" | "--dev-url" => args
                    .next()
                    .ok_or_else(|| format!("{flag} expects a value"))?,
                unknown => {
                    return Err(format!("unknown flag: {unknown}\n{USAGE}"));
                }
            };
            match flag.as_str() {
                "--directory" => options.directory = PathBuf::from(value),
                "--profile" => options.profile = Some(PathBuf::from(value)),
                "--bundle" => options.bundle = Some(PathBuf::from(value)),
                "--patch" => options.patch = Some(PathBuf::from(value)),
                "--app-server" => {
                    options.app_server = Some(PathBuf::from(value))
                }
                "--web-dir" => options.web_dir = Some(PathBuf::from(value)),
                _ => options.dev_url = Some(value.clone()),
            }
        }
        // The environment only backstops the sidecar binary — same
        // variable, same precedence as the TUI's `--app-server`.
        if options.app_server.is_none() {
            options.app_server = env.app_server.as_deref().map(PathBuf::from);
        }
        Ok(options)
    }
}

/// Where to look for `uva-app-server` (design D10): the
/// flag/env override first, then next to the shell's own binary (in
/// a packaged bundle that is exactly where the bundler drops the
/// sidecar, minus the target-triple suffix), then bare `PATH`.
pub fn app_server_candidates(
    flag: Option<&Path>,
    exe_dir: Option<&Path>,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(flag) = flag {
        candidates.push(flag.to_path_buf());
    }
    if let Some(dir) = exe_dir {
        candidates.push(dir.join("uva-app-server"));
    }
    candidates.push(PathBuf::from("uva-app-server"));
    candidates
}

/// Where to look for the web dist: the `--web-dir` override, then
/// the packaged resource copy (`resource_dir()/dist`), then the dev
/// checkout — twice, because `make run` starts in `desktop/` while
/// `cargo tauri dev` starts in `desktop/src-tauri/`.
pub fn web_dir_candidates(
    flag: Option<&Path>,
    resource_dist: Option<&Path>,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(flag) = flag {
        candidates.push(flag.to_path_buf());
    }
    if let Some(resource) = resource_dist {
        candidates.push(resource.to_path_buf());
    }
    candidates.push(PathBuf::from("../uva-web/dist"));
    candidates.push(PathBuf::from("../../uva-web/dist"));
    candidates
}

/// The first candidate the injected `exists` predicate accepts.
/// Tests pass their own predicate; production passes `Path::is_file`
/// or `Path::is_dir`.
pub fn first_existing(
    candidates: &[PathBuf],
    exists: impl Fn(&Path) -> bool,
) -> Option<&PathBuf> {
    candidates.iter().find(|candidate| exists(candidate))
}

/// The gateway port for this session (design D4 / plan Review Focus
/// #2): release builds hand `0` to the supervisor (it picks a free
/// port, and the window reads `base_url()`); debug builds bind the
/// fixed port the Vite dev-server proxy targets —
/// `UVA_BACKEND_PORT`, default `8080` (`uva-web/vite.config.ts`).
pub fn gateway_port(
    is_release: bool,
    backend_port: Option<&str>,
) -> Result<u16, String> {
    if is_release {
        return Ok(0);
    }
    match backend_port {
        None => Ok(8080),
        Some(raw) => raw.parse::<u16>().map_err(|_| {
            format!("UVA_BACKEND_PORT is not a port number: {raw}")
        }),
    }
}
