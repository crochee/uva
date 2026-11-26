//! The ACP terminal delegation, over the real protocol.
//!
//! The test drives the adapter the way an editor does: the client
//! advertises the `terminal` capability in `initialize`, opens a
//! session, and then the deployment asks that client to run a command.
//! The fake client answers the four `terminal/*` requests the way a
//! real one does — create, wait for the exit, output, release — and
//! the test checks that the deployment learned the session's terminal
//! and that the command came back with the client's own output.

use std::{path::Path, sync::Arc, time::Duration};

use agent_client_protocol::{
    Agent,
    Channel,
    Client,
    schema::ProtocolVersion,
    v1::{
        ConnectionTo,
        schema::{
            ClientCapabilities,
            CreateTerminalRequest,
            CreateTerminalResponse,
            InitializeRequest,
            KillTerminalRequest,
            NewSessionRequest,
            ReleaseTerminalRequest,
            TerminalExitStatus,
            TerminalOutputRequest,
            TerminalOutputResponse,
            WaitForTerminalExitRequest,
            WaitForTerminalExitResponse,
        },
    },
};

use crate::state::AppState;

#[tokio::test]
async fn a_client_that_offers_the_terminal_owns_its_sessions_commands() {
    let dir = tempfile::tempdir().expect("a temp workspace");
    let manager = uva::session::manager::SessionRegistry::new(
        dir.path().join("sessions"),
    );
    let state =
        Arc::new(AppState::for_test(manager, dir.path().to_path_buf()).await);
    let agent = crate::adapters::acp::agent::AcpAgent::new(Arc::clone(&state));

    let (agent_transport, client_transport) = Channel::duplex();
    // The commands the deployment asked the client to run.
    let ran = Arc::new(parking_lot::Mutex::new(Vec::new()));
    // The session the client opens, and the signal that keeps its
    // connection open until the test is done with it.
    let (session_tx, session_rx) = tokio::sync::oneshot::channel::<String>();
    let release = Arc::new(tokio::sync::Notify::new());

    // The client side: the handshake, the session, then the command
    // the deployment asks the client to run.
    let client_task = tokio::spawn({
        let ran = Arc::clone(&ran);
        let release = Arc::clone(&release);
        async move {
            Client
                .v1()
                .on_receive_request(
                    {
                        let ran = Arc::clone(&ran);
                        async move |request: CreateTerminalRequest,
                                    responder: agent_client_protocol::v1::Responder<
                                        CreateTerminalResponse,
                                    >,
                                    _cx: ConnectionTo<Agent>| {
                            ran.lock().push(request.command);
                            responder.respond(
                                CreateTerminalResponse::new("term-1"),
                            )
                        }
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .on_receive_request(
                    async move |_request: TerminalOutputRequest,
                                responder: agent_client_protocol::v1::Responder<
                                    TerminalOutputResponse,
                                >,
                                _cx: ConnectionTo<Agent>| {
                        responder.respond(TerminalOutputResponse::new(
                            "the client's pane\n",
                            false,
                        ))
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .on_receive_request(
                    async move |_request: WaitForTerminalExitRequest,
                                responder: agent_client_protocol::v1::Responder<
                                    WaitForTerminalExitResponse,
                                >,
                                _cx: ConnectionTo<Agent>| {
                        responder.respond(WaitForTerminalExitResponse::new(
                            TerminalExitStatus::new().exit_code(0),
                        ))
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .on_receive_request(
                    async move |_request: KillTerminalRequest,
                                responder: agent_client_protocol::v1::Responder<
                                    agent_client_protocol::v1::schema::KillTerminalResponse,
                                >,
                                _cx: ConnectionTo<Agent>| {
                        responder.respond(
                            agent_client_protocol::v1::schema::KillTerminalResponse::new(),
                        )
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .on_receive_request(
                    async move |_request: ReleaseTerminalRequest,
                                responder: agent_client_protocol::v1::Responder<
                                    agent_client_protocol::v1::schema::ReleaseTerminalResponse,
                                >,
                                _cx: ConnectionTo<Agent>| {
                        responder.respond(
                            agent_client_protocol::v1::schema::ReleaseTerminalResponse::new(),
                        )
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .connect_with(
                    client_transport,
                    async |cx: ConnectionTo<Agent>| {
                        let _initialized = cx
                            .send_request(
                                InitializeRequest::new(ProtocolVersion::V1)
                                    .client_capabilities(
                                        ClientCapabilities::new().terminal(true),
                                    ),
                            )
                            .block_task()
                            .await?;
                        let session = cx
                            .send_request(NewSessionRequest::new(
                                std::env::current_dir().unwrap(),
                            ))
                            .block_task()
                            .await?;
                        let session_id = session.session_id.to_string();
                        let _ = session_tx.send(session_id);
                        // The client stays connected: the deployment
                        // asks it to run a command after the session
                        // is open, and a closed connection answers
                        // nothing.
                        release.notified().await;
                        Ok(())
                    },
                )
                .await
        }
    });

    let agent_task = tokio::spawn(async move {
        agent
            .connect(agent_transport)
            .await
            .expect("the agent connects");
    });

    let session_id = session_rx.await.expect("the client opens the session");

    // The session's terminal is the client's: the deployment asks the
    // client to run the command and gets the client's own output back.
    let owner = state
        .terminals
        .client(&session_id)
        .expect("the client owns this session's terminal");
    let run = owner
        .run(
            &session_id,
            "echo hi",
            Path::new("/tmp"),
            Duration::from_secs(5),
        )
        .await
        .expect("the client runs the command");

    assert_eq!(
        run.output, "the client's pane\n",
        "the model's answer is the client's own output"
    );
    assert_eq!(run.exit_code, Some(0), "the client's exit status");
    assert!(
        ran.lock().iter().any(|command| command == "sh"),
        "the command reached the client: {:?}",
        ran.lock()
    );

    // And the same path the model takes: the session's `shell` tool
    // asks the client, because the client owns this session's
    // terminal. The tool, the owner and the client are three hops —
    // the deployment wires all three.
    let shell = state
        .tool_registry
        .get("shell")
        .expect("the deployment mounts the shell tool")
        .tool_instance();
    let context = uva::tool::Context::new(
        session_id.clone(),
        std::path::PathBuf::from("/tmp"),
    );
    let output = shell
        .call(serde_json::json!({ "command": "echo hi" }), &context)
        .await;
    let text = output
        .content
        .iter()
        .find_map(|part| part.text().map(str::to_string))
        .unwrap_or_default();
    assert!(
        text.contains("the client's pane"),
        "the model's answer is the client's own output: {text}"
    );
    assert!(
        ran.lock().iter().any(|command| command == "sh"),
        "the command reached the client through the tool: {:?}",
        ran.lock()
    );

    // The client goes home; the agent's connection ends with it.
    release.notify_one();
    let _ = client_task.await;
    agent_task.abort();
}
