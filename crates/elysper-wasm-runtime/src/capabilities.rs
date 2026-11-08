//! Manifest capabilities and the host imports they unlock.
//!
//! A component importing a host function is a *request*. The manifest's
//! `capabilities` list is the *grant*, and the linker is the enforcement
//! point: a host function is defined only when its capability is granted
//! and the host supplies it. An undeclared import therefore fails during
//! [`WasmRuntime::mount`](crate::WasmRuntime::mount) with an error that
//! names the capability, before the guest runs a single instruction.
//!
//! # The import table
//!
//! | guest import | capability | host behaviour |
//! |---|---|---|
//! | `log` | `log` | always linked; forwards to `tracing` |
//! | `fs-read` | `fs:read` | reads a path confined to the runtime's workspace root |
//! | `http-request` | `net:http` | not built in; linked when the runtime has a [`CapabilityHost`] that supplies it |
//!
//! `log` is granted implicitly: every manifest may log without spelling
//! the capability out. `fs:read` is built in. `net:http` is a
//! deployment choice (an HTTP transport is a large dependency tree), so
//! this crate does not hard-code one: the embedding host installs a
//! [`CapabilityHost`] with
//! [`WasmRuntime::with_capability_host`](crate::WasmRuntime::with_capability_host),
//! and the runtime links whatever function that host defines. Without a
//! host, a declared `net:http` fails the mount with
//! [`WasmErrorKind::UnsupportedCapability`] — fail-closed, never a
//! silently missing import.
//!
//! # Host-function shape
//!
//! Every host function is lifted at the component root with the same
//! `string`-based shape the tool exports use, so a guest's binding is a
//! plain `(func (param "…" string) (result string))` import. Import
//! names are kebab-case because the component model requires it
//! (`http-request`, not `http_request`). Host functions report failure
//! as a JSON object (`{"error": "…"}`) rather than a trap, so a missing
//! file or a refused request is a tool result the guest can inspect.

use std::{
    collections::BTreeSet,
    path::{Component as PathComponent, Path},
};

use elysper_kernel::Capability;
use wasmtime::{StoreContextMut, component::Linker};

use crate::{WasmError, WasmErrorKind, limits::StoreState};

/// The implicit logging capability.
pub const CAPABILITY_LOG: &str = "log";
/// Read access to files under the runtime's workspace root.
pub const CAPABILITY_FS_READ: &str = "fs:read";
/// Outbound HTTP. Not built in; a [`CapabilityHost`] supplies it.
pub const CAPABILITY_NET_HTTP: &str = "net:http";

/// The guest import bound to [`CAPABILITY_LOG`].
pub const IMPORT_LOG: &str = "log";
/// The guest import bound to [`CAPABILITY_FS_READ`].
pub const IMPORT_FS_READ: &str = "fs-read";
/// The guest import bound to [`CAPABILITY_NET_HTTP`].
pub const IMPORT_HTTP_REQUEST: &str = "http-request";

/// The capabilities this crate implements itself.
///
/// A declared capability outside this list is not refused on sight: the
/// runtime asks its [`CapabilityHost`], if any. With no host, the mount
/// fails with [`WasmErrorKind::UnsupportedCapability`].
pub const HOST_SUPPLIED: &[&str] = &[CAPABILITY_LOG, CAPABILITY_FS_READ];

/// A host extension that supplies capabilities the built-in host lacks.
///
/// The runtime consults the host for every granted import it does not
/// implement itself (`http-request` today). A host that supplies
/// `net:http` links its transport here; the component's bytes and the
/// tool contract do not change.
///
/// Implementations must be cheap to share: the runtime holds an
/// `Arc<dyn CapabilityHost>` for its lifetime.
pub trait CapabilityHost: Send + Sync {
    /// Whether this host supplies `capability`.
    ///
    /// Only capabilities the manifest already declares reach the host;
    /// this predicate never grants a missing declaration.
    fn supplies(&self, capability: &str) -> bool;

    /// Define the host function `import` for `capability`.
    ///
    /// `import` is the component's root import name (`http-request`).
    /// Define exactly that name on `imports`; the runtime returned
    /// [`WasmErrorKind::UnsatisfiedImport`] if instantiation still cannot
    /// find it.
    ///
    /// # Errors
    ///
    /// [`WasmErrorKind::UnsatisfiedImport`] when the function cannot be
    /// defined (a duplicate or ill-typed definition).
    fn link(
        &self,
        imports: &mut HostImports<'_>,
        capability: &str,
        import: &str,
    ) -> Result<(), WasmError>;
}

/// The linker view handed to a [`CapabilityHost`].
///
/// The store data stays private: a host function sees its string
/// argument, not the runtime's limiter state. Every definition uses the
/// runtime's `(param string) (result string)` convention.
pub struct HostImports<'a> {
    linker: &'a mut Linker<StoreState>,
}

impl HostImports<'_> {
    /// Define a `(func (param "…" string) (result string))` host function.
    ///
    /// `func` receives the guest's UTF-8 argument. `Ok` is returned
    /// verbatim; `Err` becomes `{"error": "<message>"}` so the guest can
    /// handle a refused request without a trap.
    ///
    /// # Errors
    ///
    /// [`WasmErrorKind::UnsatisfiedImport`] when the linker refuses the
    /// definition (a name already defined, or a component type mismatch).
    pub fn func_wrap_string<F>(
        &mut self,
        name: &str,
        func: F,
    ) -> Result<(), WasmError>
    where
        F: Fn(String) -> Result<String, String> + Send + Sync + 'static,
    {
        self.linker
            .root()
            .func_wrap(
                name,
                move |_store: StoreContextMut<'_, StoreState>,
                      (argument,): (String,)|
                      -> wasmtime::Result<(String,)> {
                    let result = match func(argument) {
                        Ok(result) => result,
                        Err(message) => error_json(&message),
                    };
                    Ok((result,))
                },
            )
            .map_err(|error| {
                WasmError::new(
                    WasmErrorKind::UnsatisfiedImport,
                    format!("failed to define host function `{name}`: {error}"),
                )
            })
    }
}

impl std::fmt::Debug for HostImports<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("HostImports")
    }
}

/// Map a guest import name to the capability that unlocks it.
///
/// Import names are the kebab-case spellings the component model
/// accepts (`fs-read`), not the capability names (`fs:read`). An unknown
/// import returns `None`: it is not capability-gated, and if the linker
/// cannot satisfy it instantiation reports
/// [`WasmErrorKind::UnsatisfiedImport`].
#[must_use]
pub fn capability_for_import(import: &str) -> Option<&'static str> {
    match import {
        IMPORT_LOG => Some(CAPABILITY_LOG),
        IMPORT_FS_READ => Some(CAPABILITY_FS_READ),
        IMPORT_HTTP_REQUEST => Some(CAPABILITY_NET_HTTP),
        _ => None,
    }
}

/// Whether this crate implements `capability` itself.
#[must_use]
pub fn host_supplies(capability: &str) -> bool {
    HOST_SUPPLIED.contains(&capability)
}

/// The canonical import name for a capability this crate knows.
fn import_for_capability(capability: &str) -> Option<&'static str> {
    match capability {
        CAPABILITY_LOG => Some(IMPORT_LOG),
        CAPABILITY_FS_READ => Some(IMPORT_FS_READ),
        CAPABILITY_NET_HTTP => Some(IMPORT_HTTP_REQUEST),
        _ => None,
    }
}

/// Whether `capability` can be linked: built in, or supplied by `host`.
pub(crate) fn capability_is_supplied(
    capability: &str,
    host: Option<&dyn CapabilityHost>,
) -> bool {
    host_supplies(capability)
        || host.is_some_and(|host| host.supplies(capability))
}

/// The capability grants one mounted component runs under.
///
/// Built from the manifest and handed to the linker; `log` is always
/// present because the host grants it implicitly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapabilitySet(BTreeSet<String>);

impl CapabilitySet {
    /// Build the grant set from a manifest's declared capabilities.
    #[must_use]
    pub fn from_declaration(declared: &[Capability]) -> Self {
        let mut set = Self::default();
        set.0.insert(CAPABILITY_LOG.to_string());
        for capability in declared {
            set.0.insert(capability.as_str().to_string());
        }
        set
    }

    /// Whether `capability` is granted.
    #[must_use]
    pub fn contains(&self, capability: &str) -> bool {
        self.0.contains(capability)
    }

    /// Iterate the granted capability names in sorted order.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(String::as_str)
    }

    /// Number of granted capabilities.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the set is empty (never: `log` is implicit).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Define the host functions `declared` grants.
///
/// Built-ins first; then every granted capability this crate does not
/// implement is offered to `host`.
pub(crate) fn link(
    linker: &mut Linker<StoreState>,
    declared: &CapabilitySet,
    workspace_root: Option<&Path>,
    host: Option<&dyn CapabilityHost>,
) -> Result<(), WasmError> {
    linker
        .root()
        .func_wrap(
            IMPORT_LOG,
            |_store: StoreContextMut<'_, StoreState>,
             (message,): (String,)|
             -> wasmtime::Result<()> {
                tracing::info!(target: "elysper.wasm", message = %message);
                Ok(())
            },
        )
        .map_err(|error| {
            WasmError::new(
                WasmErrorKind::UnsatisfiedImport,
                format!(
                    "failed to define host function `{IMPORT_LOG}`: {error}"
                ),
            )
        })?;

    if declared.contains(CAPABILITY_FS_READ) {
        let root = workspace_root.map(Path::to_path_buf);
        linker
            .root()
            .func_wrap(
                IMPORT_FS_READ,
                move |_store: StoreContextMut<'_, StoreState>,
                      (path,): (String,)| -> wasmtime::Result<(String,)> {
                    Ok((read_confined(root.as_deref(), &path),))
                },
            )
            .map_err(|error| {
                WasmError::new(
                    WasmErrorKind::UnsatisfiedImport,
                    format!(
                        "failed to define host function `{IMPORT_FS_READ}`: {error}"
                    ),
                )
            })?;
    }

    if let Some(host) = host {
        for capability in declared.iter() {
            if host_supplies(capability) || !host.supplies(capability) {
                continue;
            }
            let Some(import) = import_for_capability(capability) else {
                continue;
            };
            let mut imports = HostImports { linker };
            host.link(&mut imports, capability, import)?;
        }
    }

    Ok(())
}

/// Read `requested` under `root`, or return a JSON error object.
///
/// The path is rejected before any IO when it is absolute or contains
/// `..`; the joined path is then canonicalized and required to stay
/// under the canonical root, so a symlink inside the workspace cannot
/// smuggle a read outside it.
fn read_confined(root: Option<&Path>, requested: &str) -> String {
    let Some(root) = root else {
        return error_json(
            "fs:read is granted but the runtime has no workspace root configured",
        );
    };
    let relative = Path::new(requested);
    let escapes = relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                PathComponent::ParentDir
                    | PathComponent::RootDir
                    | PathComponent::Prefix(_)
            )
        });
    if escapes {
        return error_json("path escapes the workspace root");
    }
    let canonical_root = match root.canonicalize() {
        Ok(root) => root,
        Err(error) => {
            return error_json(&format!("workspace root is unusable: {error}"));
        }
    };
    let joined = canonical_root.join(relative);
    let canonical = match joined.canonicalize() {
        Ok(path) => path,
        Err(error) => return error_json(&format!("read failed: {error}")),
    };
    if !canonical.starts_with(&canonical_root) {
        return error_json("path escapes the workspace root");
    }
    match std::fs::read_to_string(&canonical) {
        Ok(contents) => contents,
        Err(error) => error_json(&format!("read failed: {error}")),
    }
}

fn error_json(message: &str) -> String {
    serde_json::json!({ "error": message }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fs_read_returns_a_file_under_the_root() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("note.txt"), "hello").expect("write");
        assert_eq!(read_confined(Some(dir.path()), "note.txt"), "hello");
    }

    #[test]
    fn fs_read_refuses_parent_traversal_and_absolute_paths() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        for escaped in ["../outside.txt", "/etc/passwd", "/tmp/x"] {
            let result = read_confined(Some(root), escaped);
            let parsed: serde_json::Value =
                serde_json::from_str(&result).expect("a JSON error object");
            assert_eq!(
                parsed.get("error").and_then(serde_json::Value::as_str),
                Some("path escapes the workspace root"),
                "`{escaped}` must not leave the root"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn fs_read_refuses_a_symlink_escape() {
        let dir = tempfile::tempdir().expect("workspace temp dir");
        let outside = tempfile::tempdir().expect("outside temp dir");
        std::fs::write(outside.path().join("secret.txt"), "secret")
            .expect("write");
        std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            dir.path().join("link.txt"),
        )
        .expect("symlink");
        let result = read_confined(Some(dir.path()), "link.txt");
        assert!(
            result.contains("path escapes the workspace root"),
            "a symlink may not escape the root: {result}"
        );
    }

    #[test]
    fn fs_read_without_a_root_reports_a_json_error() {
        let result = read_confined(None, "note.txt");
        let parsed: serde_json::Value =
            serde_json::from_str(&result).expect("a JSON error object");
        assert!(
            parsed
                .get("error")
                .and_then(serde_json::Value::as_str)
                .expect("an error message")
                .contains("no workspace root")
        );
    }

    #[test]
    fn log_is_implicit_and_unknown_imports_are_not_capabilities() {
        let declared = CapabilitySet::from_declaration(&[]);
        assert!(declared.contains(CAPABILITY_LOG));
        assert!(!declared.contains(CAPABILITY_FS_READ));
        assert_eq!(capability_for_import("log"), Some(CAPABILITY_LOG));
        assert_eq!(capability_for_import("fs-read"), Some(CAPABILITY_FS_READ));
        assert_eq!(
            capability_for_import("http-request"),
            Some(CAPABILITY_NET_HTTP)
        );
        assert_eq!(capability_for_import("wasi"), None);
    }
}
