use std::{cmp::Ordering, collections::HashSet, sync::Arc};

use schemars::JsonSchema;
use serde::Serialize;
use uva::core::{
    Error,
    registry::{Registry, RegistryItem},
};

use super::list::{Direction, SortKey};
use crate::{
    api::{List, PageQuery, validate_resource_name},
    rpc::RpcError,
    state::AppState,
};

#[derive(Serialize, JsonSchema)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
}

/// The tool list's sort vocabulary: one key, declared once.
///
/// The key's wire spelling, its comparator and the route's default all
/// live here, so "accepts `name`" and "orders by name" are the same
/// declaration — the arrangement the old whitelist-plus-`match` pair
/// could not guarantee.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolSort {
    Name,
}

impl SortKey<ToolInfo> for ToolSort {
    const ALL: &'static [Self] = &[Self::Name];

    fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
        }
    }

    fn compare(self, a: &ToolInfo, b: &ToolInfo) -> Ordering {
        match self {
            Self::Name => a.name.cmp(&b.name),
        }
    }

    fn default_sort() -> (Self, Direction) {
        (Self::Name, Direction::Ascending)
    }
}

#[derive(Serialize, JsonSchema)]
pub struct ToolDetail {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    /// `core` for tools compiled into the binary, `dynamic`
    /// for tools registered at runtime.
    pub provenance: String,
    /// R17: the tool's canonical rendering contract
    /// (`RenderKind` + title + presentation hints). Consumers
    /// (UI, replay) pick a renderer from this instead of
    /// hard-coding per-tool shapes.
    pub output_definition: uva::tool::ToolOutputDefinition,
}

/// Collect tool definitions from the registry, keyed by the
/// registry's monotonic version. Hit: Arc clone (8-byte
/// refcount) + full Vec deep clone for the caller (the schema
/// `serde_json::Value` clones are unavoidable but only happen on
/// the consumer side, not the registry side). Miss: acquire
/// the registry read lock, walk the descriptors, project them
/// through [`uva::tool::project_tool_definitions`], store
/// under the current version.
///
/// `ToolDefinition::input_schema` is built from `parameters()`,
/// which (for built-in tools) calls `schemars::schema_for!` and
/// `serde_json::to_value` on every invocation — tens of
/// microseconds per tool × N tools per request — so the projected
/// list is cached on the [`AppState`] keyed by the registry's
/// version. Every registry mutation that changes the model-facing
/// list (register, unregister, [`uva::tool::ToolRegistry::set_exposure`],
/// [`uva::tool::ToolRegistry::set_hidden`]) bumps that version,
/// so a cache hit only ever serves the same registry at the same
/// version. The cache lives on the state (not a process-wide
/// static) so two servers in one process cannot read each other's
/// list.
///
/// The projection is the same seam the agent loop uses, so this
/// endpoint reports what the model would be offered: `Direct` tools
/// with their full schema, `Deferred` tools with the placeholder
/// until a call promotes them, and `Hidden` / `is_hidden` tools
/// nowhere. There is no transcript here, so no tool is ever
/// promoted on this path; `max_visible` is likewise not applied —
/// the cap belongs to one agent request, not to the catalog.
async fn collect_tool_defs(
    state: &Arc<AppState>,
) -> Arc<Vec<uva::provider::ToolDefinition>> {
    let version = state.tool_registry.version();
    {
        let guard = state.tool_defs_cache.read();
        if let Some((v, cached)) = guard.as_ref()
            && *v == version
        {
            return Arc::clone(cached);
        }
    }
    let descriptors = state.tool_registry.descriptors();
    // Self-management MCP tools (`mcp__self__*`) are server-facing
    // (operators reach them via MCP), not model-facing. The HTTP
    // `/api/tools` catalog follows the same contract as the agent
    // loop's tool projection: hide them so an operator
    // browsing the tool surface is not presented with nine
    // server-management entries that the LLM cannot call.
    let operator_facing: Vec<uva::tool::ToolDescriptor> = descriptors
        .into_iter()
        .filter(|d| !uva::tool::is_mcp_self_tool(&d.name))
        .collect();
    let defs: Vec<uva::provider::ToolDefinition> =
        uva::tool::project_tool_definitions(
            &operator_facing,
            &HashSet::new(),
            None,
        );
    let arc = Arc::new(defs);
    *state.tool_defs_cache.write() = Some((version, Arc::clone(&arc)));
    arc
}

/// GET /api/v1/tools - List registered tools.
///
/// The catalog carries the name and the description; the schema the
/// projection also holds is not part of the response, so the cached
/// list is read by reference rather than copied. The order, the page
/// and the envelope come from the route's sort key.
pub async fn list_tools(
    state: Arc<AppState>,
    page: PageQuery,
) -> Result<List<ToolInfo>, RpcError> {
    let defs = collect_tool_defs(&state).await;
    let tools: Vec<ToolInfo> = defs
        .iter()
        .map(|d| ToolInfo {
            name: d.name.clone(),
            description: d.description.clone(),
        })
        .collect();

    Ok(ToolSort::sort_and_page(tools, &page, |t| t.name.as_str())?)
}

// POST /api/tools - Register a tool.
//
// Restored in turn 13 of the 2026-08-15 optimization pass to
// address Task 3 of the active goal ("实现skill.tool、agent、
// model的全生命周期管理"). Tools live in the in-memory
// `tool_registry` and survive until the server restarts; the
// dynamic registration accepts a name + description + JSON
// schema and registers a passthrough tool that echoes its
// arguments back.
pub async fn register_tool(
    state: Arc<AppState>,
    req: RegisterToolRequest,
) -> Result<ToolInfo, RpcError> {
    validate_resource_name(&req.name)?;

    let desc = req.description.clone();
    let params = req.input_schema.clone();
    let tool = uva::tool::ToolEntry::dynamic(
        req.name.clone(),
        desc.clone(),
        params.clone(),
    );

    // One write guard decides and inserts: reading the name and then
    // registering it let two concurrent requests for one name both
    // pass the check, and the second would shadow the first. On a
    // cluster the other nodes learn of this registration through the
    // mutation below, exactly as they learn of it on the local path.
    if !state.tool_registry.register_entry_if_absent(tool) {
        return Err(RpcError::from(Error::already_exists(format!(
            "tool '{}'",
            req.name
        ))));
    }
    // Tell the other nodes; a local bus publishes into the process
    // (the dispatcher skips our own origin, the local apply already
    // happened).
    publish_tool_mutation(
        &state,
        crate::cluster::ToolMutation::RegisterDynamic {
            name: req.name.clone(),
            description: desc.clone(),
            parameters: params.clone(),
        },
    )
    .await;

    Ok(ToolInfo {
        name: req.name,
        description: desc,
    })
}

/// Request body for `POST /api/v1/tools`.
#[derive(serde::Deserialize, validator::Validate)]
pub struct RegisterToolRequest {
    #[validate(length(min = 1, message = "must not be empty"))]
    pub name: String,
    #[validate(length(min = 1, message = "must not be empty"))]
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// GET /api/v1/tools/{name} - Get a single tool.
pub async fn get_tool(
    state: Arc<AppState>,
    name: String,
) -> Result<ToolDetail, RpcError> {
    validate_resource_name(&name)?;
    let tool_reg = state.tool_registry.as_ref();
    // O(1) lookup against the registry's `HashMap<String, Vec<ToolEntry>>`
    // instead of the previous O(n) `list()` + linear search that
    // cloned every `ToolDefinition` (including the JSON schema)
    // just to find the one named entry. The previous path wasted
    // work on every detail-page render and the front-end's per-
    // detail-page prefetch.
    let entry = tool_reg.get(&name).ok_or_else(|| {
        RpcError::from(Error::not_found(format!("tool '{name}'")))
    })?;
    // Provenance isn't on `ToolEntry`; the registry exposes it via
    // a snapshot scan. The snapshot is cheap (one read-lock, single
    // pass over an in-memory `HashMap`) and much smaller than the
    // full `list()` payload the previous code built — we just need
    // the one matching record.
    let snap = tool_reg.snapshot_with_provenance();
    let provenance = snap
        .iter()
        .find(|r| r.metadata.name == name)
        .map(|r| match r.provenance {
            uva::tool::registry::ToolProvenance::Core => "core",
            uva::tool::registry::ToolProvenance::Dynamic => "dynamic",
        })
        .unwrap_or("dynamic")
        .to_string();
    let description = entry.description().to_string();
    let input_schema = entry.tool_instance().parameters();
    let output_definition = entry.tool_instance().output_definition();
    Ok(ToolDetail {
        name: entry.name().to_string(),
        description,
        input_schema,
        provenance,
        output_definition,
    })
}

// DELETE /api/tools/{name} - Unregister a tool.
//
// Restored in turn 13 of the 2026-08-15 optimization pass.
// Returns `404 Not Found` if the tool is not registered.
//
// A tool the binary **ships** is not unregisterable: the core
// immutability rule says a core-owned name cannot be taken, and
// deleting it would free the name for a plugin to claim — the very
// substitution the rule exists to prevent. Such a delete is refused
// with `409 Conflict`.
pub async fn unregister_tool(
    state: Arc<AppState>,
    name: String,
) -> Result<ToolInfo, RpcError> {
    validate_resource_name(&name)?;
    {
        let reg = state.tool_registry.as_ref();
        let entries = reg.list(None).await.unwrap_or_default();
        let target =
            entries.into_iter().find(|e| e.name() == name).ok_or_else(
                || RpcError::from(Error::not_found(format!("tool '{name}'"))),
            )?;
        let description = target.description().to_string();
        if reg.core_owns(&name) {
            return Err(RpcError::from(Error::already_exists(format!(
                "tool '{name}' is shipped by the binary and cannot be \
                 unregistered"
            ))));
        }
        reg.delete(&name).await.map_err(|e| {
            Error::internal(format!("failed to unregister tool: {e}"))
        })?;
        publish_tool_mutation(
            &state,
            crate::cluster::ToolMutation::Remove { name: name.clone() },
        )
        .await;
        Ok(ToolInfo { name, description })
    }
}

/// Publish one tool-registry mutation for peer nodes. Best effort:
/// a publish failure leaves peers with a stale view until their next
/// mutation or restart, which the bus contract accepts.
async fn publish_tool_mutation(
    state: &AppState,
    mutation: crate::cluster::ToolMutation,
) {
    let frame = crate::cluster::ControlFrame::InvalidateTools {
        origin: state.cluster.node_id.clone(),
        mutation,
    };
    if let Err(e) = state.cluster.bus.publish_control(frame).await {
        tracing::warn!(error = %e, "cluster: tool mutation publish failed");
    }
}

#[cfg(test)]
mod tests;
