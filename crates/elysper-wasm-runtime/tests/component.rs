//! End-to-end tests for the component runtime, driven by a hand-written
//! WAT component.
//!
//! # The fixture ABI
//!
//! The fixture is a real WebAssembly component (not a core module wrapped
//! in a lie). Its core module exports:
//!
//! - `memory` — the linear memory the canonical ABI lifts strings out of.
//! - `cabi_realloc(old_ptr, old_size, align, new_size) -> ptr` — the bump
//!   allocator the canonical adapter calls for argument and return areas.
//! - `describe() -> retptr` — a pointer to a `(ptr, len)` pair naming the
//!   schema JSON in linear memory.
//! - `execute(in_ptr, in_len) -> retptr` — the input string arrives as a
//!   `(ptr, len)` pair; the result is named by a `(ptr, len)` pair at the
//!   returned pointer.
//!
//! The component lift declarations are:
//!
//! ```wat
//! (func (export "describe") (result string)
//!   (canon lift (core func $i "describe") (memory (core memory $i "memory"))
//!               (realloc (core func $i "cabi_realloc"))))
//! (func (export "execute") (param "input" string) (result string)
//!   (canon lift (core func $i "execute") (memory (core memory $i "memory"))
//!               (realloc (core func $i "cabi_realloc"))))
//! ```
//!
//! That is the canonical ABI for a `string` result and a `string`
//! parameter: the parameter flattens to `(ptr, len)` directly, while the
//! result is too wide to return directly, so the guest allocates a
//! return area and returns its pointer (Wasmtime 50's lifted-export
//! lowering). Strings are UTF-8, the default canonical encoding. The
//! fixture's return area is a fixed 8-byte scratch slot at address 64;
//! the supported calling convention is one call at a time per instance.
//! The bump allocator starts at 8192, so the 4096 scratch slot and the
//! static payloads below it are never reused by `cabi_realloc`.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use elysper_kernel::{
    Capability,
    Context,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    PluginManifest,
    RuntimeKind,
    WasmAbi,
    mount_all,
};
use elysper_tool::{
    Context as ToolContext,
    ToolRegistry,
    plugin::{KEY_TOOLS, ToolRegistryPlugin},
};
use elysper_tool_shell::{ExecutionPolicy, SandboxBackend};
use elysper_wasm_runtime::{
    CapabilityHost,
    HostImports,
    WasmError,
    WasmErrorKind,
    WasmLimits,
    WasmRuntime,
    WasmSandbox,
    WasmToolPlugin,
};
use serde_json::{Value, json};

/// The schema `describe()` returns. `name` is what the `Tool` surface
/// advertises.
const SCHEMA: &str = concat!(
    r#"{"name":"echo","description":"Echo the JSON input","#,
    r#""parameters":{"type":"object","properties":{"value":{"type":"string"}}}}"#,
);

/// A fixed non-JSON payload the fixture can return from `execute`.
const NOT_JSON: &str = "not json";

const ECHO: &str = r#"
      (i32.store (i32.const 4096) (local.get $ptr))
      (i32.store offset=4 (i32.const 4096) (local.get $len))
      (i32.const 4096)
"#;

const LOOP_FOREVER: &str = r#"
      (loop $spin (br $spin))
      unreachable
"#;

const GROW_UNTIL_REFUSED: &str = r#"
      (block $stop
        (loop $grow
          (br_if $stop (i32.eq (memory.grow (i32.const 1)) (i32.const -1)))
          (br $grow)))
      unreachable
"#;

const RETURN_NOT_JSON: &str = r#"
      (i32.store (i32.const 4096) (i32.const 2048))
      (i32.store offset=4 (i32.const 4096) (i32.const 8))
      (i32.const 4096)
"#;

/// Trap when the input starts with `t`; echo otherwise. Used to prove a
/// trapped store is discarded instead of resumed.
const TRAP_ON_T: &str = r#"
      (if (i32.eq (i32.load8_u (local.get $ptr)) (i32.const 116))
        (then unreachable))
      (i32.store (i32.const 4096) (local.get $ptr))
      (i32.store offset=4 (i32.const 4096) (local.get $len))
      (i32.const 4096)
"#;

fn wat_string(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// Build a component whose `execute` body is `body`, with `extra_imports`
/// spliced at the component's root.
fn component(extra_imports: &str, body: &str) -> Vec<u8> {
    let schema = wat_string(SCHEMA);
    let not_json = wat_string(NOT_JSON);
    format!(
        r#"(component
{extra_imports}
  (core module $lib
    (memory (export "memory") 1)
    (global $next (mut i32) (i32.const 8192))
    (data (i32.const 16) "{schema}")
    (data (i32.const 2048) "{not_json}")
    (func (export "cabi_realloc")
      (param $old_ptr i32) (param $old_size i32)
      (param $align i32) (param $new_size i32)
      (result i32)
      (local $ptr i32)
      (local $mask i32)
      (local.set $mask (i32.sub (local.get $align) (i32.const 1)))
      (local.set $ptr
        (i32.and
          (i32.add (global.get $next) (local.get $mask))
          (i32.xor (local.get $mask) (i32.const -1))))
      (global.set $next (i32.add (local.get $ptr) (local.get $new_size)))
      (if (i32.ne (local.get $old_size) (i32.const 0))
        (then
          (memory.copy (local.get $ptr) (local.get $old_ptr)
            (if (result i32)
              (i32.lt_u (local.get $old_size) (local.get $new_size))
              (then (local.get $old_size))
              (else (local.get $new_size))))))
      (local.get $ptr))
    (func (export "describe") (result i32)
      (i32.store (i32.const 4096) (i32.const 16))
      (i32.store offset=4 (i32.const 4096) (i32.const {schema_len}))
      (i32.const 4096))
    (func (export "execute")
      (param $ptr i32) (param $len i32) (result i32)
      {body})
  )
  (core instance $lib_inst (instantiate $lib))
  (func (export "describe") (result string)
    (canon lift (core func $lib_inst "describe")
      (memory (core memory $lib_inst "memory"))
      (realloc (core func $lib_inst "cabi_realloc"))))
  (func (export "execute") (param "input" string) (result string)
    (canon lift (core func $lib_inst "execute")
      (memory (core memory $lib_inst "memory"))
      (realloc (core func $lib_inst "cabi_realloc"))))
)"#,
        schema_len = SCHEMA.len(),
    )
    .into_bytes()
}

fn runtime_with(limits: WasmLimits) -> Arc<WasmRuntime> {
    Arc::new(WasmRuntime::with_limits(limits).expect("build engine"))
}

fn echo_wasm() -> Vec<u8> {
    component("", ECHO)
}

/// A component that imports `http-request`, calls it from `execute`, and
/// returns the host's string verbatim.
///
/// The host import's lowered core signature is `(param i32 i32 i32)`:
/// the request `(ptr, len)` plus the return-area pointer. Because a
/// lowered string result needs the caller's memory and `realloc`, the
/// component defines a small memory/allocator module first and feeds the
/// same memory to the main module and to `canon lower` — the canonical
/// ABI's way out of the module/import cycle.
fn http_component() -> Vec<u8> {
    let schema = wat_string(SCHEMA);
    format!(
        r#"(component
  (import "http-request"
    (func $http (param "request" string) (result string)))
  (core module $mem
    (memory (export "memory") 1)
    (global $next (mut i32) (i32.const 8192))
    (func (export "cabi_realloc")
      (param $old_ptr i32) (param $old_size i32)
      (param $align i32) (param $new_size i32)
      (result i32)
      (local $ptr i32)
      (local $mask i32)
      (local.set $mask (i32.sub (local.get $align) (i32.const 1)))
      (local.set $ptr
        (i32.and
          (i32.add (global.get $next) (local.get $mask))
          (i32.xor (local.get $mask) (i32.const -1))))
      (global.set $next (i32.add (local.get $ptr) (local.get $new_size)))
      (if (i32.ne (local.get $old_size) (i32.const 0))
        (then
          (memory.copy (local.get $ptr) (local.get $old_ptr)
            (if (result i32)
              (i32.lt_u (local.get $old_size) (local.get $new_size))
              (then (local.get $old_size))
              (else (local.get $new_size))))))
      (local.get $ptr)))
  (core instance $mem_i (instantiate $mem))
  (core func $http_lower
    (canon lower (func $http)
      (memory (core memory $mem_i "memory"))
      (realloc (core func $mem_i "cabi_realloc"))))
  (core module $lib
    (import "host" "memory" (memory 1))
    (import "host" "http-request" (func $http_call (param i32 i32 i32)))
    (data (i32.const 16) "{schema}")
    (func (export "describe") (result i32)
      (i32.store (i32.const 4096) (i32.const 16))
      (i32.store offset=4 (i32.const 4096) (i32.const {schema_len}))
      (i32.const 4096))
    (func (export "execute") (param $ptr i32) (param $len i32) (result i32)
      (call $http_call (local.get $ptr) (local.get $len) (i32.const 4096))
      (i32.const 4096)))
  (core instance $lib_i
    (instantiate $lib
      (with "host"
        (instance
          (export "memory" (memory $mem_i "memory"))
          (export "http-request" (func $http_lower))))))
  (func (export "describe") (result string)
    (canon lift (core func $lib_i "describe")
      (memory (core memory $mem_i "memory"))
      (realloc (core func $mem_i "cabi_realloc"))))
  (func (export "execute") (param "input" string) (result string)
    (canon lift (core func $lib_i "execute")
      (memory (core memory $mem_i "memory"))
      (realloc (core func $mem_i "cabi_realloc"))))
)"#,
        schema_len = SCHEMA.len(),
    )
    .into_bytes()
}

/// A host extension that supplies `net:http` with a deterministic fake.
#[derive(Debug)]
struct FakeHttpHost;

impl CapabilityHost for FakeHttpHost {
    fn supplies(&self, capability: &str) -> bool {
        capability == "net:http"
    }

    fn link(
        &self,
        imports: &mut HostImports<'_>,
        capability: &str,
        import: &str,
    ) -> Result<(), WasmError> {
        assert_eq!(capability, "net:http");
        assert_eq!(import, "http-request");
        imports.func_wrap_string(import, |request| {
            Ok(format!(r#"{{"echoed":{request}}}"#))
        })
    }
}

#[test]
fn describes_once_and_executes_each_call() {
    let runtime = runtime_with(WasmLimits::default());
    let mount = runtime
        .mount(&echo_wasm(), &[])
        .expect("mount the echo component");

    assert_eq!(mount.describe_calls(), 1, "describe() runs at mount");
    assert_eq!(mount.schema().name, "echo");
    assert_eq!(mount.schema().description, "Echo the JSON input");

    for input in [
        r#"{"value":"one"}"#,
        r#"{"value":"two"}"#,
        r#"{"value":"three"}"#,
    ] {
        let output = mount.execute(input).expect("execute");
        assert_eq!(
            output,
            serde_json::from_str::<Value>(input)
                .expect("fixture input is JSON")
        );
    }

    assert_eq!(mount.describe_calls(), 1, "describe() is never re-called");
    assert_eq!(mount.execute_calls(), 3);
    assert_eq!(
        mount.instance_creations(),
        1,
        "successful calls reuse one store and one instance"
    );
}

#[test]
fn compilation_cache_hits_for_identical_bytes() {
    let runtime = runtime_with(WasmLimits::default());
    let wasm = echo_wasm();

    let first = runtime.mount(&wasm, &[]).expect("first mount");
    let second = runtime.mount(&wasm, &[]).expect("second mount");

    assert_eq!(
        runtime.compilations(),
        1,
        "identical bytes are compiled once"
    );
    assert_eq!(runtime.cached_components(), 1);
    assert_eq!(first.describe_calls(), 1);
    assert_eq!(second.describe_calls(), 1);
    assert_eq!(
        second.execute("{\"value\":1}").expect("execute"),
        json!({"value": 1})
    );
}

#[test]
fn rejects_component_without_granted_capability() {
    let runtime = runtime_with(WasmLimits::default());
    let wasm = component(
        r#"(import "http-request" (func $http_request (param "url" string) (result string)))"#,
        ECHO,
    );

    let error = runtime.mount(&wasm, &[Capability::new("log")]).expect_err(
        "the component imports a capability the manifest does not declare",
    );

    assert_eq!(
        error.kind(),
        WasmErrorKind::MissingCapability,
        "unexpected import rejection: {error}"
    );
    assert_eq!(error.code(), "missing_capability");
    let message = error.to_string();
    assert!(
        message.contains("net:http"),
        "the error names the missing capability: {error}"
    );
    assert!(
        message.contains("http-request"),
        "the error names the rejected import: {error}"
    );
}

#[test]
fn an_unknown_import_fails_at_instantiation_naming_the_import() {
    let runtime = runtime_with(WasmLimits::default());
    let wasm = component(
        r#"(import "mystery-import" (func $mystery (param "x" string) (result string)))"#,
        ECHO,
    );

    let error = runtime
        .mount(&wasm, &[])
        .expect_err("the linker cannot define an unknown import");

    assert_eq!(
        error.kind(),
        WasmErrorKind::UnsatisfiedImport,
        "unexpected instantiation failure: {error}"
    );
    assert_eq!(error.code(), "unsatisfied_import");
    assert!(
        error.to_string().contains("mystery-import"),
        "the error names the import: {error}"
    );
}

#[test]
fn declared_capabilities_are_linked_and_unsupported_ones_are_refused() {
    let runtime = runtime_with(WasmLimits::default());

    let fs_component = component(
        r#"(import "fs-read" (func $fs_read (param "path" string) (result string)))"#,
        ECHO,
    );
    let mount = runtime
        .mount(&fs_component, &[Capability::new("fs:read")])
        .expect("a declared, supplied capability satisfies the import");
    assert!(mount.declared_capabilities().contains("fs:read"));
    assert!(mount.declared_capabilities().contains("log"));
    assert_eq!(
        mount.execute(r#"{"value":"ok"}"#).expect("execute"),
        json!({"value": "ok"})
    );

    let net_component = component(
        r#"(import "http-request" (func $http (param "request" string) (result string)))"#,
        ECHO,
    );
    let error = runtime
        .mount(&net_component, &[Capability::new("net:http")])
        .expect_err("net:http is declared but this host does not supply it");
    assert_eq!(error.kind(), WasmErrorKind::UnsupportedCapability);
    assert!(
        error.to_string().contains("net:http"),
        "the error names the unsupported capability: {error}"
    );
}

#[test]
fn fuel_exhaustion_returns_stable_error() {
    let runtime = runtime_with(WasmLimits {
        fuel: 2_000_000,
        ..WasmLimits::default()
    });
    let mount = runtime
        .mount(&component("", LOOP_FOREVER), &[])
        .expect("mount the looping component");

    let error = mount.execute("{}").expect_err("the loop exhausts its fuel");

    assert_eq!(error.kind(), WasmErrorKind::FuelExhausted);
    assert_eq!(error.code(), "fuel_exhausted");
    assert_eq!(
        mount.execute_calls(),
        0,
        "a trapped call is never counted as a successful execution"
    );
}

#[test]
fn memory_limit_returns_stable_error() {
    let runtime = runtime_with(WasmLimits {
        fuel: 50_000_000,
        max_memory_bytes: 256 * 1024,
        deadline: Duration::from_secs(5),
    });
    let mount = runtime
        .mount(&component("", GROW_UNTIL_REFUSED), &[])
        .expect("mount the growing component");

    let error = mount
        .execute("{}")
        .expect_err("growth past the limit traps");

    assert_eq!(error.kind(), WasmErrorKind::MemoryLimit);
    assert_eq!(error.code(), "memory_limit");
}

#[test]
fn timeout_returns_stable_error() {
    let runtime = runtime_with(WasmLimits {
        fuel: 1_000_000_000_000,
        max_memory_bytes: 64 * 1024 * 1024,
        deadline: Duration::from_millis(50),
    });
    let mount = runtime
        .mount(&component("", LOOP_FOREVER), &[])
        .expect("mount the looping component");

    let started = Instant::now();
    let error = mount.execute("{}").expect_err("the loop is interrupted");
    let elapsed = started.elapsed();

    assert_eq!(error.kind(), WasmErrorKind::Timeout);
    assert_eq!(error.code(), "timeout");
    assert!(
        elapsed >= Duration::from_millis(20),
        "the deadline actually elapsed before the trap: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "the epoch ticker interrupted instead of letting the loop run: {elapsed:?}"
    );
}

#[test]
fn invalid_json_from_execute_returns_stable_error() {
    let runtime = runtime_with(WasmLimits::default());
    let mount = runtime
        .mount(&component("", RETURN_NOT_JSON), &[])
        .expect("mount the component");

    let error = mount.execute("{}").expect_err("the result is not JSON");

    assert_eq!(error.kind(), WasmErrorKind::InvalidJson);
    assert_eq!(error.code(), "invalid_json");
}

#[test]
fn capability_host_supplies_the_declared_net_http_import() {
    let runtime = Arc::new(
        WasmRuntime::with_limits(WasmLimits::default())
            .expect("build engine")
            .with_capability_host(Arc::new(FakeHttpHost)),
    );

    let mount = runtime
        .mount(&http_component(), &[Capability::new("net:http")])
        .expect("the installed host links the declared net:http import");
    let output = mount
        .execute(r#"{"value":7}"#)
        .expect("the guest can call the host-supplied function");

    assert_eq!(output, json!({"echoed": {"value": 7}}));
}

#[test]
fn dropping_the_last_component_evicts_the_compiled_cache_entry() {
    let runtime = runtime_with(WasmLimits::default());
    let wasm = echo_wasm();

    let first = runtime.mount(&wasm, &[]).expect("first mount");
    let second = runtime.mount(&wasm, &[]).expect("second mount");
    assert_eq!(runtime.cached_components(), 1);
    assert_eq!(runtime.compilations(), 1);

    drop(first);
    assert_eq!(
        runtime.cached_components(),
        1,
        "a live mount keeps the compilation cached"
    );
    drop(second);
    assert_eq!(
        runtime.cached_components(),
        0,
        "the last drop evicts the compiled component"
    );
    assert_eq!(runtime.compilations(), 1, "no extra compilation happened");

    let third = runtime.mount(&wasm, &[]).expect("third mount");
    assert_eq!(
        runtime.compilations(),
        2,
        "a mount after eviction recompiles from the bytes"
    );
    assert!(
        runtime.evict(third.content_hash()),
        "explicit eviction removes it"
    );
    assert_eq!(runtime.cached_components(), 0);
}

#[test]
fn bad_bytes_and_core_modules_are_rejected() {
    let runtime = runtime_with(WasmLimits::default());

    let error = runtime
        .mount(b"this is not WebAssembly", &[])
        .expect_err("garbage bytes are not a component");
    assert_eq!(error.kind(), WasmErrorKind::InvalidComponent);

    let error = runtime
        .mount(
            br#"(module (func (export "describe") (result i32) i32.const 0))"#,
            &[],
        )
        .expect_err("a core module is not a component");
    assert_eq!(
        error.kind(),
        WasmErrorKind::InvalidComponent,
        "a core module must not be silently accepted as a component: {error}"
    );
}

#[test]
fn wasm_sandbox_fails_closed_and_passes_danger_full_access() {
    let sandbox = WasmSandbox::new(runtime_with(WasmLimits::default()));
    let root = std::env::temp_dir();

    assert!(
        !sandbox.availability().is_available(),
        "a Wasm sandbox cannot confine an OS process"
    );
    let error = sandbox
        .confine(ExecutionPolicy::ReadOnly, &root)
        .expect_err("a confined policy is refused");
    assert_eq!(error.code(), Some(elysper_tool_shell::SANDBOX_UNAVAILABLE));

    let command = sandbox
        .confine(ExecutionPolicy::DangerFullAccess, &root)
        .expect("the unconfined policy needs no backend");
    assert_eq!(command, elysper_tool_shell::ConfinedCommand::unconfined());
}

#[test]
fn a_trap_poisons_the_pool_and_the_next_call_rebuilds() {
    let runtime = runtime_with(WasmLimits::default());
    let mount = runtime
        .mount(&component("", TRAP_ON_T), &[])
        .expect("mount the trapping fixture");
    assert_eq!(mount.instance_creations(), 1);

    let error = mount.execute("trap-me").expect_err("the guest traps");
    assert_eq!(error.kind(), WasmErrorKind::Trap);
    assert_eq!(mount.execute_calls(), 0, "a trap is not a successful call");

    let output = mount
        .execute(r#"{"ok":1}"#)
        .expect("the next call succeeds on a fresh instance");
    assert_eq!(output, json!({"ok": 1}));
    assert_eq!(
        mount.instance_creations(),
        2,
        "the trapped store is discarded, not resumed"
    );
    assert_eq!(mount.execute_calls(), 1);
}

#[tokio::test]
async fn wasm_plugin_registers_a_tool_and_unload_revokes_it() {
    let ctx = Context::new();
    let tools = Arc::new(ToolRegistry::new());
    let tools_mount = mount_all(
        &ctx,
        &[Arc::new(ToolRegistryPlugin::new(Arc::clone(&tools)))
            as Arc<dyn KernelPlugin>],
    )
    .expect("mount the `tools` service");

    let runtime = runtime_with(WasmLimits::default());
    let mut manifest =
        PluginManifest::new("echo-wasm", PLUGIN_PROTOCOL_VERSION);
    manifest.runtime = RuntimeKind::Wasm;
    manifest.abi = Some(WasmAbi::Json);

    let plugin =
        WasmToolPlugin::new(Arc::clone(&runtime), echo_wasm(), manifest)
            .expect("build the plugin");
    let mounted = mount_all(&ctx, &[Arc::new(plugin) as Arc<dyn KernelPlugin>])
        .expect("mount the Wasm tool plugin");

    assert!(tools.contains("echo"), "the tool is in the registry");
    assert_eq!(runtime.cached_components(), 1);
    let tool = tools
        .get("echo")
        .expect("the registered tool resolves")
        .tool_instance();
    let output = tool
        .call(json!({"value": "hi"}), &ToolContext::default())
        .await;
    assert_eq!(output.is_error, None, "the tool call succeeded");
    let text = output
        .content
        .iter()
        .find_map(|part| match part {
            elysper_provider::types::ContentPart::Text(text) => {
                Some(text.text.clone())
            }
            _ => None,
        })
        .expect("a text result");
    assert_eq!(
        serde_json::from_str::<Value>(&text).expect("the result is JSON"),
        json!({"value": "hi"})
    );
    // The registry hands out an entry clone; the tool stays alive until
    // every holder of it drops.
    drop(tool);

    drop(mounted);
    assert!(
        !tools.contains("echo"),
        "unloading the plugin revokes the tool registration"
    );
    assert_eq!(
        runtime.cached_components(),
        0,
        "unload also evicts the compiled component"
    );
    drop(tools_mount);
    assert!(ctx.require::<ToolRegistry>(KEY_TOOLS).is_err());
}

#[tokio::test]
async fn a_fuel_exhausting_tool_call_reports_the_kind_in_its_metadata() {
    let ctx = Context::new();
    let tools = Arc::new(ToolRegistry::new());
    let tools_mount = mount_all(
        &ctx,
        &[Arc::new(ToolRegistryPlugin::new(Arc::clone(&tools)))
            as Arc<dyn KernelPlugin>],
    )
    .expect("mount the `tools` service");

    let runtime = runtime_with(WasmLimits {
        fuel: 2_000_000,
        ..WasmLimits::default()
    });
    let mut manifest =
        PluginManifest::new("loop-wasm", PLUGIN_PROTOCOL_VERSION);
    manifest.runtime = RuntimeKind::Wasm;
    manifest.abi = Some(WasmAbi::Json);
    let plugin = WasmToolPlugin::new(
        Arc::clone(&runtime),
        component("", LOOP_FOREVER),
        manifest,
    )
    .expect("build the plugin");
    let mounted = mount_all(&ctx, &[Arc::new(plugin) as Arc<dyn KernelPlugin>])
        .expect("mount the looping Wasm tool plugin");

    let tool = tools
        .get("echo")
        .expect("the registered tool resolves")
        .tool_instance();
    let output = tool.call(json!({}), &ToolContext::default()).await;

    assert_eq!(
        output.is_error,
        Some(true),
        "fuel exhaustion is a tool error, not a result"
    );
    assert_eq!(
        output
            .metadata
            .get("wasm_error_kind")
            .and_then(Value::as_str),
        Some("fuel_exhausted"),
        "the tool output carries the stable kind: {:?}",
        output.metadata
    );
    drop(tool);
    drop(mounted);
    drop(tools_mount);
}
