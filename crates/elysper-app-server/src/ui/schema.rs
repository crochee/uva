//! The `[ui]` segment as JSON schema the web front-end renders.
//!
//! A UI row is a description, never code: `UiBundle` carries the
//! `version` and the component list a registered renderer draws. v1
//! executes no remote JavaScript and applies no remote stylesheet, so
//! a schema naming one is rejected at load time.
//!
//! # Namespace
//!
//! UI ids carry the kernel's [`UI_PREFIX`] (`ui.`). The core loader
//! rejects a core manifest declaring a `ui.` key
//! ([`ProfileError::NamespaceViolation`]); this loader is the other
//! half of the same boundary and rejects an entry outside it.
//!
//! [`ProfileError::NamespaceViolation`]: crate::profile::ProfileError::NamespaceViolation

use elysper::kernel::{Namespace, UI_PREFIX, namespace_allows};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::profile::{Entry, RuntimeKind};

/// One UI row's rendered description.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UiSchema {
    /// The schema version the front-end renders. v1 is the current one.
    pub version: u32,
    /// The panels the row contributes, in assembly order.
    pub components: Vec<UiComponent>,
}

/// One component a UI row places in a slot.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UiComponent {
    /// Which slot renders it, e.g. `chat.sidebar`.
    pub slot: String,
    /// The registered component kind. An unknown kind is carried
    /// through unchanged and rendered as the fallback placeholder.
    pub kind: String,
    /// The component's props, a JSON object.
    pub props: Value,
}

/// Why a `[ui]` row could not be projected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UiError {
    /// The id is not in the `ui.` namespace.
    #[error(
        "entry `{id}` is not in the `ui.` namespace; UI entries must \
         start with `ui.`"
    )]
    NamespaceViolation {
        /// The rejected id.
        id: String,
    },
    /// The row names a runtime the UI loader does not read.
    #[error(
        "entry `{id}` selects runtime `{runtime}`, but the UI loader \
         reads `ui-bundle` rows only"
    )]
    WrongRuntime {
        /// The row's id.
        id: String,
        /// The row's runtime wire name.
        runtime: &'static str,
    },
    /// The row's config is not a renderable schema.
    #[error("entry `{id}` config is not a valid UI schema: {message}")]
    InvalidConfig {
        /// The row's id.
        id: String,
        /// The failing field and reason.
        message: String,
    },
    /// The row names executable content, which v1 never runs.
    #[error(
        "entry `{id}` declares `{field}`; v1 UI bundles are JSON schema \
         and never execute remote JavaScript or CSS"
    )]
    ExecutablePayload {
        /// The row's id.
        id: String,
        /// The rejected top-level field.
        field: String,
    },
}

/// Project every UI row into the schema the front-end renders.
///
/// Each row must be in the `ui.` namespace, select the `ui-bundle`
/// runtime, and carry a schema object:
///
/// ```toml
/// [[ui.entries]]
/// id = "ui.slot.chat.sidebar"
/// runtime = "ui-bundle"
/// config = { version = 1, components = [
///   { slot = "chat.sidebar", kind = "panel", props = { title = "Notes" } },
/// ] }
/// ```
///
/// # Errors
///
/// Returns [`UiError`] naming the first row that cannot be projected.
pub fn load_ui_manifest(entries: &[Entry]) -> Result<Vec<UiSchema>, UiError> {
    entries.iter().map(project).collect()
}

fn project(entry: &Entry) -> Result<UiSchema, UiError> {
    if !namespace_allows(Namespace::Ui, &entry.id) {
        return Err(UiError::NamespaceViolation {
            id: entry.id.clone(),
        });
    }
    if entry.runtime != RuntimeKind::UiBundle {
        return Err(UiError::WrongRuntime {
            id: entry.id.clone(),
            runtime: runtime_name(entry.runtime),
        });
    }
    let config = match &entry.config {
        Value::Null => return Ok(empty_schema()),
        Value::Object(object) => object,
        other => {
            return Err(invalid(
                entry,
                format!("expected a schema object, got {other}"),
            ));
        }
    };
    reject_executable(entry, config)?;

    let version = match config.get("version") {
        None => 1,
        Some(Value::Number(number)) => number
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| {
                invalid(
                    entry,
                    "`version` must be a non-negative integer".to_owned(),
                )
            })?,
        Some(other) => {
            return Err(invalid(
                entry,
                format!("`version` must be an integer, got {other}"),
            ));
        }
    };

    let components = match config.get("components") {
        None => Vec::new(),
        Some(Value::Array(values)) => values
            .iter()
            .enumerate()
            .map(|(index, value)| component(entry, index, value))
            .collect::<Result<Vec<_>, _>>()?,
        Some(other) => {
            return Err(invalid(
                entry,
                format!("`components` must be an array, got {other}"),
            ));
        }
    };

    Ok(UiSchema {
        version,
        components,
    })
}

fn component(
    entry: &Entry,
    index: usize,
    value: &Value,
) -> Result<UiComponent, UiError> {
    let Some(object) = value.as_object() else {
        return Err(invalid(
            entry,
            format!("component {index} must be an object, got {value}"),
        ));
    };
    let slot = required_string(entry, object, index, "slot")?;
    let kind = required_string(entry, object, index, "kind")?;
    let props = match object.get("props") {
        None | Some(Value::Null) => Value::Object(Map::new()),
        Some(Value::Object(_)) => object["props"].clone(),
        Some(other) => {
            return Err(invalid(
                entry,
                format!(
                    "component {index} `props` must be an object, got {other}"
                ),
            ));
        }
    };
    Ok(UiComponent { slot, kind, props })
}

fn required_string(
    entry: &Entry,
    object: &Map<String, Value>,
    index: usize,
    field: &str,
) -> Result<String, UiError> {
    match object.get(field).and_then(Value::as_str) {
        Some(value) if !value.trim().is_empty() => Ok(value.to_owned()),
        Some(_) => Err(invalid(
            entry,
            format!("component {index} `{field}` is empty"),
        )),
        None => Err(invalid(
            entry,
            format!("component {index} is missing `{field}`"),
        )),
    }
}

/// Reject the fields through which a bundle would ask the browser to
/// run code or apply remote CSS.
fn reject_executable(
    entry: &Entry,
    config: &Map<String, Value>,
) -> Result<(), UiError> {
    const FORBIDDEN: [&str; 5] =
        ["script", "scripts", "javascript", "stylesheet", "css"];
    for field in FORBIDDEN {
        if config.contains_key(field) {
            return Err(UiError::ExecutablePayload {
                id: entry.id.clone(),
                field: field.to_owned(),
            });
        }
    }
    Ok(())
}

fn empty_schema() -> UiSchema {
    UiSchema {
        version: 1,
        components: Vec::new(),
    }
}

fn invalid(entry: &Entry, message: String) -> UiError {
    UiError::InvalidConfig {
        id: entry.id.clone(),
        message,
    }
}

fn runtime_name(runtime: RuntimeKind) -> &'static str {
    match runtime {
        RuntimeKind::Native => "native",
        RuntimeKind::Wasm => "wasm",
        RuntimeKind::UiBundle => "ui-bundle",
    }
}

/// The `ui.` prefix the loader requires, re-exported for callers that
/// build entry ids.
pub const UI_NAMESPACE: &str = UI_PREFIX;

#[cfg(test)]
mod tests {
    use elysper::kernel::RuntimeKind;
    use serde_json::json;

    use crate::profile::Entry;

    fn entry(id: &str, config: serde_json::Value) -> Entry {
        Entry {
            id: id.to_owned(),
            runtime: RuntimeKind::UiBundle,
            config,
            capabilities: Vec::new(),
        }
    }

    #[test]
    fn ui_loader_rejects_core_key() {
        let entries = vec![entry(
            "tools",
            json!({ "components": [{ "slot": "chat.sidebar", "kind": "panel" }] }),
        )];
        let error = super::load_ui_manifest(&entries)
            .expect_err("a core service key is not a UI entry");
        assert!(
            matches!(error, super::UiError::NamespaceViolation { .. }),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn unknown_schema_component_falls_back() {
        // The loader keeps an unknown kind: the *renderer* owns the
        // fallback, so a schema a newer UI plugin ships still reaches
        // the client instead of failing the whole `[ui]` segment.
        let entries = vec![entry(
            "ui.slot.chat.sidebar",
            json!({
                "version": 2,
                "components": [{
                    "slot": "chat.sidebar",
                    "kind": "not-yet-known",
                    "props": { "title": "Hello" },
                }],
            }),
        )];
        let schemas =
            super::load_ui_manifest(&entries).expect("the schema loads");
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0].version, 2);
        assert_eq!(schemas[0].components[0].slot, "chat.sidebar");
        assert_eq!(schemas[0].components[0].kind, "not-yet-known");
        assert_eq!(schemas[0].components[0].props, json!({ "title": "Hello" }));
    }

    #[test]
    fn ui_loader_rejects_a_non_ui_runtime() {
        let mut entry = entry(
            "ui.slot.chat.sidebar",
            json!({ "components": [{ "slot": "chat.sidebar", "kind": "panel" }] }),
        );
        entry.runtime = RuntimeKind::Native;
        let error = super::load_ui_manifest(&[entry])
            .expect_err("the UI loader owns `ui-bundle` rows only");
        assert!(
            matches!(error, super::UiError::WrongRuntime { .. }),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn ui_loader_rejects_an_executable_payload() {
        let entries = vec![entry(
            "ui.slot.chat.sidebar",
            json!({
                "components": [],
                "script": "https://example.invalid/panel.js",
            }),
        )];
        let error = super::load_ui_manifest(&entries)
            .expect_err("v1 never executes remote JS/CSS");
        assert!(
            matches!(error, super::UiError::ExecutablePayload { .. }),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn an_empty_config_is_an_empty_schema() {
        let entries = vec![entry("ui.slot.chat.sidebar", json!({}))];
        let schemas = super::load_ui_manifest(&entries).expect("loads");
        assert_eq!(
            schemas,
            vec![super::UiSchema {
                version: 1,
                components: Vec::new(),
            }]
        );
    }
}
