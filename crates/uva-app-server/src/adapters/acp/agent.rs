//! The ACP agent: the protocol's methods, answered by the core.
//!
//! One agent, one deployment: the sessions a client opens are the
//! deployment's own sessions, the turn a client prompts is the
//! deployment's own turn, and the decision the operator takes on a
//! held call lands in the deployment's own approval rows.
//!
//! # The methods
//!
//! | method | what it does |
//! |---|---|
//! | `initialize` | the handshake: the protocol version, the capabilities, the agent's identity |
//! | `authenticate` | refused — this deployment has no authentication methods |
//! | `session/new` | a session of its own, addressed by an opaque id |
//! | `session/load` | the session's transcript, replayed as session updates |
//! | `session/prompt` | one turn: the deltas stream back, the answer names the stop reason |
//! | `session/cancel` | the live turn is cancelled |
//!
//! # The conversation
//!
//! A `session/prompt` request stays open until the turn is over:
//! the deltas arrive as `session/update` notifications, and a held
//! call turns into a `session/request_permission` request the client
//! answers. Only then does the prompt request answer, with the stop
//! reason.
//!
//! # What this agent does not do
//!
//! The protocol allows more than this agent implements, and the
//! capabilities it advertises say so: no session modes, no slash
//! commands, no client-side file system or terminal delegation, no
//! per-session MCP servers. A client that reads the capabilities
//! knows what it may ask for.

use std::{collections::HashMap, sync::Arc};

use agent_client_protocol::{
    Agent,
    Client,
    Error,
    v1::schema::{
        AgentCapabilities,
        AuthenticateRequest,
        CancelNotification,
        FileSystemCapabilities,
        Implementation,
        InitializeRequest,
        InitializeResponse,
        LoadSessionRequest,
        LoadSessionResponse,
        McpCapabilities,
        NewSessionRequest,
        NewSessionResponse,
        PromptCapabilities,
        PromptRequest,
        PromptResponse,
    },
};
use parking_lot::Mutex;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::{
    adapters::acp::{projection, turn},
    state::AppState,
};

/// The ACP agent: the deployment's sessions, spoken as ACP.
pub struct AcpAgent {
    /// The deployment the agent answers for.
    pub(crate) state: Arc<AppState>,
    /// The sessions the client opened, and the directory it named.
    sessions: Mutex<HashMap<String, AcpSession>>,
    /// What the client offered in `initialize`: which `fs/*` methods
    /// it answers. A session opened by a client that offers them
    /// reads and writes **its own files**.
    fs: Mutex<Option<FileSystemCapabilities>>,
    /// Whether the client offered the `terminal/*` methods. A session
    /// opened by a client that offers them runs its commands in the
    /// client's own terminal.
    terminal: Mutex<bool>,
}

/// One session the client opened.
#[derive(Clone, Debug, Default)]
pub(crate) struct AcpSession {
    /// The working directory the client named.
    ///
    /// The deployment's own workspace root is where the tools run;
    /// the client's directory is kept so the session can report it.
    pub(crate) cwd: Option<String>,
}

impl AcpAgent {
    /// Build the agent over a deployment.
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self {
            state,
            sessions: Mutex::new(HashMap::new()),
            fs: Mutex::new(None),
            terminal: Mutex::new(false),
        })
    }

    /// Run the agent over a transport.
    ///
    /// The methods are the protocol's own: the SDK's schema types
    /// carry their names and their shapes, so the agent answers with
    /// what a client decodes. The transport decides how the frames
    /// travel — the process's pipes, or a channel the core's own
    /// transport feeds.
    pub async fn connect(
        self: &Arc<Self>,
        transport: impl agent_client_protocol::ConnectTo<Agent>,
    ) -> Result<(), agent_client_protocol::Error> {
        let agent = Arc::clone(self);
        Agent
            .v1()
            .on_receive_request(
                {
                    let agent = Arc::clone(&agent);
                    async move |request: InitializeRequest, responder, _cx| {
                        responder.respond(agent.initialize(request))
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |_request: AuthenticateRequest, _responder, _cx| {
                    Err::<(), Error>(
                        Error::auth_required().data(
                            "this deployment has no authentication methods",
                        ),
                    )
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let agent = Arc::clone(&agent);
                    async move |request: NewSessionRequest,
                                responder,
                                cx: agent_client_protocol::v1::ConnectionTo<
                        Client,
                    >| {
                        responder.respond(agent.session_new(request, &cx))
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let agent = Arc::clone(&agent);
                    async move |request: LoadSessionRequest, responder, cx| {
                        let agent = Arc::clone(&agent);
                        let task_cx = cx.clone();
                        // The replay runs outside the dispatch loop:
                        // it reads the session's log and streams the
                        // conversation back, and the loop must keep
                        // serving the client's other messages while
                        // it does.
                        cx.spawn(async move {
                            responder.respond(
                                agent.session_load(request, &task_cx).await,
                            )
                        })
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let agent = Arc::clone(&agent);
                    async move |request: PromptRequest, responder, cx| {
                        let agent = Arc::clone(&agent);
                        let task_cx = cx.clone();
                        // The turn runs outside the dispatch loop: it
                        // can take minutes, and the loop must keep
                        // serving the client's other messages — the
                        // cancel notification, the answer to a
                        // permission question — while it runs. The
                        // answer to the prompt is sent from here,
                        // when the turn is over.
                        cx.spawn(async move {
                            match turn::run(&agent, &task_cx, &request).await {
                                Ok(stop_reason) => responder
                                    .respond(PromptResponse::new(stop_reason)),
                                Err(error) => {
                                    responder.respond_with_internal_error(error)
                                }
                            }
                        })
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_notification(
                {
                    let agent = Arc::clone(&agent);
                    async move |notification: CancelNotification, _cx| {
                        agent.cancel(&notification).await;
                        Ok(())
                    }
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .connect_to(transport)
            .await
    }

    /// The handshake.
    ///
    /// The capabilities say exactly what this agent does: it can
    /// reload a session, it takes text and image prompts, and it has
    /// no MCP transports of its own.
    fn initialize(&self, request: InitializeRequest) -> InitializeResponse {
        let fs = request.client_capabilities.fs.clone();
        if fs.read_text_file || fs.write_text_file {
            info!(
                read = fs.read_text_file,
                write = fs.write_text_file,
                "the client offers the ACP file methods; its sessions \
                 will work on its own files"
            );
        }
        if request.client_capabilities.terminal {
            info!(
                "the client offers the ACP terminal methods; its sessions \
                 will run their commands in its own terminal"
            );
        }
        *self.terminal.lock() = request.client_capabilities.terminal;
        self.fs.lock().replace(fs);
        InitializeResponse::new(request.protocol_version)
            .agent_capabilities(
                AgentCapabilities::new()
                    .load_session(true)
                    .prompt_capabilities(
                        PromptCapabilities::new()
                            .image(true)
                            .audio(false)
                            .embedded_context(true),
                    )
                    .mcp_capabilities(
                        McpCapabilities::new().http(false).sse(false),
                    ),
            )
            .agent_info(
                Implementation::new("uva", crate::build_info::VERSION)
                    .title("Uva"),
            )
            .auth_methods(Vec::new())
    }

    /// Open a session.
    ///
    /// The id is opaque to the client and is the session's own id in
    /// the deployment, so a session the client opened is the same
    /// session the deployment's own surfaces see.
    fn session_new(
        &self,
        request: NewSessionRequest,
        cx: &agent_client_protocol::v1::ConnectionTo<Client>,
    ) -> NewSessionResponse {
        if !request.mcp_servers.is_empty() {
            info!(
                servers = request.mcp_servers.len(),
                "the client offered MCP servers; this agent serves the \
                 deployment's own MCP configuration instead"
            );
        }
        let cwd = Some(request.cwd.to_string_lossy().to_string());
        let session_id = format!("sess_{}", Uuid::new_v4().simple());
        // A client that offered the `fs/*` methods owns this session's
        // files: the session's `read` and `write` tools ask it instead
        // of touching this disk. A client that offered nothing leaves
        // the session on the deployment's own workspace.
        self.claim_session(&session_id, &request.cwd, cx);
        self.sessions
            .lock()
            .insert(session_id.clone(), AcpSession { cwd: cwd.clone() });
        info!(session_id = %session_id, cwd = ?cwd, "ACP session opened");
        NewSessionResponse::new(session_id)
    }

    /// Give the client the session it owns.
    ///
    /// The client that advertised the `fs/*` methods owns the
    /// session's files; the client that advertised the `terminal/*`
    /// methods owns the session's terminal. A client that advertised
    /// neither leaves the session on the deployment's own disk and its
    /// own machine.
    fn claim_session(
        &self,
        session_id: &str,
        cwd: &std::path::Path,
        cx: &agent_client_protocol::v1::ConnectionTo<Client>,
    ) {
        // ACP v1 has no question primitive: a client can answer a
        // permission request, and nothing else. A session this client
        // owns therefore refuses the model's questions at once,
        // instead of waiting out the budget for an answer no editor
        // can give.
        self.state.questions.register(
            session_id,
            Arc::new(crate::session::questions::RefusingQuestions::new(
                "this session's client speaks ACP, which has no question \
                 primitive; decide without an answer",
            )),
        );
        if let Some(fs) = self.fs.lock().clone() {
            let owner = Arc::new(super::files::ClientFileSystem::new(
                cx.clone(),
                cwd.to_path_buf(),
                fs,
            ));
            self.state.file_system.register(session_id, owner);
        }
        if *self.terminal.lock() {
            let owner =
                Arc::new(super::terminal::ClientTerminal::new(cx.clone()));
            self.state.terminals.register(
                session_id,
                owner as Arc<dyn uva::tool_shell::TerminalOwner>,
            );
        }
    }

    /// Restore a session and replay its transcript.
    ///
    /// The replay streams the conversation back as session updates
    /// before the request is answered, so the client sees the same
    /// conversation it saw before the agent was restarted.
    async fn session_load(
        &self,
        request: LoadSessionRequest,
        cx: &agent_client_protocol::v1::ConnectionTo<Client>,
    ) -> LoadSessionResponse {
        let session_id = request.session_id.to_string();
        let rows = match read_rows(&self.state, &session_id).await {
            Ok(rows) => rows,
            Err(error) => {
                warn!(
                    session_id = %session_id,
                    error = %error,
                    "the session could not be read; the load answers empty"
                );
                return LoadSessionResponse::new();
            }
        };
        let surface = uva::session::fold_log_surface(&rows);
        let messages = uva::session::surface_to_messages(&surface.messages);
        for update in projection::replay_updates(&messages) {
            if let Err(error) = cx.send_notification(
                agent_client_protocol::v1::schema::SessionNotification::new(
                    session_id.clone(),
                    update,
                ),
            ) {
                warn!(
                    session_id = %session_id,
                    error = %error,
                    "the replayed update could not be sent"
                );
                break;
            }
        }
        // A restored session is the client's session too: the client
        // owns its files and its terminal, exactly as it owned them
        // when the session was opened.
        self.claim_session(&session_id, &request.cwd, cx);
        self.sessions.lock().entry(session_id).or_default();
        LoadSessionResponse::new()
    }

    /// Cancel the live turn of a session.
    async fn cancel(&self, notification: &CancelNotification) {
        let session_id = notification.session_id.to_string();
        let user_id = self.state.default_user_id().to_string();
        match self
            .state
            .must_exist_session_controller(&user_id, &session_id)
            .await
        {
            Ok(controller) => {
                if let Err(error) = controller.cancel().await {
                    warn!(
                        session_id = %session_id,
                        error = %error,
                        "the ACP cancel did not reach the session"
                    );
                }
            }
            Err(error) => {
                debug!(
                    session_id = %session_id,
                    error = ?error,
                    "the ACP cancel named a session that does not exist"
                );
            }
        }
    }

    /// The session record of a session id, when the client opened it.
    pub(crate) fn session(&self, session_id: &str) -> AcpSession {
        self.sessions
            .lock()
            .get(session_id)
            .cloned()
            .unwrap_or_default()
    }
}

/// The rows of a session's durable log.
///
/// The follow projection's reader is the one owner of that read: a
/// reload sees exactly what the deployment's own surfaces see.
async fn read_rows(
    state: &Arc<AppState>,
    session_id: &str,
) -> Result<Vec<serde_json::Value>, String> {
    let user_id = state.default_user_id().to_string();
    crate::routes::follow::session_rows(state, &user_id, session_id)
        .await
        .map_err(|error| error.message)
}
