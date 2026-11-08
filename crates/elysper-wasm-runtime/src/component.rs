//! The component cache, the pooled instance, and the JSON calling
//! convention.
//!
//! # Mount once, execute many times
//!
//! [`WasmRuntime::mount`] compiles the component (or reuses the cached
//! compilation for the same bytes), checks the manifest's grants against
//! the component's imports, instantiates one store/instance pair, and
//! calls `describe()` exactly once. The resulting [`WasmComponent`] owns
//! that pair and the cached schema.
//!
//! Every [`WasmComponent::execute`] re-arms the pooled store with a fresh
//! fuel budget and epoch deadline, calls `execute()`, and validates the
//! guest's answer as JSON. A successful call leaves the instance in place
//! so guest state (and the instantiation cost) is reused. A trap poisons
//! the store: Wasmtime does not promise a store is consistent after a
//! trap, so the next call instantiates a fresh one instead of resuming an
//! instance whose memory or globals may be half-written.
//!
//! # The ABI
//!
//! The component exports exactly:
//!
//! ```wit
//! describe: func() -> string;
//! execute: func(input: string) -> string;
//! ```
//!
//! Both are lifted by the canonical ABI with UTF-8 strings. `describe()`
//! returns one tool schema —
//! `{"name": string, "description": string, "parameters": JSON Schema}` —
//! or a one-element array of one. Nothing is inferred from `execute`;
//! a schema is the only thing the model is shown.

use std::{
    collections::HashMap,
    fmt::Write as _,
    sync::{
        Arc,
        Mutex,
        MutexGuard,
        PoisonError,
        Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
};

use async_trait::async_trait;
use elysper_kernel::Capability;
use elysper_tool::{Context as ToolContext, Tool, ToolOutput};
use sha2::{Digest, Sha256};
use wasmtime::{
    Config,
    Engine,
    Store,
    Trap,
    component::{Component, Instance, Linker},
};

use crate::{
    WasmError,
    WasmErrorKind,
    capabilities::{
        self,
        CapabilityHost,
        CapabilitySet,
        capability_for_import,
        capability_is_supplied,
    },
    limits::{EPOCH_TICK, StoreState, WasmLimits},
};

/// The runtime: one engine, one compilation cache, one epoch ticker.
///
/// The type is cheap to share (`Arc<WasmRuntime>`): the engine is
/// internally shared, the cache is a mutex over content hashes, and the
/// ticker is one background thread for the runtime's lifetime.
pub struct WasmRuntime {
    engine: Engine,
    limits: WasmLimits,
    // Weak: the cache never keeps a compiled component alive. The last
    // `WasmComponent` holding it evicts the entry on drop.
    cache: Mutex<HashMap<String, Weak<Component>>>,
    compilations: AtomicU64,
    workspace_root: Option<std::path::PathBuf>,
    capability_host: Option<Arc<dyn CapabilityHost>>,
    // Dropped first: the ticker stops before the engine it holds a clone
    // of goes away. The field is never read; it exists for its `Drop`.
    #[expect(dead_code, reason = "the field exists for its Drop side effect")]
    ticker: EpochTicker,
}

impl std::fmt::Debug for WasmRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WasmRuntime")
            .field("limits", &self.limits)
            .field("compilations", &self.compilations.load(Ordering::Relaxed))
            .field("workspace_root", &self.workspace_root)
            .field("capability_host", &self.capability_host.is_some())
            .finish_non_exhaustive()
    }
}

impl WasmRuntime {
    /// Build a runtime with [`WasmLimits::default`].
    ///
    /// # Errors
    ///
    /// Fails when the engine cannot be created (an unsupported host
    /// target, or a Wasmtime configuration error).
    pub fn new() -> Result<Self, WasmError> {
        Self::with_limits(WasmLimits::default())
    }

    /// Build a runtime whose every call is armed with `limits`.
    ///
    /// # Errors
    ///
    /// Fails when the engine cannot be created.
    pub fn with_limits(limits: WasmLimits) -> Result<Self, WasmError> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.consume_fuel(true);
        config.epoch_interruption(true);
        let engine = Engine::new(&config).map_err(|error| {
            WasmError::new(
                WasmErrorKind::RuntimeInit,
                format!("failed to create the Wasm engine: {error}"),
            )
        })?;
        let ticker = EpochTicker::start(&engine)?;
        Ok(Self {
            engine,
            limits,
            cache: Mutex::new(HashMap::new()),
            compilations: AtomicU64::new(0),
            workspace_root: None,
            capability_host: None,
            ticker,
        })
    }

    /// Confine the `fs:read` host function to `root`.
    ///
    /// Without a root a granted `fs:read` still links (the guest may call
    /// it), but every call returns a JSON error object: the host never
    /// reads outside a root it was explicitly given.
    #[must_use]
    pub fn with_workspace_root(
        mut self,
        root: impl Into<std::path::PathBuf>,
    ) -> Self {
        self.workspace_root = Some(root.into());
        self
    }

    /// Install the host extension that supplies non-built-in
    /// capabilities (`net:http` today).
    ///
    /// Without a host, declaring such a capability fails the mount with
    /// [`WasmErrorKind::UnsupportedCapability`]. With one, the host
    /// defines the component's import and the guest may call it.
    #[must_use]
    pub fn with_capability_host(
        mut self,
        host: Arc<dyn CapabilityHost>,
    ) -> Self {
        self.capability_host = Some(host);
        self
    }

    /// The per-call budget this runtime arms.
    #[must_use]
    pub fn limits(&self) -> &WasmLimits {
        &self.limits
    }

    /// The engine every component is compiled for.
    #[must_use]
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Number of live compiled components in the cache.
    ///
    /// An entry is removed when the last [`WasmComponent`] holding that
    /// compilation drops; a raw `Arc<Component>` returned by
    /// [`Self::compile`] keeps its entry until it drops and
    /// [`Self::evict`] is called.
    #[must_use]
    pub fn cached_components(&self) -> usize {
        self.lock_cache().len()
    }

    /// Remove the compilation for `hash` from the cache.
    ///
    /// `hash` is [`content_hash`]'s output. Returns whether an entry was
    /// removed. Dropping the last [`WasmComponent`] calls this
    /// automatically; the explicit form is for a host that compiled via
    /// [`Self::compile`] and holds no mounted component.
    pub fn evict(&self, hash: &str) -> bool {
        self.lock_cache().remove(hash).is_some()
    }

    /// Number of component compilations performed. A second mount of the
    /// same bytes does not increment it.
    #[must_use]
    pub fn compilations(&self) -> u64 {
        self.compilations.load(Ordering::Relaxed)
    }

    /// Compile `wasm`, or return the cached compilation for the same
    /// content hash.
    ///
    /// # Errors
    ///
    /// [`WasmErrorKind::InvalidComponent`] when the bytes are not a valid
    /// component (including a core module passed where a component is
    /// required).
    pub fn compile(&self, wasm: &[u8]) -> Result<Arc<Component>, WasmError> {
        let key = content_hash(wasm);
        if let Some(cached) =
            self.lock_cache().get(&key).and_then(Weak::upgrade)
        {
            return Ok(cached);
        }
        let component =
            Component::new(&self.engine, wasm).map_err(|error| {
                WasmError::new(
                    WasmErrorKind::InvalidComponent,
                    format!("not a valid WebAssembly component: {error:#}"),
                )
            })?;
        let component = Arc::new(component);
        self.compilations.fetch_add(1, Ordering::Relaxed);
        let mut cache = self.lock_cache();
        // Another thread may have compiled the same bytes while this one
        // was working; keep that compilation and let ours drop.
        if let Some(existing) = cache.get(&key).and_then(Weak::upgrade) {
            return Ok(existing);
        }
        cache.insert(key, Arc::downgrade(&component));
        Ok(component)
    }

    /// Mount `wasm` under the capabilities `declared`.
    ///
    /// Compiles (or reuses) the component, verifies the manifest grants
    /// every capability the component imports, instantiates one
    /// store/instance pair, and calls `describe()` exactly once.
    ///
    /// # Errors
    ///
    /// - [`WasmErrorKind::InvalidComponent`] for bytes that are not a
    ///   component.
    /// - [`WasmErrorKind::MissingCapability`] for an import whose
    ///   capability the manifest does not declare.
    /// - [`WasmErrorKind::UnsupportedCapability`] for a declared
    ///   capability this host does not supply.
    /// - [`WasmErrorKind::UnsatisfiedImport`] when instantiation fails
    ///   for any other reason.
    /// - [`WasmErrorKind::DescribeFailed`] when `describe()` is missing,
    ///   traps, or returns an unusable schema.
    pub fn mount(
        self: &Arc<Self>,
        wasm: &[u8],
        declared: &[Capability],
    ) -> Result<WasmComponent, WasmError> {
        let content_hash = content_hash(wasm);
        let component = self.compile(wasm)?;
        let declared = CapabilitySet::from_declaration(declared);
        check_capabilities(self, &component, &declared)?;
        let mut session = self.open_session(&component, &declared)?;
        let raw_schema = call_describe(&mut session, &self.limits)?;
        let schema = ToolSchema::parse(&raw_schema)?;
        Ok(WasmComponent {
            runtime: Arc::clone(self),
            component,
            content_hash,
            declared,
            schema,
            describe_calls: AtomicU64::new(1),
            execute_calls: AtomicU64::new(0),
            instance_creations: AtomicU64::new(1),
            session: Mutex::new(Some(session)),
        })
    }

    fn open_session(
        &self,
        component: &Component,
        declared: &CapabilitySet,
    ) -> Result<Session, WasmError> {
        let mut store = Store::new(
            &self.engine,
            StoreState::new(self.limits.max_memory_bytes),
        );
        store.limiter(|state| state as &mut dyn wasmtime::ResourceLimiter);
        // Arm before instantiation too: a store with no epoch deadline
        // set has deadline zero, so the ticker would interrupt the first
        // (slowest) instantiation instead of the first call.
        arm(&self.limits, &mut store)?;
        let mut linker = Linker::new(&self.engine);
        capabilities::link(
            &mut linker,
            declared,
            self.workspace_root.as_deref(),
            self.capability_host.as_deref(),
        )?;
        let instance =
            linker.instantiate(&mut store, component).map_err(|error| {
                WasmError::new(
                    WasmErrorKind::UnsatisfiedImport,
                    format!("component instantiation failed: {error}"),
                )
            })?;
        // Look the exports up now: a component missing either contract
        // function is rejected at mount, not on the first model call.
        instance
            .get_typed_func::<(), (String,)>(&mut store, "describe")
            .map_err(|error| {
                WasmError::new(
                    WasmErrorKind::DescribeFailed,
                    format!(
                        "component exports no `describe: func() -> string`: {error}"
                    ),
                )
            })?;
        instance
            .get_typed_func::<(String,), (String,)>(&mut store, "execute")
            .map_err(|error| {
                WasmError::new(
                    WasmErrorKind::ExecuteFailed,
                    format!(
                        "component exports no `execute: func(input: string) -> string`: {error}"
                    ),
                )
            })?;
        Ok(Session { store, instance })
    }

    fn lock_cache(&self) -> MutexGuard<'_, HashMap<String, Weak<Component>>> {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A compiled component mounted under one capability grant.
///
/// One mount owns one pooled store/instance pair and the schema
/// `describe()` returned. It *is* the [`Tool`] the kernel registers: the
/// name, description, and parameter schema come from the cached
/// `describe()`, and every call serialises the arguments to JSON, calls
/// `execute()`, and validates the JSON result.
pub struct WasmComponent {
    runtime: Arc<WasmRuntime>,
    component: Arc<Component>,
    content_hash: String,
    declared: CapabilitySet,
    schema: ToolSchema,
    describe_calls: AtomicU64,
    execute_calls: AtomicU64,
    instance_creations: AtomicU64,
    session: Mutex<Option<Session>>,
}

impl std::fmt::Debug for WasmComponent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WasmComponent")
            .field("schema", &self.schema)
            .field("capabilities", &self.declared)
            .field(
                "describe_calls",
                &self.describe_calls.load(Ordering::Relaxed),
            )
            .field("execute_calls", &self.execute_calls.load(Ordering::Relaxed))
            .field(
                "instance_creations",
                &self.instance_creations.load(Ordering::Relaxed),
            )
            .finish_non_exhaustive()
    }
}

impl WasmComponent {
    /// The schema cached from the single `describe()` call.
    #[must_use]
    pub fn schema(&self) -> &ToolSchema {
        &self.schema
    }

    /// The cache key of the compiled component
    /// ([`content_hash`] of its bytes).
    #[must_use]
    pub fn content_hash(&self) -> &str {
        &self.content_hash
    }

    /// The capabilities this instance was mounted with (`log` always).
    #[must_use]
    pub fn declared_capabilities(&self) -> &CapabilitySet {
        &self.declared
    }

    /// How many times the guest's `describe()` was invoked. Always one
    /// for a mounted component; exposed so the mount contract is a test,
    /// not a comment.
    #[must_use]
    pub fn describe_calls(&self) -> u64 {
        self.describe_calls.load(Ordering::Relaxed)
    }

    /// How many successful `execute()` invocations have completed.
    #[must_use]
    pub fn execute_calls(&self) -> u64 {
        self.execute_calls.load(Ordering::Relaxed)
    }

    /// How many store/instance pairs this mount has created. One until a
    /// trap poisons the pool, then one more per poisoned call.
    #[must_use]
    pub fn instance_creations(&self) -> u64 {
        self.instance_creations.load(Ordering::Relaxed)
    }

    /// Call `execute(input)` and return its JSON result.
    ///
    /// # Errors
    ///
    /// A stable [`WasmErrorKind`] for every failure mode: an exhausted
    /// fuel budget, a refused memory growth, an epoch interrupt, a guest
    /// trap, or a non-JSON answer.
    pub fn execute(&self, input: &str) -> Result<serde_json::Value, WasmError> {
        let mut guard =
            self.session.lock().unwrap_or_else(PoisonError::into_inner);
        if guard.is_none() {
            let session =
                self.runtime.open_session(&self.component, &self.declared)?;
            self.instance_creations.fetch_add(1, Ordering::Relaxed);
            *guard = Some(session);
        }
        let session = guard.as_mut().expect("a session was just installed");
        arm(&self.runtime.limits, &mut session.store)?;
        let function = session
            .instance
            .get_typed_func::<(String,), (String,)>(
                &mut session.store,
                "execute",
            )
            .map_err(|error| {
                WasmError::new(
                    WasmErrorKind::ExecuteFailed,
                    format!("component lost its `execute` export: {error}"),
                )
            })?;
        let result = function.call(&mut session.store, (input.to_owned(),));
        let denied = session.store.data_mut().take_memory_denied();
        match result {
            Ok((output,)) => {
                self.execute_calls.fetch_add(1, Ordering::Relaxed);
                serde_json::from_str(&output).map_err(|error| {
                    WasmError::new(
                        WasmErrorKind::InvalidJson,
                        format!(
                            "execute() returned invalid JSON ({error}); output: {output}"
                        ),
                    )
                })
            }
            Err(error) => {
                // A trapped store is not promised to be consistent, so
                // the pooled instance is discarded and the next call
                // instantiates a fresh one.
                *guard = None;
                Err(self.classify(error, denied))
            }
        }
    }

    fn classify(
        &self,
        error: wasmtime::Error,
        memory_denied: bool,
    ) -> WasmError {
        classify_standalone(error, memory_denied, &self.runtime.limits)
    }
}

impl Drop for WasmComponent {
    fn drop(&mut self) {
        // When this is the last mount of these bytes, `component` is the
        // only strong reference left. Evicting then means a plugin
        // unload releases the compiled JIT code instead of leaving it in
        // the cache forever.
        if Arc::strong_count(&self.component) == 1 {
            self.runtime.evict(&self.content_hash);
        }
    }
}

#[async_trait]
impl Tool for WasmComponent {
    fn name(&self) -> &str {
        &self.schema.name
    }

    fn description(&self) -> &str {
        &self.schema.description
    }

    fn parameters(&self) -> serde_json::Value {
        self.schema.parameters.clone()
    }

    /// Execute the component synchronously.
    ///
    /// The runtime is a blocking engine: a host on an async executor
    /// should drive it from a blocking context (the kernel's tool
    /// dispatch runs on the caller's poll, and this call does not
    /// spawn).
    async fn call(
        &self,
        input: serde_json::Value,
        _context: &ToolContext,
    ) -> ToolOutput {
        let arguments = match serde_json::to_string(&input) {
            Ok(arguments) => arguments,
            Err(error) => {
                return ToolOutput::error(format!(
                    "wasm tool `{}` could not serialise its input: {error}",
                    self.schema.name
                ));
            }
        };
        match self.execute(&arguments) {
            Ok(result) => ToolOutput::from_raw(result),
            Err(error) => {
                let mut output = ToolOutput::error(format!(
                    "wasm tool `{}` failed ({}): {error}",
                    self.schema.name,
                    error.code()
                ));
                output.metadata.insert(
                    "wasm_error_kind".to_string(),
                    serde_json::Value::String(error.code().to_string()),
                );
                output
            }
        }
    }
}

/// One tool's schema, as `describe()` returned it.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSchema {
    /// The function name the model sees.
    pub name: String,
    /// The one-line description the model sees.
    pub description: String,
    /// JSON Schema for the tool's arguments.
    pub parameters: serde_json::Value,
}

impl ToolSchema {
    /// Parse and validate a `describe()` result.
    ///
    /// Accepts one schema object or a one-element array of one object.
    ///
    /// # Errors
    ///
    /// [`WasmErrorKind::DescribeFailed`] for invalid JSON, a wrong shape,
    /// a missing `name`, more than one schema, or a `parameters` value
    /// that is not an object.
    pub fn parse(raw: &str) -> Result<Self, WasmError> {
        let value: serde_json::Value =
            serde_json::from_str(raw).map_err(|error| {
                WasmError::new(
                    WasmErrorKind::DescribeFailed,
                    format!(
                        "describe() returned invalid JSON ({error}); output: {raw}"
                    ),
                )
            })?;
        let value = match value {
            serde_json::Value::Array(mut items) => {
                if items.len() != 1 {
                    return Err(WasmError::new(
                        WasmErrorKind::DescribeFailed,
                        format!(
                            "describe() must return exactly one tool schema, got {}",
                            items.len()
                        ),
                    ));
                }
                items.pop().expect("length checked above")
            }
            other => other,
        };
        let object = value.as_object().ok_or_else(|| {
            WasmError::new(
                WasmErrorKind::DescribeFailed,
                "describe() must return a JSON object",
            )
        })?;
        let name = object
            .get("name")
            .and_then(serde_json::Value::as_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| {
                WasmError::new(
                    WasmErrorKind::DescribeFailed,
                    "describe() returned no `name`",
                )
            })?;
        let description = object
            .get("description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let parameters = match object.get("parameters") {
            Some(parameters) if parameters.is_object() => parameters.clone(),
            Some(_) => {
                return Err(WasmError::new(
                    WasmErrorKind::DescribeFailed,
                    "describe() returned a non-object `parameters` schema",
                ));
            }
            None => serde_json::json!({ "type": "object" }),
        };
        Ok(Self {
            name: name.to_string(),
            description: description.to_string(),
            parameters,
        })
    }
}

struct Session {
    store: Store<StoreState>,
    instance: Instance,
}

/// Re-arm one call: fresh fuel, a clean memory-denial record, and an
/// epoch deadline measured from now.
fn arm(
    limits: &WasmLimits,
    store: &mut Store<StoreState>,
) -> Result<(), WasmError> {
    store.set_fuel(limits.fuel).map_err(|error| {
        WasmError::new(
            WasmErrorKind::ExecuteFailed,
            format!("failed to arm the fuel budget: {error}"),
        )
    })?;
    store.data_mut().reset_for_call();
    let ticks = if limits.deadline_enabled() {
        limits.deadline_ticks()
    } else {
        // Effectively never: the ticker still advances, the store never
        // reaches this deadline.
        u64::MAX / 2
    };
    store.set_epoch_deadline(ticks);
    Ok(())
}

fn call_describe(
    session: &mut Session,
    limits: &WasmLimits,
) -> Result<String, WasmError> {
    arm(limits, &mut session.store)?;
    let function = session
        .instance
        .get_typed_func::<(), (String,)>(&mut session.store, "describe")
        .map_err(|error| {
            WasmError::new(
                WasmErrorKind::DescribeFailed,
                format!(
                    "component exports no `describe: func() -> string`: {error}"
                ),
            )
        })?;
    let result = function.call(&mut session.store, ());
    let denied = session.store.data_mut().take_memory_denied();
    match result {
        Ok((schema,)) => Ok(schema),
        Err(error) => Err(classify_standalone(error, denied, limits)),
    }
}

fn classify_standalone(
    error: wasmtime::Error,
    memory_denied: bool,
    limits: &WasmLimits,
) -> WasmError {
    // The trap itself is the authority: a guest that was refused memory
    // and then ran out of fuel (or missed its deadline) reports the trap
    // that actually ended the call. The denial heuristic only names a
    // call whose trap is otherwise generic (`unreachable`, …).
    if let Some(trap) = error.downcast_ref::<Trap>() {
        match trap {
            Trap::OutOfFuel => {
                return WasmError::new(
                    WasmErrorKind::FuelExhausted,
                    format!(
                        "guest exhausted its {} fuel: {error}",
                        limits.fuel
                    ),
                );
            }
            Trap::Interrupt => {
                return WasmError::new(
                    WasmErrorKind::Timeout,
                    format!(
                        "guest exceeded its {:?} wall-clock deadline: {error}",
                        limits.deadline
                    ),
                );
            }
            _ => {}
        }
    }
    if memory_denied {
        return WasmError::new(
            WasmErrorKind::MemoryLimit,
            format!(
                "guest linear memory was refused growth past {} bytes: {error}",
                limits.max_memory_bytes
            ),
        );
    }
    WasmError::new(WasmErrorKind::Trap, format!("guest trapped: {error}"))
}

/// Check the component's imports against the manifest's grants.
fn check_capabilities(
    runtime: &WasmRuntime,
    component: &Component,
    declared: &CapabilitySet,
) -> Result<(), WasmError> {
    let imports: Vec<String> = component
        .component_type()
        .imports(&runtime.engine)
        .map(|(name, _extern)| name.to_string())
        .collect();
    for name in imports {
        let Some(capability) = capability_for_import(&name) else {
            continue;
        };
        if !declared.contains(capability) {
            return Err(WasmError::new(
                WasmErrorKind::MissingCapability,
                format!(
                    "component imports `{name}`, which requires the `{capability}` capability \
                     the manifest does not declare"
                ),
            ));
        }
        if !capability_is_supplied(
            capability,
            runtime.capability_host.as_deref(),
        ) {
            return Err(WasmError::new(
                WasmErrorKind::UnsupportedCapability,
                format!(
                    "the manifest declares `{capability}`, but neither the built-in host                      ({}) nor an installed `CapabilityHost` supplies it",
                    capability_list()
                ),
            ));
        }
    }
    Ok(())
}

/// The cache key for `wasm`: the lowercase hex SHA-256 of its bytes.
#[must_use]
pub fn content_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        // Writing into a `String` is infallible.
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// One background thread per runtime advancing the engine epoch.
///
/// Epoch interruption is the only interrupt that reaches a guest stuck
/// in an infinite loop; each call converts its wall-clock deadline into
/// a number of ticks and sets it on the store. The ticker always runs,
/// even when every configured deadline is zero, so enabling a deadline
/// later does not require restarting the engine.
struct EpochTicker {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl EpochTicker {
    fn start(engine: &Engine) -> Result<Self, WasmError> {
        let engine = engine.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = thread::Builder::new()
            .name("elysper-wasm-epoch".to_string())
            .spawn(move || {
                while !flag.load(Ordering::Relaxed) {
                    thread::sleep(EPOCH_TICK);
                    engine.increment_epoch();
                }
            })
            .map_err(|error| {
                WasmError::new(
                    WasmErrorKind::RuntimeInit,
                    format!(
                        "failed to start the Wasm epoch ticker, so wall-clock                          deadlines cannot be enforced: {error}"
                    ),
                )
            })?;
        Ok(Self {
            stop,
            handle: Some(handle),
        })
    }
}

impl Drop for EpochTicker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Capability names in the "host supplies only …" message.
const fn capability_list() -> &'static str {
    "`log` and `fs:read`"
}
