//! The `[ui]` segment assembled into a slot tree.
//!
//! A UI row is a description, never code. A row may **declare** the
//! slots it owns and **register** components into slots any row
//! declared before it:
//!
//! ```toml
//! [[ui.entries]]
//! id = "ui.shell"
//! runtime = "ui-bundle"
//! config = { version = 1, slots = [
//!   { id = "chat.sidebar", cardinality = "many" },
//! ] }
//!
//! [[ui.entries]]
//! id = "ui.slot.notes"
//! runtime = "ui-bundle"
//! config = { version = 1, components = [
//!   { slot = "chat.sidebar", kind = "panel", props = { title = "Notes" } },
//! ] }
//! ```
//!
//! # Effect semantics
//!
//! [`UiTree::declare`] and [`UiTree::register`] return revocable
//! handles: dropping a [`SlotHandle`] withdraws the declaration and
//! every registration into it, dropping a [`ComponentHandle`] withdraws
//! one registration. Clones share the guard, so the rollback runs when
//! the **last** clone drops — the same contract as the kernel's
//! [`EffectHandle`](uva::kernel::EffectHandle).
//!
//! [`load_ui_tree`] is the activation stage: it reads the rows in
//! assembly order, declares each row's slots, then registers its
//! components, and retains every handle in the returned tree. Swapping
//! the tree (HMR) therefore drops the old one and rolls every
//! declaration and registration back.
//!
//! # Activation failures
//!
//! Three violations fail the **whole activation** instead of surfacing
//! at render time: registering into a slot no row declares
//! ([`UiError::UnknownSlot`]), declaring a slot another row already
//! declared ([`UiError::SlotOccupied`]), and a second registration into
//! a `one`-cardinality slot ([`UiError::CardinalityExceeded`]). Boot
//! refuses to start and the HMR reload refuses to swap the tree.
//!
//! # Slots, services and namespaces
//!
//! Slot ids are host-agnostic (`chat.sidebar` is a DOM region in the
//! browser and a ratatui column in the terminal) and are **not** kernel
//! service keys: they carry no `ui.` prefix and never enter the service
//! registry. The three enforcement points are:
//!
//! 1. the core loader rejects a core manifest declaring a `ui.*`
//!    service key ([`ProfileError::NamespaceViolation`], `build_one`);
//! 2. this loader rejects a row id outside `ui.`
//!    ([`UiError::NamespaceViolation`]);
//! 3. `crate::profile::model::segment_of` routes every `ui-bundle` row
//!    into the `[ui]` segment, so such a row never reaches the core
//!    loader at all.
//!
//! [`ProfileError::NamespaceViolation`]:
//!     crate::profile::ProfileError::NamespaceViolation

use std::sync::{
    Arc,
    Mutex,
    MutexGuard,
    atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uva::kernel::{Namespace, namespace_allows};

pub use super::schema::{UiComponent, UiError, UiSchema};
use crate::profile::{Entry, RuntimeKind};

/// How many components one slot accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Cardinality {
    /// Exactly one; a second registration fails activation.
    One,
    /// Any number, in registration order.
    Many,
}

/// One slot a `ui-bundle` row declares.
///
/// Slot ids are host-agnostic (`chat.sidebar` is a DOM region in the
/// browser and a ratatui column in the terminal) and are **not**
/// kernel service keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SlotDecl {
    /// The slot id, e.g. `chat.sidebar`.
    pub id: String,
    /// How many components the slot accepts.
    pub cardinality: Cardinality,
}

/// The wire projection of one declared slot (`ui/schema`'s `slots`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SlotInfo {
    /// The host-agnostic slot id.
    pub id: String,
    /// How many components the slot accepts.
    pub cardinality: Cardinality,
    /// The declaring `[ui]` row's id (`ui.`-namespaced).
    pub owner: String,
}

/// The assembled UI tree: declarations and registrations, each owned
/// by a revocable handle.
///
/// A clone shares the table and the retained handles; the retained
/// handles roll back when the last clone drops.
#[derive(Clone)]
pub struct UiTree {
    table: Arc<UiTable>,
    retained: Arc<Mutex<Vec<Retained>>>,
}

impl Default for UiTree {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for UiTree {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let retained: Vec<String> = lock(&self.retained)
            .iter()
            .map(Retained::describe)
            .collect();
        f.debug_struct("UiTree")
            .field("slots", &self.slots())
            .field("retained", &retained)
            .finish()
    }
}

impl UiTree {
    /// An empty tree.
    #[must_use]
    pub fn new() -> Self {
        Self {
            table: Arc::new(UiTable::default()),
            retained: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Declare one slot.
    ///
    /// The returned handle owns the declaration: dropping it (with no
    /// clone left) withdraws the slot and every registration into it.
    /// [`load_ui_tree`] retains the handle in the tree.
    ///
    /// # Errors
    ///
    /// Returns [`UiError::SlotOccupied`] when the id is already
    /// declared.
    pub fn declare(
        &self,
        owner: &str,
        decl: SlotDecl,
    ) -> Result<SlotHandle, UiError> {
        let mut slots = lock(&self.table.slots);
        if let Some(existing) =
            slots.iter().find(|record| record.decl.id == decl.id)
        {
            return Err(UiError::SlotOccupied {
                id: owner.to_owned(),
                slot: decl.id,
                owner: existing.owner.clone(),
            });
        }
        let slot = decl.id.clone();
        slots.push(SlotRecord {
            decl,
            owner: owner.to_owned(),
            registrations: Vec::new(),
        });
        Ok(SlotHandle {
            guard: Arc::new(SlotGuard {
                table: Arc::clone(&self.table),
                slot,
            }),
        })
    }

    /// Register one component into a declared slot.
    ///
    /// `version` is the declaring row's schema version; the projection
    /// groups a row's registrations under that version.
    ///
    /// # Errors
    ///
    /// Returns [`UiError::UnknownSlot`] when nothing declares the slot,
    /// or [`UiError::CardinalityExceeded`] when a `one` slot already
    /// holds a component.
    pub fn register(
        &self,
        owner: &str,
        version: u32,
        component: UiComponent,
    ) -> Result<ComponentHandle, UiError> {
        let seq = self.table.next_seq.fetch_add(1, Ordering::Relaxed);
        let mut slots = lock(&self.table.slots);
        let Some(record) = slots
            .iter_mut()
            .find(|record| record.decl.id == component.slot)
        else {
            return Err(UiError::UnknownSlot {
                id: owner.to_owned(),
                slot: component.slot,
            });
        };
        if record.decl.cardinality == Cardinality::One
            && let Some(first) = record.registrations.first()
        {
            return Err(UiError::CardinalityExceeded {
                id: owner.to_owned(),
                slot: component.slot,
                owner: first.owner.clone(),
            });
        }
        record.registrations.push(Registration {
            seq,
            owner: owner.to_owned(),
            version,
            component,
        });
        Ok(ComponentHandle {
            guard: Arc::new(ComponentGuard {
                table: Arc::clone(&self.table),
                seq,
            }),
        })
    }

    /// The declared slots, in declaration order.
    #[must_use]
    pub fn slots(&self) -> Vec<SlotInfo> {
        lock(&self.table.slots)
            .iter()
            .map(|record| SlotInfo {
                id: record.decl.id.clone(),
                cardinality: record.decl.cardinality,
                owner: record.owner.clone(),
            })
            .collect()
    }

    /// One [`UiSchema`] per `[ui]` row, in assembly order.
    ///
    /// A row without components keeps its empty schema, which is
    /// today's wire shape.
    #[must_use]
    pub fn schemas(&self) -> Vec<UiSchema> {
        let rows: Vec<RowRecord> = lock(&self.table.rows).clone();
        let slots = lock(&self.table.slots);
        rows.into_iter()
            .map(|row| {
                let mut registered: Vec<&Registration> = slots
                    .iter()
                    .flat_map(|record| record.registrations.iter())
                    .filter(|registration| {
                        registration.owner == row.owner
                            && registration.version == row.version
                    })
                    .collect();
                registered.sort_by_key(|registration| registration.seq);
                UiSchema {
                    version: row.version,
                    components: registered
                        .into_iter()
                        .map(|registration| registration.component.clone())
                        .collect(),
                }
            })
            .collect()
    }

    /// The components delivered into `slot`, in registration order.
    #[must_use]
    pub fn components_in(&self, slot: &str) -> Vec<UiComponent> {
        lock(&self.table.slots)
            .iter()
            .find(|record| record.decl.id == slot)
            .map(|record| {
                record
                    .registrations
                    .iter()
                    .map(|registration| registration.component.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Keep one handle alive for as long as the tree is alive.
    fn retain(&self, handle: Retained) {
        lock(&self.retained).push(handle);
    }
}

/// Dropping rolls the declaration **and every registration into it**
/// back.
#[derive(Clone)]
pub struct SlotHandle {
    guard: Arc<SlotGuard>,
}

impl std::fmt::Debug for SlotHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlotHandle")
            .field("slot", &self.guard.slot)
            .finish()
    }
}

/// Dropping rolls this one registration back.
#[derive(Clone)]
pub struct ComponentHandle {
    guard: Arc<ComponentGuard>,
}

impl std::fmt::Debug for ComponentHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComponentHandle")
            .field("seq", &self.guard.seq)
            .finish()
    }
}

/// The activation stage: read the `[ui]` rows in assembly order,
/// declare each row's slots, then register its components.
///
/// Any violation fails the whole activation — boot refuses to start
/// and the HMR reload refuses to swap the tree.
///
/// # Errors
///
/// Returns [`UiError`] naming the first row (or component) that cannot
/// be activated.
pub fn load_ui_tree(entries: &[Entry]) -> Result<UiTree, UiError> {
    let tree = UiTree::new();
    for entry in entries {
        if let Err(error) = load_row(&tree, entry) {
            // The rejection is the point: a row that names a slot
            // nobody declared (or declares one twice) must not mount.
            // Record it, so an operator can see *which* row was
            // refused instead of only that the boot failed.
            tracing::error!(
                target: "uva::ui",
                row = %entry.id,
                error = %error,
                "a ui row was refused by the slot loader"
            );
            return Err(error);
        }
    }
    Ok(tree)
}

/// One retained handle: the tree holds every declaration and
/// registration `load_ui_tree` made, so dropping the tree revokes them
/// all.
#[derive(Clone)]
enum Retained {
    Slot(SlotHandle),
    Component(ComponentHandle),
}

impl Retained {
    /// The handle this tree keeps alive, rendered for diagnostics.
    fn describe(&self) -> String {
        match self {
            Self::Slot(handle) => format!("slot {}", handle.guard.slot),
            Self::Component(handle) => {
                format!("component #{}", handle.guard.seq)
            }
        }
    }
}

fn load_row(tree: &UiTree, entry: &Entry) -> Result<(), UiError> {
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
        Value::Null => {
            record_row(tree, &entry.id, 1);
            return Ok(());
        }
        Value::Object(object) => object,
        other => {
            return Err(invalid(
                entry,
                format!("expected a schema object, got {other}"),
            ));
        }
    };
    reject_executable(entry, config)?;
    let version = version(entry, config)?;
    record_row(tree, &entry.id, version);

    let declarations = match config.get("slots") {
        None => Vec::new(),
        Some(Value::Array(values)) => values
            .iter()
            .enumerate()
            .map(|(index, value)| slot_decl(entry, index, value))
            .collect::<Result<Vec<_>, _>>()?,
        Some(other) => {
            return Err(invalid(
                entry,
                format!("`slots` must be an array, got {other}"),
            ));
        }
    };
    for decl in declarations {
        let handle = tree.declare(&entry.id, decl)?;
        tree.retain(Retained::Slot(handle));
    }

    match config.get("components") {
        None => Ok(()),
        Some(Value::Array(values)) => {
            for (index, value) in values.iter().enumerate() {
                let component = component(entry, index, value)?;
                let handle = tree.register(&entry.id, version, component)?;
                tree.retain(Retained::Component(handle));
            }
            Ok(())
        }
        Some(other) => Err(invalid(
            entry,
            format!("`components` must be an array, got {other}"),
        )),
    }
}

/// Parse one slot declaration, rejecting executable fields on the
/// declaration object itself.
fn slot_decl(
    entry: &Entry,
    index: usize,
    value: &Value,
) -> Result<SlotDecl, UiError> {
    let Some(object) = value.as_object() else {
        return Err(invalid(
            entry,
            format!("slot {index} must be an object, got {value}"),
        ));
    };
    reject_executable(entry, object)?;
    let id = match object.get("id").and_then(Value::as_str) {
        Some(id) if !id.trim().is_empty() => id.to_owned(),
        Some(_) => {
            return Err(invalid(entry, format!("slot {index} `id` is empty")));
        }
        None => {
            return Err(invalid(
                entry,
                format!("slot {index} is missing `id`"),
            ));
        }
    };
    let cardinality = match object.get("cardinality") {
        None | Some(Value::Null) => Cardinality::Many,
        Some(Value::String(name)) => match name.as_str() {
            "one" => Cardinality::One,
            "many" => Cardinality::Many,
            other => {
                return Err(invalid(
                    entry,
                    format!(
                        "slot {index} `cardinality` must be `one` or \
                         `many`, got `{other}`"
                    ),
                ));
            }
        },
        Some(other) => {
            return Err(invalid(
                entry,
                format!(
                    "slot {index} `cardinality` must be a string, got {other}"
                ),
            ));
        }
    };
    Ok(SlotDecl { id, cardinality })
}

fn version(entry: &Entry, config: &Map<String, Value>) -> Result<u32, UiError> {
    match config.get("version") {
        None => Ok(1),
        Some(Value::Number(number)) => number
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| {
                invalid(
                    entry,
                    "`version` must be a non-negative integer".to_owned(),
                )
            }),
        Some(other) => Err(invalid(
            entry,
            format!("`version` must be an integer, got {other}"),
        )),
    }
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
    reject_executable(entry, object)?;
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

/// Reject the fields through which a bundle would ask a host to run
/// code or apply remote CSS.
fn reject_executable(
    entry: &Entry,
    object: &Map<String, Value>,
) -> Result<(), UiError> {
    const FORBIDDEN: [&str; 5] =
        ["script", "scripts", "javascript", "stylesheet", "css"];
    for field in FORBIDDEN {
        if object.contains_key(field) {
            return Err(UiError::ExecutablePayload {
                id: entry.id.clone(),
                field: field.to_owned(),
            });
        }
    }
    Ok(())
}

fn record_row(tree: &UiTree, owner: &str, version: u32) {
    lock(&tree.table.rows).push(RowRecord {
        owner: owner.to_owned(),
        version,
    });
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
        RuntimeKind::UiBundle => "ui-bundle",
    }
}

/// The mutable half of a [`UiTree`]: rows in assembly order, slots in
/// declaration order, and the registration sequence counter.
#[derive(Default)]
struct UiTable {
    rows: Mutex<Vec<RowRecord>>,
    slots: Mutex<Vec<SlotRecord>>,
    next_seq: AtomicU64,
}

#[derive(Clone)]
struct RowRecord {
    owner: String,
    version: u32,
}

struct SlotRecord {
    decl: SlotDecl,
    owner: String,
    registrations: Vec<Registration>,
}

struct Registration {
    seq: u64,
    owner: String,
    version: u32,
    component: UiComponent,
}

struct SlotGuard {
    table: Arc<UiTable>,
    slot: String,
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        let mut slots = lock(&self.table.slots);
        if let Some(index) =
            slots.iter().position(|record| record.decl.id == self.slot)
        {
            slots.remove(index);
        }
    }
}

struct ComponentGuard {
    table: Arc<UiTable>,
    seq: u64,
}

impl Drop for ComponentGuard {
    fn drop(&mut self) {
        let mut slots = lock(&self.table.slots);
        for record in slots.iter_mut() {
            if let Some(index) = record
                .registrations
                .iter()
                .position(|registration| registration.seq == self.seq)
            {
                record.registrations.remove(index);
                break;
            }
        }
    }
}

/// Lock the table. Nothing runs user code while a lock is held, so the
/// only poison source is a panic in this module; recovering keeps the
/// tree usable instead of cascading the panic into every reader.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

#[cfg(test)]
mod tests;
