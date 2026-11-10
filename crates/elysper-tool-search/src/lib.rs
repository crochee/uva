//! # elysper-tool-search
//!
//! The `search` tool — and nothing else: one [`Tool`] implementation
//! that fans a query out across every engine registered in a
//! [`Registry`], i.e. the tool / mcp / skill / memory / agent
//! catalogs the host has indexed, and returns the agent-facing
//! projection (domain, id, title, why, score) best match first.
//!
//! ## Cross-domain by construction
//!
//! A [`Registry`] is keyed by the *domain label* the host picks at
//! registration time, so the host builds one registry, registers one
//! `SearchEngine<Tool>` under `"tool"`, one `SearchEngine<Skill>`
//! under `"skill"`, and adapter engines for the async retrievers
//! (session full-text, memory recall) — this crate needs no
//! knowledge of the domains: [`Registry::search`](Registry::search)
//! merges every engine's hits, stamps each with its domain label
//! and [`elysper_search::agent_view`] renders them. Teaching the
//! tool a new domain is the host's `register` call, not a change
//! here — the same seam the `Registry` doc-comment describes.
//!
//! ## Deferred exposure
//!
//! [`register_search_tool`] registers with [`ToolExposure::Deferred`]:
//! the cold-start tool list carries name + description with a
//! deliberately permissive placeholder schema, and the real argument
//! schema (`query` required, `limit` 1..=50) is promoted once the
//! transcript mentions a `search` call — Anthropic's Tool-Search
//! pattern on this repo's existing exposure seam. The tool stays
//! callable the whole time; only its *advertisement* is staged. A
//! model that never needs cross-domain discovery never pays for the
//! schema.
//!
//! Retrieval itself is read-only and CPU-bound, so the tool keeps the
//! default [`ExecutionMode::Parallel`](elysper_tool::ExecutionMode::Parallel)
//! and may run alongside its siblings.
//!
//! ## Wiring it up
//!
//! ```
//! use std::sync::Arc;
//!
//! use elysper_core::registry::RegistryItem;
//! use elysper_search::{
//!     CjkTokenizer, Embedder, HashingEmbedder, Registry, SearchEngine,
//!     Searchable, Tokenizer,
//! };
//! use elysper_tool::{Context, Tool as _, ToolRegistry};
//!
//! #[derive(Clone)]
//! struct Skill {
//!     id: String,
//!     title: String,
//!     body: String,
//! }
//!
//! impl RegistryItem for Skill {
//!     fn name(&self) -> &str {
//!         &self.id
//!     }
//!
//!     fn description(&self) -> &str {
//!         &self.title
//!     }
//! }
//!
//! impl Searchable for Skill {
//!     fn indexed_fields(&self) -> Vec<(String, f32)> {
//!         vec![(self.title.clone(), 2.0), (self.body.clone(), 1.0)]
//!     }
//! }
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! let tk: Arc<dyn Tokenizer> = Arc::new(CjkTokenizer);
//! let emb: Arc<dyn Embedder> = Arc::new(HashingEmbedder::new(64, tk.clone()));
//! let skills: SearchEngine<Skill> = SearchEngine::new(emb, tk);
//! skills
//!     .add(Skill {
//!         id: "skill:pdf".into(),
//!         title: "pdf 提取".into(),
//!         body: "把 PDF 解析成文本".into(),
//!     })
//!     .unwrap();
//!
//! let search_registry = Arc::new(Registry::new());
//! search_registry.register(skills);
//!
//! // Advertised as name + description until the first call…
//! let tools = ToolRegistry::new();
//! assert!(elysper_tool_search::register_search_tool(
//!     &tools,
//!     Arc::clone(&search_registry),
//! ));
//!
//! // …and callable immediately: the model's second call is a call.
//! let tool = elysper_tool_search::SearchTool::new(search_registry);
//! let out = tool
//!     .call(
//!         serde_json::json!({"query": "pdf 提取"}),
//!         &Context::default(),
//!     )
//!     .await;
//! assert_eq!(out.is_error, None);
//! let text: String = out
//!     .content
//!     .iter()
//!     .filter_map(|part| part.text())
//!     .collect::<Vec<_>>()
//!     .join("\n");
//! assert!(text.contains("skill:pdf"), "{text}");
//! # }
//! ```

use std::sync::Arc;

use elysper_search::Registry;
use elysper_tool::{
    Context,
    Tool,
    ToolAnnotations,
    ToolEntry,
    ToolExposure,
    ToolOutput,
};

/// The name the model calls: `search`.
pub const SEARCH_TOOL_NAME: &str = "search";

/// Hits returned when the model does not ask for a `limit`.
const DEFAULT_LIMIT: usize = 8;

/// Ceiling for `limit` — a discovery call returns candidates, not a
/// corpus dump.
const MAX_LIMIT: usize = 50;

/// The `search` tool over a host-built [`Registry`].
pub struct SearchTool {
    registry: Arc<Registry>,
}

impl SearchTool {
    /// Build the tool over `registry`.
    ///
    /// The registry is held (not copied), so the host may keep
    /// registering domains after the tool is in its registry —
    /// including lazily, behind the same `Arc`.
    pub fn new(registry: Arc<Registry>) -> Self {
        Self { registry }
    }
}

/// The model-supplied arguments of one `search` call.
#[derive(serde::Deserialize)]
struct SearchArgs {
    query: String,
    limit: Option<usize>,
    /// Restrict the call to one domain label (`"tool"`,
    /// `"skill"`, `"mcp"`, `"memory"`, `"agent"`, `"session"` —
    /// whatever the host registered). Absent searches all.
    domain: Option<String>,
}

#[async_trait::async_trait]
impl Tool for SearchTool {
    fn name(&self) -> &str {
        SEARCH_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Search every catalog the host registered — tools, skills, \
         MCP tools, memory, peer agents and past sessions — by \
         keyword and meaning. Returns domain, id, title and a \
         one-line why per hit, best match first. Use it to \
         discover what is available before asking for details."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "query": {
                    "type": "string",
                    "description": "What to look for, in natural language."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_LIMIT,
                    "description": "Max hits to return (default 8)."
                },
                "domain": {
                    "type": "string",
                    "description": "Restrict to one catalog — tool, skill, \
                     mcp, memory, agent or session. Omit to search all."
                }
            },
            "required": ["query"]
        })
    }

    /// R124: MCP-native descriptor hints. `search` is read-only and
    /// returns the same hits for the same query against an
    /// unchanged registry (idempotent within a session). The
    /// `open_world_hint` is left to the host — the registry's
    /// engines decide what an `open_world` is (memory recall is
    /// local; session recall is on-disk; tool/skill catalogs are
    /// metadata). Default `false` is the right conservative
    /// setting for the model-facing tool surface.
    fn annotations(&self) -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(false),
        })
    }

    /// Validation errors are results the model reads and can correct —
    /// an unknown argument shape or a blank query never reaches the
    /// engines.
    async fn call(
        &self,
        input: serde_json::Value,
        _context: &Context,
    ) -> ToolOutput {
        let args: SearchArgs = match serde_json::from_value(input) {
            Ok(args) => args,
            Err(e) => {
                return ToolOutput::error(format!("Invalid arguments: {e}"));
            }
        };
        if args.query.trim().is_empty() {
            return ToolOutput::error("`query` must not be empty.");
        }
        let limit = args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let mut ctx =
            elysper_search::QueryContext::new(args.query.trim()).top_k(limit);
        if let Some(domain) = args
            .domain
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
        {
            ctx = ctx.domains([domain.to_string()]);
        }
        // The projection is `search_tool_ctx`'s verbatim output: it
        // already fans across every engine and renders the agent
        // view, so a second rendering here could only drift from it.
        ToolOutput::text(
            elysper_search::search_tool_ctx(&self.registry, ctx).await,
        )
    }
}

/// Register the `search` tool with [`ToolExposure::Deferred`].
///
/// Returns the underlying
/// [`ToolRegistry::register_entry`](elysper_tool::ToolRegistry::register_entry)
/// result: `false` only when a `Core`-provenance tool already owns the
/// name, i.e. the plugin must not shadow the harness's own tool.
pub fn register_search_tool(
    tool_registry: &elysper_tool::ToolRegistry,
    search_registry: Arc<Registry>,
) -> bool {
    tool_registry.register_entry(
        ToolEntry::new(Arc::new(SearchTool::new(search_registry)))
            .with_exposure(ToolExposure::Deferred),
    )
}

/// Register the `search` tool and return the withdrawal token.
///
/// The plugin-shaped registration: the caller keeps the token and
/// unregisters with it when its effect revokes, so the tool's
/// lifetime is the row's. `None` means a `Core`-provenance tool
/// already owns the name.
pub fn register_search_tool_with_token(
    tool_registry: &elysper_tool::ToolRegistry,
    search_registry: Arc<Registry>,
) -> Option<elysper_tool::RegistrationToken> {
    tool_registry.register_entry_with_token(
        ToolEntry::new(Arc::new(SearchTool::new(search_registry)))
            .with_exposure(ToolExposure::Deferred),
    )
}

#[cfg(test)]
mod tests;
