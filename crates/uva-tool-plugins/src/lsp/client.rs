//! The language server client: the protocol half of the `lsp` tool.
//!
//! The flow answers the two questions worth asking — **what does the
//! compiler say about this file?** and **where does this symbol
//! live?**:
//!
//! 1. spawn the server and `initialize` it (the workspace root ✓);
//! 2. `initialized` — the handshake's second half ✓;
//! 3. `textDocument/didOpen` — the file's text, so the server can
//!    analyse it ✓;
//! 4. either wait for the server's `textDocument/publishDiagnostics`
//!    notification for that file ✓, or ask one of the four
//!    navigation requests and read its answer ✓;
//! 5. `shutdown` + `exit` — the server goes home ✓.
//!
//! The framing is LSP's own: `Content-Length` headers over stdio ✓
//! (the same shape the deployment's JSON-RPC uses ✓ — different
//! protocol, same envelope ✓).
//!
//! Everything the server sends is read by one task: the responses
//! answer the pending requests ✓ and the notifications update the
//! diagnostics ✓. The tool waits on the diagnostics it asked for ✓.
//!
//! The navigation operations are dsh's four ✓ — `goToDefinition`,
//! `findReferences`, `goToImplementation`, `hover` ✓ — over
//! `textDocument/definition`, `textDocument/references`,
//! `textDocument/implementation` and `textDocument/hover` ✓.
//! Positions cross the tool's boundary as 1-based line and column ✓
//! (what a model reads ✓) and leave it as the protocol's 0-based
//! UTF-16 ✓.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{Mutex, oneshot},
};

/// How long a diagnostic wait runs before the tool answers with what
/// it has.
pub const DEFAULT_DIAGNOSTIC_WAIT: Duration = Duration::from_secs(20);

/// The severities LSP names ✓.
pub(crate) fn severity_label(severity: Option<i64>) -> &'static str {
    match severity {
        Some(1) => "error",
        Some(2) => "warning",
        Some(3) => "info",
        Some(4) => "hint",
        _ => "diagnostic",
    }
}

/// One diagnostic the server published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// The 1-based line the diagnostic starts on ✓.
    pub line: usize,
    /// The 1-based column the diagnostic starts on ✓.
    pub column: usize,
    /// `"error"` | `"warning"` | `"info"` | `"hint"` | `"diagnostic"` ✓.
    pub severity: String,
    /// The message ✓.
    pub message: String,
    /// The diagnostic's source, when the server named one ✓.
    pub source: Option<String>,
}

/// The diagnostics one file has ✓.
pub type Diagnostics = Vec<Diagnostic>;

/// The navigation operations a language server answers ✓.
///
/// The four are dsh's ✓ (`packages/lsp/tool-lsp/src/index.ts`) ✓:
/// where a symbol is defined ✓, where it is used ✓, what implements
/// it ✓, and what the server says about it ✓.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    /// `textDocument/definition` ✓.
    Definition,
    /// `textDocument/references` ✓ — the declaration included ✓.
    References,
    /// `textDocument/implementation` ✓.
    Implementation,
    /// `textDocument/hover` ✓.
    Hover,
}

impl Operation {
    /// The protocol's method name ✓.
    #[must_use]
    pub fn method(self) -> &'static str {
        match self {
            Self::Definition => "textDocument/definition",
            Self::References => "textDocument/references",
            Self::Implementation => "textDocument/implementation",
            Self::Hover => "textDocument/hover",
        }
    }

    /// The capability the server must advertise for this operation ✓.
    #[must_use]
    pub fn capability(self) -> &'static str {
        match self {
            Self::Definition => "definitionProvider",
            Self::References => "referencesProvider",
            Self::Implementation => "implementationProvider",
            Self::Hover => "hoverProvider",
        }
    }

    /// The operation the model named ✓.
    ///
    /// The names are the ones the tool's description publishes ✓ —
    /// dsh's names ✓ — so the model's word and the protocol's method
    /// meet at one place ✓.
    ///
    /// # Errors
    ///
    /// The name is not one of the four ✓.
    pub fn parse(name: &str) -> Result<Self, LspError> {
        match name {
            "goToDefinition" | "definition" => Ok(Self::Definition),
            "findReferences" | "references" => Ok(Self::References),
            "goToImplementation" | "implementation" => Ok(Self::Implementation),
            "hover" => Ok(Self::Hover),
            other => Err(LspError::Unsupported(format!(
                "`{other}` is not a navigation operation"
            ))),
        }
    }
}

/// One place the server pointed at ✓.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    /// The file the place is in ✓.
    pub path: PathBuf,
    /// The 1-based line the place starts on ✓.
    pub line: usize,
    /// The 1-based column the place starts on ✓.
    pub column: usize,
}

/// What a navigation request answered ✓.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryResult {
    /// The places the server pointed at ✓.
    Locations(Vec<Location>),
    /// What the server says about the symbol ✓ — `None` when it says
    /// nothing ✓.
    Hover(Option<String>),
}

/// The file URI LSP names a path by ✓.
///
/// The percent-encoding is the one LSP requires ✓: a space in a path
/// is `%20` ✓, and a server that cannot map the URI back would
/// analyse the wrong file ✓.
pub fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for byte in path.to_string_lossy().as_bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'.'
            | b'_'
            | b'~'
            | b'/' => {
                uri.push(*byte as char);
            }
            other => uri.push_str(&format!("%{other:02X}")),
        }
    }
    uri
}

/// Why a language server could not answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspError {
    /// The server could not be started.
    Spawn(String),
    /// The server answered with an error.
    Failed(String),
    /// The server did not answer in time.
    Timeout,
    /// The server's answer was not the shape the protocol names.
    Protocol(String),
    /// The server cannot answer the question that was asked.
    Unsupported(String),
}

impl std::fmt::Display for LspError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(message) => {
                write!(f, "the language server could not start: {message}")
            }
            Self::Failed(message) => {
                write!(f, "the language server refused: {message}")
            }
            Self::Timeout => {
                write!(f, "the language server did not answer in time")
            }
            Self::Protocol(message) => {
                write!(
                    f,
                    "the language server's answer was malformed: {message}"
                )
            }
            Self::Unsupported(message) => {
                write!(f, "the language server cannot answer: {message}")
            }
        }
    }
}

/// The shared state the reader task owns ✓.
#[derive(Default)]
struct ServerState {
    /// The pending requests, by id ✓.
    pending: HashMap<i64, oneshot::Sender<Value>>,
    /// The diagnostics the server published, by file URI ✓.
    diagnostics: HashMap<String, Diagnostics>,
    /// The capabilities the server advertised ✓.
    capabilities: Value,
    /// The answers that arrived before their question did ✓.
    ///
    /// A server that speaks first is unusual ✓ — but dropping the
    /// answer would lose it ✓, so it waits here for its question ✓.
    early: HashMap<i64, Value>,
}

/// A language server, spawned for one workspace ✓.
pub struct LspClient {
    /// The server's process ✓.
    child: Arc<Mutex<Option<Child>>>,
    /// The server's input ✓.
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    /// The server's shared state ✓.
    state: Arc<Mutex<ServerState>>,
    /// The next request id ✓.
    next_id: Arc<Mutex<i64>>,
}

impl std::fmt::Debug for LspClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LspClient").finish_non_exhaustive()
    }
}

impl LspClient {
    /// Start the server for `root` and shake hands with it ✓.
    ///
    /// # Errors
    ///
    /// The server could not start, or the handshake failed ✓.
    pub async fn start(
        command: &str,
        args: &[String],
        root: &Path,
    ) -> Result<Self, LspError> {
        let mut child = Command::new(command)
            .args(args)
            .current_dir(root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| LspError::Spawn(error.to_string()))?;
        let stdin = child.stdin.take().ok_or_else(|| {
            LspError::Spawn("the server has no input".to_string())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            LspError::Spawn("the server has no output".to_string())
        })?;

        let client = Self {
            child: Arc::new(Mutex::new(Some(child))),
            stdin: Arc::new(Mutex::new(Some(stdin))),
            state: Arc::new(Mutex::new(ServerState::default())),
            next_id: Arc::new(Mutex::new(0)),
        };
        // One reader for the server's whole life ✓.
        let state = Arc::clone(&client.state);
        tokio::spawn(async move {
            read_frames(BufReader::new(stdout), state).await;
        });

        client.handshake(root).await?;
        Ok(client)
    }

    /// The `initialize` / `initialized` handshake ✓.
    async fn handshake(&self, root: &Path) -> Result<(), LspError> {
        let root_uri = file_uri(root);
        let result = self
            .request(
                "initialize",
                json!({
                    "processId": std::process::id(),
                    "rootUri": root_uri,
                    "capabilities": {
                        "general": {
                            "positionEncodings": ["utf-16"],
                        },
                        "textDocument": {
                            "publishDiagnostics": {},
                            "synchronization": {},
                        },
                    },
                    "clientInfo": {"name": "uva"},
                }),
            )
            .await?;
        let capabilities =
            result.get("capabilities").cloned().ok_or_else(|| {
                LspError::Protocol(
                    "the handshake carried no capabilities".to_string(),
                )
            })?;
        // The server may name the encoding it will use ✓; an omitted
        // one means `utf-16` ✓. Another one would make every
        // position wrong ✓, so the handshake refuses it ✓.
        if let Some(encoding) =
            capabilities.get("positionEncoding").and_then(Value::as_str)
            && encoding != "utf-16"
        {
            return Err(LspError::Unsupported(format!(
                "the server negotiated the `{encoding}` position \
                 encoding; this client speaks `utf-16`"
            )));
        }
        self.state.lock().await.capabilities = capabilities;
        self.notify("initialized", json!({})).await;
        Ok(())
    }

    /// Whether the server advertised the provider for `operation` ✓.
    async fn supports(&self, operation: Operation) -> bool {
        let capabilities = self.state.lock().await.capabilities.clone();
        match capabilities.get(operation.capability()) {
            Some(Value::Bool(yes)) => *yes,
            Some(Value::Object(_)) => true,
            _ => false,
        }
    }

    /// Open the file for the server ✓.
    ///
    /// A server that has not seen the file has nothing to say about
    /// it ✓ — neither a diagnostic nor a definition ✓.
    async fn open_document(&self, path: &Path, text: &str, language: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": file_uri(path),
                    "languageId": language,
                    "version": 1,
                    "text": text,
                },
            }),
        )
        .await;
    }

    /// The diagnostics the server published for `path` ✓.
    ///
    /// The file is opened for the server first ✓ — a server that has
    /// not seen the file has nothing to say about it ✓.
    ///
    /// # Errors
    ///
    /// The file could not be read, or the server could not be asked ✓.
    pub async fn diagnostics(
        &self,
        path: &Path,
        text: &str,
        language: &str,
        wait: Duration,
    ) -> Result<Diagnostics, LspError> {
        let uri = file_uri(path);
        self.open_document(path, text, language).await;

        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let published =
                self.state.lock().await.diagnostics.get(&uri).cloned();
            if let Some(diagnostics) = published {
                return Ok(diagnostics);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(LspError::Timeout);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// Ask the server one navigation question ✓.
    ///
    /// The server must advertise the provider ✓ — a server that
    /// cannot answer says so rather than answering nothing ✓. The
    /// file is opened first ✓ (a server that has not seen the file
    /// cannot point into it ✓), and the position crosses the wire as
    /// the protocol's 0-based UTF-16 ✓.
    ///
    /// # Errors
    ///
    /// The server does not advertise the operation ✓, refuses the
    /// request ✓, or answers with a shape the protocol does not name
    /// ✓.
    pub async fn query(
        &self,
        operation: Operation,
        path: &Path,
        text: &str,
        language: &str,
        line: usize,
        character: usize,
    ) -> Result<QueryResult, LspError> {
        if !self.supports(operation).await {
            return Err(LspError::Unsupported(format!(
                "the server does not advertise `{}`",
                operation.capability()
            )));
        }
        self.open_document(path, text, language).await;
        let mut params = json!({
            "textDocument": {"uri": file_uri(path)},
            "position": {
                "line": line.saturating_sub(1),
                "character": character.saturating_sub(1),
            },
        });
        if operation == Operation::References {
            // The declaration is a reference too ✓ — dsh's tool says
            // so in its own description ✓.
            params["context"] = json!({"includeDeclaration": true});
        }
        let answer = self.request(operation.method(), params).await?;
        Ok(match operation {
            Operation::Hover => QueryResult::Hover(hover_text(&answer)),
            _ => QueryResult::Locations(locations_from(&answer)),
        })
    }

    /// Send one request and wait for its answer ✓.
    async fn request(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, LspError> {
        let id = {
            let mut next = self.next_id.lock().await;
            *next += 1;
            *next
        };
        let (tx, rx) = oneshot::channel();
        {
            let mut state = self.state.lock().await;
            // An answer that came early waits for its question ✓.
            if let Some(answer) = state.early.remove(&id) {
                return result_of(&answer);
            }
            state.pending.insert(id, tx);
        }
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .await?;
        let answer = rx.await.map_err(|_| {
            LspError::Failed("the server closed the request".to_string())
        })?;
        result_of(&answer)
    }

    /// Send one notification ✓.
    async fn notify(&self, method: &str, params: Value) {
        let _ = self
            .send(&json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": params,
            }))
            .await;
    }

    /// Write one frame ✓.
    async fn send(&self, frame: &Value) -> Result<(), LspError> {
        let payload = serde_json::to_vec(frame)
            .map_err(|error| LspError::Protocol(error.to_string()))?;
        let mut stdin = self.stdin.lock().await;
        let Some(stdin) = stdin.as_mut() else {
            return Err(LspError::Failed("the server is gone".to_string()));
        };
        stdin
            .write_all(
                format!("Content-Length: {}\r\n\r\n", payload.len()).as_bytes(),
            )
            .await
            .map_err(|error| LspError::Failed(error.to_string()))?;
        stdin
            .write_all(&payload)
            .await
            .map_err(|error| LspError::Failed(error.to_string()))?;
        stdin
            .flush()
            .await
            .map_err(|error| LspError::Failed(error.to_string()))
    }

    /// The server goes home ✓.
    ///
    /// The protocol's `shutdown` request is not awaited: the tool
    /// owns the server for one call, so the exit is the exit
    /// notification plus the process going away ✓ — waiting for a
    /// handshake the tool does not need would only add a hang ✓.
    pub async fn shutdown(&self) {
        self.notify("exit", Value::Null).await;
        if let Some(mut child) = self.child.lock().await.take() {
            let _ = child.kill().await;
        }
    }
}

/// Read the server's frames until its output ends ✓.
async fn read_frames(
    mut stdout: BufReader<ChildStdout>,
    state: Arc<Mutex<ServerState>>,
) {
    loop {
        let Ok(Some(length)) = read_length(&mut stdout).await else {
            return;
        };
        let mut payload = vec![0u8; length];
        if stdout.read_exact(&mut payload).await.is_err() {
            return;
        }
        let Ok(message) = serde_json::from_slice::<Value>(&payload) else {
            continue;
        };
        dispatch(message, &state).await;
    }
}

/// Read one frame's header, returning the payload's length ✓.
async fn read_length(
    stdout: &mut BufReader<ChildStdout>,
) -> std::io::Result<Option<usize>> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if stdout.read_line(&mut line).await? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            return Ok(length);
        }
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            length = value.trim().parse::<usize>().ok();
        }
    }
}

/// Route one message: a response answers its request ✓, a
/// notification updates the diagnostics ✓.
async fn dispatch(message: Value, state: &Arc<Mutex<ServerState>>) {
    if let Some(id) = message.get("id").and_then(Value::as_i64) {
        let mut state = state.lock().await;
        if let Some(sender) = state.pending.remove(&id) {
            let _ = sender.send(message);
        } else {
            state.early.insert(id, message);
        }
        return;
    }
    if message.get("method").and_then(Value::as_str)
        == Some("textDocument/publishDiagnostics")
    {
        let Some(params) = message.get("params") else {
            return;
        };
        let Some(uri) = params.get("uri").and_then(Value::as_str) else {
            return;
        };
        let diagnostics = params
            .get("diagnostics")
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(diagnostic_from).collect())
            .unwrap_or_default();
        state
            .lock()
            .await
            .diagnostics
            .insert(uri.to_string(), diagnostics);
    }
}

/// The result one answer carries ✓.
///
/// An answer with an `error` is the server's refusal ✓ — the tool
/// reports it as one ✓.
fn result_of(answer: &Value) -> Result<Value, LspError> {
    if let Some(error) = answer.get("error") {
        return Err(LspError::Failed(error.to_string()));
    }
    Ok(answer.get("result").cloned().unwrap_or(Value::Null))
}

/// The places a navigation request answered ✓.
///
/// The protocol names three shapes ✓: one `Location` ✓, an array of
/// them ✓, or an array of `LocationLink` ✓ (a link carries the
/// target's uri and the range to select in it ✓). `null` means "no
/// place" ✓ — an empty answer, not an error ✓.
fn locations_from(answer: &Value) -> Vec<Location> {
    match answer {
        Value::Array(items) => items.iter().filter_map(location_from).collect(),
        Value::Object(_) => location_from(answer).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// One place, read from the wire's shape ✓.
fn location_from(value: &Value) -> Option<Location> {
    let (uri, range) =
        if let Some(uri) = value.get("uri").and_then(Value::as_str) {
            (uri, value.get("range"))
        } else {
            let uri = value.get("targetUri").and_then(Value::as_str)?;
            (
                uri,
                value
                    .get("targetSelectionRange")
                    .or_else(|| value.get("targetRange")),
            )
        };
    let path = path_from_uri(uri)?;
    let start = range?.get("start")?;
    let line = start.get("line").and_then(Value::as_i64).unwrap_or(0) + 1;
    let column =
        start.get("character").and_then(Value::as_i64).unwrap_or(0) + 1;
    Some(Location {
        path,
        line: line.max(1) as usize,
        column: column.max(1) as usize,
    })
}

/// What a hover answered ✓.
///
/// The protocol's contents are a string ✓, a `MarkupContent` ✓, a
/// `MarkedString` ✓, or an array of those ✓ — all of them carry
/// their text in `value` ✓ (a plain string is its own text ✓).
fn hover_text(answer: &Value) -> Option<String> {
    hover_contents(answer.get("contents")?)
}

/// The text one hover content carries ✓.
fn hover_contents(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Object(map) => {
            map.get("value").and_then(Value::as_str).map(str::to_string)
        }
        Value::Array(items) => {
            let parts: Vec<String> =
                items.iter().filter_map(hover_contents).collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        _ => None,
    }
}

/// One published diagnostic, read from the wire's shape ✓.
fn diagnostic_from(value: &Value) -> Option<Diagnostic> {
    let range = value.get("range")?;
    let start = range.get("start")?;
    let line = start.get("line").and_then(Value::as_i64).unwrap_or(0) + 1;
    let column =
        start.get("character").and_then(Value::as_i64).unwrap_or(0) + 1;
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Some(Diagnostic {
        line: line.max(1) as usize,
        column: column.max(1) as usize,
        severity: severity_label(value.get("severity").and_then(Value::as_i64))
            .to_string(),
        message,
        source: value
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// The path a file URI names ✓.
pub fn path_from_uri(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let mut out = Vec::with_capacity(rest.len());
    let bytes = rest.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    Some(PathBuf::from(String::from_utf8(out).ok()?))
}
