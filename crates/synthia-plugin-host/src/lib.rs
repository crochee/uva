//! # synthia-plugin-host
//!
//! The host half of Synthia's dynamic plugin channel: it turns a plugin
//! **process** that speaks [`synthia_plugin_protocol`] into
//! registrations on the harness's [`Host`], so the agent loop never
//! learns that a plugin exists.
//!
//! The static half — the `Plugin` trait,
//! [`Registration`](synthia_harness::Registration) and the withdrawal
//! contract — lives in `synthia-harness`. This crate is only the
//! adapter.
//!
//! ## Layers
//!
//! ```text
//! RemotePlugin::install(&mut Host)
//!   ├─ process     std::process::Command, piped stdin/stdout
//!   ├─ client      line-delimited JSON-RPC 2.0, correlated by `id`,
//!   │              one reader thread + one deadline reaper per plugin
//!   ├─ RemoteTool  one synthia_tool::Tool per `synthia/tools/list` row
//!   └─ observer    hook events → `synthia/event` notifications
//! ```
//!
//! One plugin is one process and one channel. Calls are correlated by
//! request id, so several may be in flight and their answers may arrive
//! in any order. Every message — a call, and equally the write that
//! carries it — is bounded by one deadline; a plugin that misses it is
//! treated as wedged and killed, because a plugin that stops reading its
//! stdin blocks the host's writer. The failure paths are in `client`'s
//! module docs.
//!
//! **An expired deadline kills the whole plugin**: every tool it
//! registered and its observer, not only the message that expired. That
//! is deliberate — the kill is the only thing that releases a writer
//! blocked on the plugin's stdin — so a plugin whose calls can be slow
//! should answer within the configured timeout rather than rely on the
//! host distinguishing one slow call from a wedged process.
//!
//! ## The plugin's side of the contract
//!
//! - One JSON-RPC 2.0 object per line on stdout, newline-terminated.
//!   **stdout carries protocol frames only** — a plugin that logs there
//!   breaks framing, and the host fails the install with
//!   [`PluginError::BadRequest`](synthia_plugin_protocol::PluginError).
//!   stderr is inherited, so that is where diagnostics belong.
//! - `synthia/initialize` → `PluginCapabilities`.
//! - `synthia/tools/list` → an array of [`ToolSpec`].
//! - `synthia/tools/call` → params `{"name": …, "arguments": …}`, result
//!   is the tool's result: a JSON string is its text verbatim, any other
//!   value is rendered as JSON for the model.
//! - `synthia/event` → a notification, no answer.
//!
//! The method names are the constants in
//! [`synthia_plugin_protocol::methods`]; nothing here hard-codes a
//! string.
//!
//! ## Authority
//!
//! [`Authority::Reduced`] is the default and most of the permission
//! matrix: register tools, observe events, rewrite a tool result. A
//! reduced plugin gets no gate, no request transformer and no prompt
//! section — deciding *whether* a call runs, rewriting the request, and
//! writing the system prompt are the escalation paths, because they
//! steer the run rather than describe its results.
//! [`Authority::Full`] is the operator's promotion, never something a
//! plugin can ask for; [`RemotePlugin::exposes_gate`] reports it.
//!
//! A rewrite of a tool result is granted at *either* authority. The
//! harness runs transformers before it commits the result, so the
//! `tool_result` row holds the rewritten text — and it also writes a
//! `tool_result_transformed` row naming the transformer that altered
//! it. That provenance is why a remote plugin can be allowed to rewrite
//! without becoming invisible.
//!
//! A remote tool's risk is the host's decision too: see
//! [`Tool::risk`](synthia_tool::Tool::risk).
//!
//! ## Tool names belong to plugins
//!
//! A [`Host`] is per-agent and knows nothing about plugin identity: two
//! dynamic tools may share a name, the registry appends both, and the
//! last one wins. Installing a second plugin that reuses a name would
//! therefore hand that name to a different process under a different host
//! risk — capability substitution. [`PluginRegistry`] is the layer that
//! refuses it, and an install through it fails with
//! [`HostError::DuplicateToolName`].
//!
//! ## Runtime neutrality
//!
//! This crate names no runtime, in its dependency graph or its public
//! API. The plugin is a `std::process::Command`; blocking protocol IO
//! runs through [`Blocking`], replaceable through
//! [`RemotePlugin::with_blocking`]. The deadline is enforced by the
//! connection's own reaper thread, which waits on a condition variable:
//! a delay seam cannot express it, because a seam's wait cannot be
//! cancelled, so a reaper built on one would either spawn a thread per
//! request or delay shutdown by a whole timeout.

mod client;
mod error;
mod gate;
mod observer;
mod process;
mod registry;
mod remote_tool;
mod transform;

use std::{collections::HashMap, fmt, sync::Arc, time::Duration};

pub use error::HostError;
pub use registry::PluginRegistry;
use serde::{Deserialize, Serialize};
use serde_json::json;
use synthia_core::blocking::{Blocking, ThreadBlocking};
use synthia_harness::{Host, PluginHandle};
use synthia_plugin_protocol::{PluginCapabilities, ToolSpec, methods};
use synthia_tool::Risk;

use crate::{
    client::Connection,
    gate::RemoteGate,
    observer::RemoteObserver,
    remote_tool::RemoteTool,
    transform::RemoteTransformer,
};

/// How long one request may take before the host gives up on it.
///
/// A plugin is another process: a call can hang on its own deadlock, on
/// a prompt, or on a wait that has nothing to do with this host. The
/// bound covers the write as well as the answer — a plugin that stops
/// reading its stdin blocks the writer — and expiring it kills the
/// process, which is what releases the write.
const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// How to launch and trust one plugin.
///
/// A plain value type — no seams, no channels — because the composition
/// layer deserialises it from the deployment's config file. The runtime
/// seam belongs to [`RemotePlugin`], not here.
///
/// `capabilities` is the operator's ceiling for the three roles a
/// plugin can offer. Its `gate` field is not read: the gate is granted
/// by `authority` and *answered* by the plugin, so a promotion the
/// plugin cannot serve is an install error rather than a silently
/// disabled feature.
///
/// Unknown keys are rejected: this config decides `authority` and
/// `risk`, so a misspelled key must fail loudly instead of silently
/// leaving the default in place. A *missing* required field is an error
/// for the same reason — the fail-closed direction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemotePluginConfig {
    /// The plugin's name: the prefix of its diagnostics and the
    /// observer's registration name.
    pub name: String,
    /// The argv the host spawns: `argv[0]` is the program and the rest
    /// are its arguments.
    ///
    /// An argv vector, not a shell string: nothing in a plugin's command
    /// line is interpreted by a shell.
    pub command: Vec<String>,
    /// How the host reaches the plugin.
    pub transport: TransportKind,
    /// The roles the host is willing to use.
    ///
    /// A ceiling, not a request: the handshake can only narrow it, so a
    /// plugin that announces more than the operator enabled gets the
    /// intersection of the two.
    pub capabilities: PluginCapabilities,
    /// How much authority the operator grants it.
    pub authority: Authority,
    /// The risk every one of this plugin's tools is judged at.
    ///
    /// A host decision, because a plugin's self-report cannot be trusted
    /// to withhold its own approval — see
    /// [`Tool::risk`](synthia_tool::Tool::risk).
    pub risk: Risk,
}

/// How much authority the operator grants a plugin.
///
/// The default is [`Authority::Reduced`]. Promotion to
/// [`Authority::Full`] is an operator decision, never something the
/// plugin can request, because it hands over the ability to veto a tool
/// call, to hold one for approval and to end the run — the decisions
/// that steer *whether* the work happens. A remote prompt section is not
/// registered from here; when it is, it stays escalation-only, because
/// a section is rendered **unfenced and unescaped** — it means *trusted
/// to write the system prompt*.
///
/// Rewriting a tool result is the default's own grant, not an
/// escalation: it changes what one result says, the harness writes a
/// `tool_result_transformed` row for it, and a rewrite that fails costs
/// the result a diagnostic rather than a turn.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Authority {
    /// The default: tools and observation, and nothing that can steer
    /// the run.
    #[default]
    Reduced,
    /// Operator-granted parity with an in-process plugin.
    Full,
}

/// How the host reaches the plugin process.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportKind {
    /// Line-delimited JSON-RPC 2.0 over the process's stdin and stdout.
    #[default]
    Stdio,
    /// JSON-RPC over HTTP.
    ///
    /// Part of the wire design's transport list, and part of this config
    /// surface so a deployment's file does not change shape later — but
    /// not implemented here: an HTTP client is a runtime-bearing
    /// dependency, and a library crate in this workspace must not name
    /// or carry one. [`RemotePlugin::install`] reports
    /// [`HostError::UnsupportedTransport`] for it, and stdio is the
    /// deployed transport.
    Http {
        /// The endpoint to POST to.
        url: String,
    },
}

/// One plugin, as this host runs it.
///
/// The config says *what* to launch; the plugin holds *how* this host
/// runs it — the blocking seam and the deadline — so the config stays a
/// value a deployment file can describe.
pub struct RemotePlugin {
    config: RemotePluginConfig,
    blocking: Arc<dyn Blocking>,
    timeout: Duration,
}

impl RemotePlugin {
    /// A plugin with the runtime-free seam and the default deadline.
    ///
    /// Blocking IO goes through [`ThreadBlocking`]; a consumer on a real
    /// runtime installs that runtime's implementation instead.
    #[must_use]
    pub fn new(config: RemotePluginConfig) -> Self {
        Self {
            config,
            blocking: Arc::new(ThreadBlocking),
            timeout: DEFAULT_CALL_TIMEOUT,
        }
    }

    /// Run the channel's blocking IO through `blocking`.
    #[must_use]
    pub fn with_blocking(mut self, blocking: Arc<dyn Blocking>) -> Self {
        self.blocking = blocking;
        self
    }

    /// How long one request may take before it is abandoned.
    ///
    /// The default is 30 seconds, which is generous for an IPC call and
    /// still bounds a wedged plugin. The deadline covers the write as
    /// well as the answer, and expiry kills the plugin.
    #[must_use]
    pub fn with_call_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The plugin's configured name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.config.name
    }

    /// Whether the operator elevated this plugin to full parity.
    ///
    /// `true` for [`Authority::Full`] only. A [`Authority::Reduced`]
    /// plugin is never sent `synthia/gate`, whatever it implements.
    #[must_use]
    pub fn exposes_gate(&self) -> bool {
        matches!(self.config.authority, Authority::Full)
    }

    /// The risk the host judges every tool of this plugin at.
    ///
    /// The plugin's own claims never move this value: every call is
    /// judged at the configured risk.
    #[must_use]
    pub fn remote_risk(&self) -> Risk {
        self.config.risk
    }

    /// Spawn the plugin, negotiate its capabilities, and register what
    /// the host may use.
    ///
    /// Tools are registered when both the config and the plugin announce
    /// `tools`; a read-only observer is registered when both announce
    /// `observe`; a result transformer is registered when both announce
    /// `transform` — at either authority, because a rewrite only changes
    /// what one result says and the harness records its provenance. A
    /// gate is registered only when the operator promoted the plugin to
    /// [`Authority::Full`] *and* the plugin announces `gate`, and this
    /// branch is the only place that decides it: a
    /// [`Authority::Reduced`] plugin has no path to a veto. A request
    /// transformer and a prompt section are not wired yet.
    ///
    /// The returned handle owns every registration the install made:
    /// dropping it withdraws them and kills the process. Each call
    /// starts its own process, so installing one plugin twice yields two
    /// independent channels.
    ///
    /// A duplicate tool name inside one plugin's own list is refused
    /// here; a duplicate against a *previously installed* plugin needs
    /// [`PluginRegistry`], because a [`Host`] alone does not remember
    /// which plugin owns a name.
    ///
    /// # Errors
    ///
    /// Any failure of the handshake: the process does not start, exits
    /// before answering, answers with something that is not a
    /// [`PluginCapabilities`], writes something that is not JSON, misses
    /// its deadline, or offers nothing this host can register. A
    /// [`Authority::Full`] plugin that does not announce `gate` is
    /// refused too: promoting it would leave it blocking — or rather,
    /// failing closed on — every tool call in this host. Nothing is
    /// registered in that case; a plugin is optional capability, so the
    /// caller reports the error and carries on without it.
    pub async fn install(
        &self,
        host: &mut Host,
    ) -> Result<PluginHandle, HostError> {
        // One plugin's own list is checked with an empty claim map: an
        // install outside a registry still has to notice that it offers
        // the same name twice.
        let (handle, _tools) =
            self.install_claimed(host, &mut HashMap::new()).await?;
        Ok(handle)
    }

    // The install both entry points share.
    //
    // `claimed` maps a tool name to the plugin that owns it, so a
    // collision with an *earlier* plugin is caught before anything is
    // registered. Returns the tool names this install claimed, so the
    // caller can check the registry's own verdict on them.
    pub(crate) async fn install_claimed(
        &self,
        host: &mut Host,
        claimed: &mut HashMap<String, String>,
    ) -> Result<(PluginHandle, Vec<String>), HostError> {
        let negotiated = self.connect().await?;
        let specs = if negotiated.capabilities.tools {
            Some(self.list_tools(&negotiated).await?)
        } else {
            None
        };
        if let Some(specs) = &specs {
            self.claim(specs, claimed)?;
        }
        let claimed_tools = specs
            .as_ref()
            .map(|specs| specs.iter().map(|spec| spec.name.clone()).collect())
            .unwrap_or_default();
        let mut registrations = Vec::new();
        if let Some(specs) = specs {
            for spec in specs {
                let tool = Arc::new(RemoteTool::new(
                    spec,
                    self.config.risk,
                    Arc::clone(&negotiated.connection),
                ));
                registrations.push(host.tool(tool));
            }
        }
        if negotiated.capabilities.observe {
            let observer = RemoteObserver::new(
                self.config.name.clone(),
                Arc::clone(&negotiated.connection),
            );
            registrations.push(host.observe(Arc::new(observer)));
        }
        // The one place that decides the escalation: a gate exists only
        // for a plugin that is both promoted and able to answer, and
        // there is no other path that can register one.
        if negotiated.capabilities.gate {
            let gate = RemoteGate::new(
                self.config.name.clone(),
                Arc::clone(&negotiated.connection),
            );
            registrations.push(host.gate(Arc::new(gate)));
        }
        // A rewrite is granted at either authority: it changes what one
        // result says, and the harness audits it.
        if negotiated.capabilities.transform {
            let transformer = RemoteTransformer::new(
                self.config.name.clone(),
                Arc::clone(&negotiated.connection),
            );
            registrations.push(host.result_transformer(Arc::new(transformer)));
        }
        if registrations.is_empty() {
            // Reporting success here would hand back a handle that owns
            // nothing while the process dies with the connection: the
            // plugin and the config agree on no capability this host can
            // use, which is a misconfiguration the caller should see.
            return Err(HostError::bad_request(
                &self.config.name,
                "the plugin and the config enable no capability this host \
                 can register: no tool was listed, no observer was \
                 requested, and no result rewrite was enabled",
            ));
        }
        Ok((PluginHandle::new(registrations), claimed_tools))
    }

    // Spawn the process and agree on capabilities.
    async fn connect(&self) -> Result<Negotiated, HostError> {
        match &self.config.transport {
            TransportKind::Stdio => {}
            TransportKind::Http { .. } => {
                return Err(HostError::UnsupportedTransport {
                    transport: "http",
                });
            }
        }
        let connection = Arc::new(Connection::spawn(
            &self.config.name,
            &self.config.command,
            Arc::clone(&self.blocking),
            self.timeout,
        )?);
        let announced =
            connection.request(methods::INITIALIZE, json!({})).await?;
        let announced: PluginCapabilities = serde_json::from_value(announced)
            .map_err(|error| {
            HostError::bad_request(
                &self.config.name,
                format!(
                    "`{}` did not answer with capabilities: {error}",
                    methods::INITIALIZE
                ),
            )
        })?;
        // The operator's declaration is the ceiling: a plugin can offer
        // less than the deployment enabled, never more.
        let enabled = self.config.capabilities;
        // Promotion is the operator's word and the gate is the plugin's
        // answer. A `Full` plugin that never implements `synthia/gate`
        // would have every call in this host blocked, one deadline at a
        // time, so the mismatch is refused here — loudly, at install —
        // instead of at the first call.
        if self.exposes_gate() && !announced.gate {
            return Err(HostError::bad_request(
                &self.config.name,
                format!(
                    "authority `full` requires the plugin to announce \
                     `gate`, but `{}` did not answer `{}` with `\"gate\": \
                     true`; without that method every tool call in this \
                     host would be blocked",
                    self.config.name,
                    methods::INITIALIZE
                ),
            ));
        }
        Ok(Negotiated {
            connection,
            capabilities: PluginCapabilities {
                tools: enabled.tools && announced.tools,
                observe: enabled.observe && announced.observe,
                transform: enabled.transform && announced.transform,
                // Not a config ceiling: the gate is granted by the
                // authority and answered by the plugin.
                gate: self.exposes_gate() && announced.gate,
            },
        })
    }

    // Ask for the tool descriptors.
    async fn list_tools(
        &self,
        negotiated: &Negotiated,
    ) -> Result<Vec<ToolSpec>, HostError> {
        let listed = negotiated
            .connection
            .request(methods::TOOLS_LIST, json!({}))
            .await?;
        serde_json::from_value(listed).map_err(|error| {
            HostError::bad_request(
                &self.config.name,
                format!(
                    "`{}` did not answer with tool descriptors: {error}",
                    methods::TOOLS_LIST
                ),
            )
        })
    }

    // Claim this plugin's tool names, or refuse the install.
    //
    // Nothing is claimed until every name is free, so a refused install
    // leaves the registry exactly as it was.
    fn claim(
        &self,
        specs: &[ToolSpec],
        claimed: &mut HashMap<String, String>,
    ) -> Result<(), HostError> {
        let mut seen: Vec<&str> = Vec::with_capacity(specs.len());
        for spec in specs {
            let owner = match claimed.get(&spec.name) {
                Some(owner) => Some(owner.as_str()),
                // The same plugin offering a name twice is the same
                // conflict: the second entry would shadow the first.
                None if seen.contains(&spec.name.as_str()) => {
                    Some(self.config.name.as_str())
                }
                None => None,
            };
            if let Some(owner) = owner {
                return Err(HostError::DuplicateToolName {
                    plugin: self.config.name.clone(),
                    tool: spec.name.clone(),
                    owner: owner.to_string(),
                });
            }
            seen.push(spec.name.as_str());
        }
        for spec in specs {
            claimed.insert(spec.name.clone(), self.config.name.clone());
        }
        Ok(())
    }
}

impl fmt::Debug for RemotePlugin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemotePlugin")
            .field("name", &self.config.name)
            .field("authority", &self.config.authority)
            .field("risk", &self.config.risk)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

/// A live channel and the capabilities both sides agreed on.
struct Negotiated {
    connection: Arc<Connection>,
    capabilities: PluginCapabilities,
}
