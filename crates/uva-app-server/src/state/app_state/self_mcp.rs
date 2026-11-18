//! The self-management MCP server: the deployment's own
//! management surface, served to its agents over MCP.
//!
//! Everything the HTTP API can do to the deployment — agents,
//! tools, skills, schedules, evals, tasks, models,
//! attachments — is also an in-process MCP server, so **the
//! agent can manage itself**: register a peer, reload a skill,
//! pause a schedule, or inspect its own task fan-out without a
//! human driving the UI.
//!
//! Deliberately excluded: sessions and messages. Those are the
//! conversation surface itself — an agent writing into its own
//! transcript (or another live one) has no self-management
//! meaning and every foot-gun.
//!
//! ## Shape
//!
//! One MCP tool per domain, action-discriminated (`{"action":
//! "list" | "get" | …}`) — the same shape the `mcp` control tool
//! and the `schedule` tool established. Ten tools total,
//! registered under the server name `self`, so the naming
//! policy exposes them as `mcp__self__agents`,
//! `mcp__self__skills`, … and the boot-time `mcp__*` flip
//! gives them `Deferred` exposure (name + description at cold
//! start; the schema promotes on first call).
//!
//! ## Wiring
//!
//! [`register_self_mcp`] runs after `AppState` exists: it spawns
//! [`uva::mcp::serve`] over a
//! [`loopback_pair`](uva::mcp::loopback_pair), handshakes a
//! plain [`McpClient`], and publishes the tools through
//! [`register_mcp_tools`] — the identical path stdio servers
//! take, minus the process.

use std::sync::Arc;

use serde_json::{Value, json};
use uva::{
    core::{
        agent::AgentDescriptor,
        registry::{Registry as _, RegistryItem as _},
    },
    mcp::{
        McpClient,
        NamingPolicy,
        ServerInfo,
        ServerTool,
        loopback_pair,
        register_mcp_tools,
        serve,
    },
    tool::ToolAnnotations,
};

use crate::{
    profile::{CapabilityAction, CapabilityDomain, ProfileAction, RowEdit},
    state::AppState,
};

/// Server name the tools register under (`mcp__self__<tool>`,
/// namespaced via [`uva::tool::MCP_SELF_TOOL_PREFIX`]).
const SELF_SERVER_NAME: &str = "self";

/// Spawn the self-management MCP server and publish its tools.
///
/// Failure is logged, never fatal: a deployment whose agent
/// cannot manage itself still serves chats.
pub(crate) async fn register_self_mcp(state: &Arc<AppState>) {
    let (client_transport, server_transport) = loopback_pair(SELF_SERVER_NAME);
    let tools = self_mcp_tools(Arc::clone(state));
    tokio::spawn(serve(
        server_transport,
        ServerInfo::new(SELF_SERVER_NAME, env!("CARGO_PKG_VERSION")),
        tools,
    ));
    let client = McpClient::new(client_transport);
    if let Err(error) = client.initialize().await {
        tracing::warn!(
            %error,
            "self-management MCP server failed its handshake"
        );
        return;
    }
    let registry = state.tool_registry.as_ref();
    match register_mcp_tools(
        registry,
        client,
        SELF_SERVER_NAME,
        NamingPolicy::default(),
    )
    .await
    {
        Ok(generation) => tracing::info!(
            tools = generation.len(),
            "self-management MCP server registered"
        ),
        Err(error) => tracing::warn!(
            %error,
            "self-management MCP tools could not be registered"
        ),
    }
    // Same cold-start policy as every other `mcp__*` tool:
    // name + description up front, the schema on first call.
    for descriptor in registry.descriptors().iter().filter(|d| {
        uva::tool::is_mcp_self_tool(&d.name)
            && d.exposure == uva::tool::ToolExposure::Direct
    }) {
        registry
            .set_exposure(&descriptor.name, uva::tool::ToolExposure::Deferred);
    }
}

/// Build the nine domain tools over one shared state handle.
fn self_mcp_tools(state: Arc<AppState>) -> Vec<ServerTool> {
    vec![
        agents_tool(Arc::clone(&state)),
        tools_tool(Arc::clone(&state)),
        skills_tool(Arc::clone(&state)),
        schedules_tool(Arc::clone(&state)),
        evals_tool(Arc::clone(&state)),
        tasks_tool(Arc::clone(&state)),
        models_tool(Arc::clone(&state)),
        attachments_tool(Arc::clone(&state)),
        profile_tool(state),
    ]
}

// --- shared helpers --------------------------------------------------

fn action_of(args: &Value) -> String {
    args.get("action")
        .and_then(Value::as_str)
        .unwrap_or("list")
        .to_string()
}

fn str_field(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value)
        .unwrap_or_else(|_| format!("{{\"unserializable\": {value}}}"))
}

// --- agents ------------------------------------------------------------

fn agents_tool(state: Arc<AppState>) -> ServerTool {
    ServerTool::new(
        "agents",
        "Manage this deployment's agent registry: list, get, create \
         (full AgentDescriptor), or delete registered peers. The \
         `task` tool can delegate to any registered agent.",
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "get", "create", "delete"],
                    "description": "Defaults to list."
                },
                "name": {"type": "string"},
                "descriptor": {
                    "type": "object",
                    "description": "Full AgentDescriptor for create."
                },
            },
            "required": ["action"],
        }),
        move |args| {
            let state = Arc::clone(&state);
            async move {
                match action_of(&args).as_str() {
                    "list" => {
                        let entries = state
                            .agent_registry
                            .list(None)
                            .await
                            .map_err(|e| e.to_string())?;
                        let rows: Vec<&AgentDescriptor> = entries
                            .iter()
                            .map(|entry| entry.descriptor())
                            .collect();
                        Ok(pretty(&json!(rows)))
                    }
                    "get" => {
                        let name = str_field(&args, "name")
                            .ok_or("get requires `name`")?;
                        let entry = state
                            .agent_registry
                            .get(&name)
                            .await
                            .map_err(|e| e.to_string())?
                            .ok_or_else(|| {
                                format!("agent '{name}' not found")
                            })?;
                        Ok(pretty(&json!(entry.descriptor())))
                    }
                    "create" => {
                        let descriptor: AgentDescriptor =
                            serde_json::from_value(
                                args.get("descriptor")
                                    .cloned()
                                    .ok_or("create requires `descriptor")?,
                            )
                            .map_err(|e| format!("descriptor: {e}"))?;
                        create_agent(&state, descriptor).await
                    }
                    "delete" => {
                        let name = str_field(&args, "name")
                            .ok_or("delete requires `name`")?;
                        if crate::routes::agents::is_protected(&name) {
                            return Err(format!("agent '{name}' is protected"));
                        }
                        state
                            .agent_registry
                            .delete(&name)
                            .await
                            .map_err(|e| e.to_string())?;
                        Ok(format!("deleted agent '{name}'"))
                    }
                    other => Err(format!("unknown action: {other}")),
                }
            }
        },
    )
}

/// The create path of `POST /api/v1/agents`, minus the HTTP
/// envelope: same protected/cap/empty-description gates, same
/// runtime assembly.
async fn create_agent(
    state: &Arc<AppState>,
    descriptor: AgentDescriptor,
) -> Result<String, String> {
    if descriptor.description.trim().is_empty() {
        return Err("description must not be empty".to_string());
    }
    if crate::routes::agents::is_protected(&descriptor.name) {
        return Err(format!(
            "agent '{}' is protected and cannot be re-registered",
            descriptor.name
        ));
    }
    let registered = state.agent_registry.names().len();
    if registered >= state.max_agents()
        && state
            .agent_registry
            .resolve_sync(&descriptor.name)
            .is_none()
    {
        return Err(format!(
            "agent limit reached ({registered}/{})",
            state.max_agents()
        ));
    }
    let agent = crate::routes::agents::build_react_agent(
        state,
        descriptor.clone(),
        descriptor.max_iterations,
    )
    .await
    .map_err(|e| e.to_string())?;
    let entry = uva::agent::AgentEntry::new(agent);
    state
        .agent_registry
        .put(entry)
        .await
        .map_err(|e| e.to_string())?;
    Ok(format!("registered agent '{}'", descriptor.name))
}

// --- tools -------------------------------------------------------------

fn tools_tool(state: Arc<AppState>) -> ServerTool {
    ServerTool::new(
        "tools",
        "Inspect and extend the shared tool registry: list every \
         registered tool (name, description, exposure), fetch one \
         tool's parameter schema, register a dynamic passthrough \
         tool, or unregister one. A dynamic tool echoes its input \
         — it is a name/description/schema placeholder, not an \
         execution surface.",
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "get", "register", "unregister"],
                    "description": "Defaults to list."
                },
                "name": {"type": "string"},
                "description": {"type": "string"},
                "input_schema": {
                    "type": "object",
                    "description": "JSON Schema for register."
                },
            },
            "required": ["action"],
        }),
        move |args| {
            let state = Arc::clone(&state);
            async move {
                match action_of(&args).as_str() {
                    "list" => {
                        let registry = state.tool_registry.as_ref();
                        let rows: Vec<Value> = registry
                            .descriptors()
                            .iter()
                            .map(|d| {
                                json!({
                                    "name": d.name,
                                    "description": d.description,
                                    "exposure": format!("{:?}", d.exposure),
                                })
                            })
                            .collect();
                        Ok(pretty(&json!(rows)))
                    }
                    "get" => {
                        let name = str_field(&args, "name")
                            .ok_or("get requires `name`")?;
                        let registry = state.tool_registry.as_ref();
                        let entry = registry.get(&name).ok_or_else(|| {
                            format!("tool '{name}' not found")
                        })?;
                        Ok(pretty(&json!({
                            "name": name,
                            "description": entry.description(),
                            "parameters": entry.tool_instance()
                                .parameters(),
                        })))
                    }
                    "register" => {
                        let name = str_field(&args, "name")
                            .ok_or("register requires `name`")?;
                        let description =
                            str_field(&args, "description").unwrap_or_default();
                        let schema = args
                            .get("input_schema")
                            .cloned()
                            .unwrap_or_else(|| json!({"type": "object"}));
                        let registry = state.tool_registry.as_ref();
                        if registry.get(&name).is_some() {
                            return Err(format!(
                                "tool '{name}' already exists"
                            ));
                        }
                        let entry = uva::tool::ToolEntry::dynamic(
                            name.clone(),
                            description,
                            schema,
                        );
                        registry.put(entry).await.map_err(|e| e.to_string())?;
                        Ok(format!("registered tool '{name}'"))
                    }
                    "unregister" => {
                        let name = str_field(&args, "name")
                            .ok_or("unregister requires `name`")?;
                        let registry = state.tool_registry.as_ref();
                        if registry.get(&name).is_none() {
                            return Err(format!("tool '{name}' not found"));
                        }
                        registry
                            .delete(&name)
                            .await
                            .map_err(|e| e.to_string())?;
                        Ok(format!("unregistered tool '{name}'"))
                    }
                    other => Err(format!("unknown action: {other}")),
                }
            }
        },
    )
}

fn skills_tool(state: Arc<AppState>) -> ServerTool {
    ServerTool::new(
        "skills",
        "Read and manage the workspace skill catalog \
         (.agents/skills/<name>/SKILL.md): list with descriptions, \
         read one skill's full markdown, create a new skill, or \
         delete one.",
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "get", "create", "delete"],
                    "description": "Defaults to list."
                },
                "name": {"type": "string"},
                "content": {
                    "type": "string",
                    "description": "Full SKILL.md body for create."
                },
            },
            "required": ["action"],
        }),
        move |args| {
            let state = Arc::clone(&state);
            async move {
                let root = state.workspace_root.join(".agents").join("skills");
                // Skill files are read and written on the blocking
                // pool: a create/delete on a slow filesystem must not
                // stall the runtime that is also serving every other
                // session's stream.
                tokio::task::spawn_blocking(move || skill_action(&root, &args))
                    .await
                    .map_err(|e| format!("skills task failed: {e}"))?
            }
        },
    )
}

/// One `skills` action, executed on the blocking pool.
fn skill_action(
    root: &std::path::Path,
    args: &serde_json::Value,
) -> Result<String, String> {
    match action_of(args).as_str() {
        "list" => {
            let mut rows = Vec::new();
            let entries = std::fs::read_dir(root).map_err(|e| e.to_string())?;
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                let Ok(content) =
                    std::fs::read_to_string(entry.path().join("SKILL.md"))
                else {
                    continue;
                };
                rows.push(json!({
                    "name": name,
                    "description":
                        crate::routes::skills::skill_description(&content),
                }));
            }
            rows.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
            Ok(pretty(&json!(rows)))
        }
        "get" => {
            let name = str_field(args, "name").ok_or("get requires `name`")?;
            let content =
                std::fs::read_to_string(root.join(&name).join("SKILL.md"))
                    .map_err(|_| format!("skill '{name}' not found"))?;
            Ok(content)
        }
        "create" => {
            let name =
                str_field(args, "name").ok_or("create requires `name`")?;
            let content = str_field(args, "content")
                .ok_or("create requires `content`")?;
            let dir = root.join(&name);
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(dir.join("SKILL.md"))
                .and_then(|mut file| {
                    use std::io::Write as _;
                    file.write_all(content.as_bytes())
                })
                .map_err(|e| format!("skill '{name}' exists: {e}"))?;
            crate::routes::skills::invalidate_skill_cache();
            Ok(format!("created skill '{name}'"))
        }
        "delete" => {
            let name =
                str_field(args, "name").ok_or("delete requires `name`")?;
            std::fs::remove_dir_all(root.join(&name))
                .map_err(|e| e.to_string())?;
            crate::routes::skills::invalidate_skill_cache();
            Ok(format!("deleted skill '{name}'"))
        }
        other => Err(format!("unknown action: {other}")),
    }
}

// --- schedules ----------------------------------------------------------

fn schedules_tool(state: Arc<AppState>) -> ServerTool {
    ServerTool::new(
        "schedules",
        "Manage scheduled jobs (the `schedule` tool's store): list, \
         get, create (full Job JSON), pause, resume, delete, or \
         fire the due ones with tick.",
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "get", "create", "pause",
                             "resume", "delete", "tick"],
                    "description": "Defaults to list."
                },
                "id": {"type": "string"},
                "job": {
                    "type": "object",
                    "description": "Full Job object for create."
                },
            },
            "required": ["action"],
        }),
        move |args| {
            let state = Arc::clone(&state);
            async move {
                match action_of(&args).as_str() {
                    "list" => Ok(pretty(&json!(state.schedules.list()))),
                    "get" => {
                        let id = str_field(&args, "id")
                            .ok_or("get requires `id`")?;
                        let job = state
                            .schedules
                            .get(&uva::scheduler::JobId::from_string(&id));
                        job.map(|job| pretty(&json!(job)))
                            .ok_or_else(|| format!("job '{id}' not found"))
                    }
                    "create" => {
                        let job: uva::scheduler::Job = serde_json::from_value(
                            args.get("job")
                                .cloned()
                                .ok_or("create requires `job`")?,
                        )
                        .map_err(|e| format!("job: {e}"))?;
                        let label = job.id.to_string();
                        state.schedules.add(job)?;
                        Ok(format!("scheduled job '{label}'"))
                    }
                    "pause" | "resume" => {
                        let id = str_field(&args, "id")
                            .ok_or("pause/resume requires `id`")?;
                        let status = if action_of(&args) == "pause" {
                            uva::scheduler::JobStatus::Paused
                        } else {
                            uva::scheduler::JobStatus::Active
                        };
                        state
                            .schedules
                            .update(
                                &uva::scheduler::JobId::from_string(&id),
                                |job| job.status = status,
                            )
                            .map_err(|e| e.to_string())?
                            .map(|job| pretty(&json!(job)))
                            .ok_or_else(|| format!("job '{id}' not found"))
                    }
                    "delete" => {
                        let id = str_field(&args, "id")
                            .ok_or("delete requires `id`")?;
                        state
                            .schedules
                            .remove(&uva::scheduler::JobId::from_string(&id))?
                            .then(|| format!("deleted job '{id}'"))
                            .ok_or_else(|| format!("job '{id}' not found"))
                    }
                    "tick" => {
                        let fired: Vec<Value> = state
                            .schedules
                            .tick()
                            .iter()
                            .map(|job| {
                                json!({
                                    "id": job.id.to_string(),
                                    "name": job.name,
                                })
                            })
                            .collect();
                        Ok(pretty(&json!(fired)))
                    }
                    other => Err(format!("unknown action: {other}")),
                }
            }
        },
    )
}

// --- evals ---------------------------------------------------------------

fn evals_tool(state: Arc<AppState>) -> ServerTool {
    ServerTool::new(
        "evals",
        "Manage eval suites: list, get, create (full EvalSuite \
         JSON), delete, or run one (deterministic keyword metric \
         against a stub agent — the same engine the HTTP route \
         uses).",
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "get", "create", "delete", "run"],
                    "description": "Defaults to list."
                },
                "name": {"type": "string"},
                "suite": {
                    "type": "object",
                    "description": "Full EvalSuite object for create."
                },
            },
            "required": ["action"],
        }),
        move |args| {
            let state = Arc::clone(&state);
            async move {
                match action_of(&args).as_str() {
                    "list" => Ok(pretty(&json!(state.evals.list().await))),
                    "get" => {
                        let name = str_field(&args, "name")
                            .ok_or("get requires `name`")?;
                        state
                            .evals
                            .get(&name)
                            .await
                            .map(|suite| pretty(&json!(suite)))
                            .ok_or_else(|| {
                                format!("eval suite '{name}' not found")
                            })
                    }
                    "create" => {
                        let suite: uva::eval::EvalSuite =
                            serde_json::from_value(
                                args.get("suite")
                                    .cloned()
                                    .ok_or("create requires `suite`")?,
                            )
                            .map_err(|e| format!("suite: {e}"))?;
                        let label = suite.name().to_string();
                        state.evals.create(suite).await?;
                        Ok(format!("stored eval suite '{label}'"))
                    }
                    "delete" => {
                        let name = str_field(&args, "name")
                            .ok_or("delete requires `name`")?;
                        state
                            .evals
                            .remove(&name)
                            .await
                            .map_err(|e| e.to_string())?
                            .then(|| format!("deleted eval suite '{name}'"))
                            .ok_or_else(|| {
                                format!("eval suite '{name}' not found")
                            })
                    }
                    "run" => {
                        let name = str_field(&args, "name")
                            .ok_or("run requires `name`")?;
                        let factory = state.agent_loop().ok_or_else(|| {
                            "the `agentLoop` service is not mounted; eval \
                             runs are unavailable"
                                .to_string()
                        })?;
                        let agent = super::eval_agent::HarnessEvalAgent::new(
                            factory,
                            state.default_provider.read().clone(),
                            Arc::clone(&state.tool_registry),
                            state.plugin_host.clone(),
                            state.system_prompt.clone(),
                            state.workspace_root.clone(),
                        );
                        let report = state
                            .evals
                            .run(&name, &agent)
                            .await
                            .map_err(|e| e.to_string())?;
                        Ok(pretty(&json!(report)))
                    }
                    other => Err(format!("unknown action: {other}")),
                }
            }
        },
    )
}

// --- tasks ---------------------------------------------------------------

fn tasks_tool(state: Arc<AppState>) -> ServerTool {
    ServerTool::new(
        "tasks",
        "List this deployment's delegated sub-agent tasks (the \
         `task` tool's fan-out): each row is one delegation with \
         its parent/child session, agent, prompt, and status — \
         the same list the Tasks page renders.",
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "get"],
                    "description": "Defaults to list."
                },
                "id": {"type": "string"},
            },
            "required": ["action"],
        }),
        move |args| {
            let state = Arc::clone(&state);
            async move {
                match action_of(&args).as_str() {
                    "list" => {
                        let user_id = state.default_user_id().to_string();
                        let rows = crate::routes::tasks::delegations_of_user(
                            &state, &user_id,
                        )
                        .await;
                        Ok(pretty(&json!(rows)))
                    }
                    "get" => {
                        let id = str_field(&args, "id")
                            .ok_or("get requires `id`")?;
                        let user_id = state.default_user_id().to_string();
                        let rows = crate::routes::tasks::delegations_of_user(
                            &state, &user_id,
                        )
                        .await;
                        rows.iter()
                            .find(|row| row.id == id)
                            .map(|row| pretty(&json!(row)))
                            .ok_or_else(|| format!("task '{id}' not found"))
                    }
                    other => Err(format!("unknown action: {other}")),
                }
            }
        },
    )
}

// --- models ----------------------------------------------------------------

fn models_tool(state: Arc<AppState>) -> ServerTool {
    ServerTool::new(
        "models",
        "List the providers and models this deployment \
         configures (from the workspace config), including which \
         one is the default.",
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list"],
                    "description": "Defaults to list."
                },
            },
            "required": ["action"],
        }),
        move |args| {
            let state = Arc::clone(&state);
            async move {
                if action_of(&args) != "list" {
                    return Err("unknown action".to_string());
                }
                let config = state.workspace_config.read();
                let mut rows = Vec::new();
                for (name, entry) in &config.providers {
                    rows.push(json!({
                        "provider": name,
                        "model": entry.default_model
                            .clone()
                            .unwrap_or_else(|| "unknown".into()),
                        "context_window": entry.context_window
                            .unwrap_or(128_000),
                        "supports_tools": entry.supports_tools
                            .unwrap_or(true),
                        "default": name == &config.default_provider,
                    }));
                }
                Ok(pretty(&json!({
                    "models": rows,
                    "default_provider": config.default_provider,
                    "default_model": config.default_model,
                })))
            }
        },
    )
}

// --- attachments --------------------------------------------------------

fn attachments_tool(state: Arc<AppState>) -> ServerTool {
    ServerTool::new(
        "attachments",
        "Inspect the content-addressed attachment store: store an \
         image (base64 + mime), check a hash's byte length, or \
         evict one. Bytes are not returned — a base64 blob has no \
         business in the model context.",
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "get", "delete"],
                },
                "hash": {"type": "string"},
                "data_base64": {"type": "string"},
                "mime_type": {"type": "string"},
            },
            "required": ["action"],
        }),
        move |args| {
            let state = Arc::clone(&state);
            async move {
                match action_of(&args).as_str() {
                    "create" => {
                        let data = str_field(&args, "data_base64")
                            .ok_or("create requires `data_base64`")?;
                        let mime = str_field(&args, "mime_type")
                            .ok_or("create requires `mime_type`")?;
                        use base64::Engine as _;
                        let bytes = base64::engine::general_purpose::STANDARD
                            .decode(&data)
                            .map_err(|e| format!("base64: {e}"))?;
                        let saved = state
                            .attachment_store
                            .save_image(&bytes, &mime)
                            .map_err(|e| e.to_string())?;
                        Ok(pretty(&json!({
                            "hash": saved.hash,
                            "mime_type": saved.mime_type,
                            "byte_len": saved.byte_len,
                        })))
                    }
                    "get" => {
                        let hash = str_field(&args, "hash")
                            .ok_or("get requires `hash`")?;
                        let bytes = state
                            .attachment_store
                            .read_base64_raw(&hash)
                            .map_err(|e| e.to_string())?;
                        Ok(json!({
                            "hash": hash,
                            "byte_len": bytes.len(),
                        })
                        .to_string())
                    }
                    "delete" => {
                        let hash = str_field(&args, "hash")
                            .ok_or("delete requires `hash`")?;
                        state.attachment_store.evict(&hash);
                        Ok(format!("evicted attachment '{hash}'"))
                    }
                    other => Err(format!("unknown action: {other}")),
                }
            }
        },
    )
}

// --- profile ------------------------------------------------------------

/// The profile domain: the settings surface's read model plus the one
/// transactional write path.
///
/// The whole tool carries MCP's `destructiveHint`, because the `set` /
/// `create` / `delete` actions write the document and the tree. MCP's
/// annotations are per tool, not per action, so the read actions are
/// held by the same approval as the writes — a conservative upper
/// bound, never a way around the stance.
fn profile_tool(state: Arc<AppState>) -> ServerTool {
    ServerTool::new(
        "profile",
        "Read and change this deployment's plugin profile: list every \
         row with its effective status, get one row, or set a row's \
         disabled bit / config. Create/delete agent and skill \
         definitions through the same transaction. Writes are \
         approval-gated: the tool declares MCP's destructiveHint, so \
         the call is Risk::High.",
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "get", "set", "create", "delete"],
                    "description": "Defaults to list."
                },
                "id": {"type": "string"},
                "disabled": {
                    "type": "boolean",
                    "description": "Whether the row stays out of the assembly."
                },
                "config": {
                    "type": "object",
                    "description": "Replacement config, whole."
                },
                "domain": {
                    "type": "string",
                    "enum": ["agent", "skill"],
                },
                "definition": {
                    "type": "object",
                    "description": "Agent descriptor or {name, content}."
                },
            },
            "required": ["action"],
        }),
        move |args| {
            let state = Arc::clone(&state);
            async move {
                match action_of(&args).as_str() {
                    "list" => Ok(pretty(&json!(state.profile_rows()))),
                    "get" => {
                        let id = str_field(&args, "id")
                            .ok_or("get requires `id`")?;
                        state
                            .profile_row(&id)
                            .map(|row| pretty(&json!(row)))
                            .ok_or_else(|| {
                                format!("the profile has no row `{id}`")
                            })
                    }
                    "set" => {
                        let id = str_field(&args, "id")
                            .ok_or("set requires `id`")?;
                        let disabled = match args.get("disabled") {
                            None => None,
                            Some(value) => Some(
                                serde_json::from_value::<bool>(value.clone())
                                    .map_err(|error| {
                                    format!("disabled: {error}")
                                })?,
                            ),
                        };
                        let config = args.get("config").cloned();
                        if disabled.is_none() && config.is_none() {
                            return Err(
                                "set requires `disabled` and/or `config`"
                                    .to_string(),
                            );
                        }
                        let outcome = state
                            .profile_apply(
                                "agent",
                                ProfileAction::Edit(RowEdit::Set {
                                    id,
                                    disabled,
                                    config,
                                }),
                            )
                            .await;
                        Ok(pretty(&json!(outcome)))
                    }
                    "create" | "delete" => {
                        let domain = capability_domain(&args)?;
                        let action = if action_of(&args) == "create" {
                            CapabilityAction::Create {
                                domain,
                                definition: args
                                    .get("definition")
                                    .cloned()
                                    .ok_or("create requires `definition`")?,
                            }
                        } else {
                            CapabilityAction::Delete {
                                domain,
                                id: str_field(&args, "id")
                                    .ok_or("delete requires `id`")?,
                            }
                        };
                        let outcome = state
                            .profile_apply(
                                "agent",
                                ProfileAction::Capability(action),
                            )
                            .await;
                        Ok(pretty(&json!(outcome)))
                    }
                    other => Err(format!("unknown action: {other}")),
                }
            }
        },
    )
    .with_annotations(ToolAnnotations {
        destructive_hint: Some(true),
        ..ToolAnnotations::default()
    })
}

fn capability_domain(args: &Value) -> Result<CapabilityDomain, String> {
    serde_json::from_value(
        args.get("domain")
            .cloned()
            .ok_or("create/delete require `domain`")?,
    )
    .map_err(|error| format!("domain: {error}"))
}
