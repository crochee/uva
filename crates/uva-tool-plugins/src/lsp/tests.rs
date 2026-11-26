//! The `lsp` tool's own tests: the server's diagnostics reach the
//! model, and a deployment without a server for the language says so.

use std::{path::PathBuf, sync::Arc};

use serde_json::json;

use super::*;

/// A deployment with one server for `rust` ✓.
struct OneServer(Arc<LspServer>);

impl LspServers for OneServer {
    fn servers(&self) -> Vec<LspServer> {
        vec![(*self.0).clone()]
    }
}

/// A fake language server: it answers the handshake and publishes one
/// diagnostic for the file the tool opened ✓.
///
/// The server never reads the client's frames — the tool kills it when
/// the call ends ✓ — so the script only has to write the two frames
/// the tool waits for ✓.
fn fake_server(workspace: &std::path::Path, uri: &str) -> PathBuf {
    fake_server_with(workspace, uri, "{}", "")
}

/// A fake language server that also answers one navigation request ✓.
///
/// The server never reads the client's frames — the tool kills it when
/// the call ends ✓ — so the script writes the frames the tool waits
/// for ✓. The request's id is the second one ✓ (the handshake's
/// answer is the first ✓).
fn fake_server_with(
    workspace: &std::path::Path,
    uri: &str,
    capabilities: &str,
    navigation: &str,
) -> PathBuf {
    let path = workspace.join("fake-lsp.sh");
    let response = format!(
        r#"{{"jsonrpc":"2.0","id":1,"result":{{"capabilities":{capabilities}}}}}"#
    );
    let diagnostics = format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{{"uri":"{uri}","diagnostics":[{{"range":{{"start":{{"line":4,"character":2}},"end":{{"line":4,"character":8}}}},"severity":1,"message":"cannot find value `x` in this scope","source":"rustc"}}]}}}}"#
    );
    let navigation_frame = if navigation.is_empty() {
        String::new()
    } else {
        format!(
            "printf 'Content-Length: %s\\r\\n\\r\\n%s' {len_navigation} '{navigation}'\n",
            len_navigation = navigation.len(),
        )
    };
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\n\
             printf 'Content-Length: %s\\r\\n\\r\\n%s' {len_response} '{response}'\n\
             printf 'Content-Length: %s\\r\\n\\r\\n%s' {len_diagnostics} '{diagnostics}'\n\
             {navigation_frame}\
             sleep 5\n",
            len_response = response.len(),
            len_diagnostics = diagnostics.len(),
        ),
    )
    .expect("the fake server is written");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions =
            std::fs::metadata(&path).expect("the script").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions)
            .expect("the script is runnable");
    }
    path
}

// ---- Tool metadata --------------------------------------------

#[test]
fn tool_uses_agent_facing_name() {
    let tool = LspTool::new(Arc::new(OneServer(Arc::new(LspServer {
        language: "rust".to_string(),
        command: "true".to_string(),
        args: Vec::new(),
    }))));
    assert_eq!(tool.name(), "lsp");
}

#[test]
fn the_schema_publishes_the_operations_the_model_may_ask_for() {
    let tool = LspTool::new(Arc::new(OneServer(Arc::new(LspServer {
        language: "rust".to_string(),
        command: "true".to_string(),
        args: Vec::new(),
    }))));
    let schema = tool.parameters();
    // The enum is a named definition: the property points at it.
    let property = schema
        .pointer("/properties/operation")
        .expect("the operation property is there");
    let reference = property
        .get("$ref")
        .and_then(|reference| reference.as_str())
        .expect("the operation is a named enum");
    let definition = reference
        .rsplit('/')
        .next()
        .expect("the reference names a definition");
    // The enum's names are the `const` of each arm.
    let operations = schema
        .pointer(&format!("/$defs/{definition}/oneOf"))
        .and_then(|arms| arms.as_array())
        .expect("the operation names are an enum")
        .iter()
        .filter_map(|arm| arm.get("const").and_then(|name| name.as_str()))
        .collect::<Vec<&str>>();
    assert_eq!(
        operations,
        vec![
            "diagnostics",
            "goToDefinition",
            "findReferences",
            "goToImplementation",
            "hover"
        ],
        "the model sees the reference harness's operation names"
    );
}

#[test]
fn asking_a_server_runs_alone() {
    let tool = LspTool::new(Arc::new(OneServer(Arc::new(LspServer {
        language: "rust".to_string(),
        command: "true".to_string(),
        args: Vec::new(),
    }))));
    assert_eq!(tool.mode(), ExecutionMode::Sequential);
}

// ---- The contract ----------------------------------------------

#[tokio::test]
async fn the_servers_diagnostics_reach_the_model() {
    let dir = tempfile::tempdir().expect("a temp workspace");
    let file = dir.path().join("main.rs");
    std::fs::write(&file, "fn main() { let x = 1; }\n").expect("the file");
    let uri = client::file_uri(&file);
    let server = fake_server(dir.path(), &uri);
    let tool = LspTool::new(Arc::new(OneServer(Arc::new(LspServer {
        language: "rust".to_string(),
        command: server.to_string_lossy().into_owned(),
        args: Vec::new(),
    }))));
    let context = Context::new("s1".to_string(), dir.path().to_path_buf());

    let out = tool
        .call(
            json!({ "file_path": "main.rs", "language": "rust" }),
            &context,
        )
        .await;

    let text = out
        .content
        .iter()
        .find_map(|part| part.text().map(str::to_string))
        .unwrap_or_default();
    assert!(
        text.contains("error") && text.contains("cannot find value"),
        "the server's diagnostic reaches the model: {text}"
    );
    assert!(
        text.contains("main.rs:5:3"),
        "the diagnostic names the place: {text}"
    );
}

#[tokio::test]
async fn a_language_without_a_server_is_refused_by_name() {
    let dir = tempfile::tempdir().expect("a temp workspace");
    let tool = LspTool::new(Arc::new(OneServer(Arc::new(LspServer {
        language: "rust".to_string(),
        command: "true".to_string(),
        args: Vec::new(),
    }))));
    let context = Context::new("s1".to_string(), dir.path().to_path_buf());

    let out = tool
        .call(
            json!({ "file_path": "main.ts", "language": "typescript" }),
            &context,
        )
        .await;

    let text = out
        .content
        .iter()
        .find_map(|part| part.text().map(str::to_string))
        .unwrap_or_default();
    assert!(
        text.contains("no language server for `typescript`"),
        "the refusal names the language: {text}"
    );
    assert!(
        text.contains("rust"),
        "the refusal names what the deployment knows: {text}"
    );
}

// ---- The navigation face ---------------------------------------

#[tokio::test]
async fn a_navigation_operation_answers_with_the_places_the_server_pointed_at()
{
    let dir = tempfile::tempdir().expect("a temp workspace");
    let file = dir.path().join("main.rs");
    std::fs::write(&file, "fn main() {}\n").expect("the file");
    let uri = client::file_uri(&file);
    let answer = format!(
        r#"{{"jsonrpc":"2.0","id":2,"result":[{{"uri":"{uri}","range":{{"start":{{"line":9,"character":4}},"end":{{"line":9,"character":9}}}}}}]}}"#
    );
    let server = fake_server_with(
        dir.path(),
        &uri,
        r#"{"definitionProvider":true}"#,
        &answer,
    );
    let tool = LspTool::new(Arc::new(OneServer(Arc::new(LspServer {
        language: "rust".to_string(),
        command: server.to_string_lossy().into_owned(),
        args: Vec::new(),
    }))));
    let context = Context::new("s1".to_string(), dir.path().to_path_buf());

    let out = tool
        .call(
            json!({
                "file_path": "main.rs",
                "language": "rust",
                "operation": "goToDefinition",
                "line": 10,
                "character": 5,
            }),
            &context,
        )
        .await;

    let text = out
        .content
        .iter()
        .find_map(|part| part.text().map(str::to_string))
        .unwrap_or_default();
    assert_eq!(text, "main.rs:10:5", "the place reaches the model: {text}");
}

#[tokio::test]
async fn a_server_that_cannot_answer_says_so() {
    let dir = tempfile::tempdir().expect("a temp workspace");
    let file = dir.path().join("main.rs");
    std::fs::write(&file, "fn main() {}\n").expect("the file");
    let uri = client::file_uri(&file);
    // The server advertises no provider ✓.
    let server = fake_server_with(dir.path(), &uri, "{}", "");
    let tool = LspTool::new(Arc::new(OneServer(Arc::new(LspServer {
        language: "rust".to_string(),
        command: server.to_string_lossy().into_owned(),
        args: Vec::new(),
    }))));
    let context = Context::new("s1".to_string(), dir.path().to_path_buf());

    let out = tool
        .call(
            json!({
                "file_path": "main.rs",
                "language": "rust",
                "operation": "hover",
                "line": 1,
                "character": 1,
            }),
            &context,
        )
        .await;

    let text = out
        .content
        .iter()
        .find_map(|part| part.text().map(str::to_string))
        .unwrap_or_default();
    assert!(
        text.contains("does not advertise `hoverProvider`"),
        "the refusal names the missing provider: {text}"
    );
}

#[tokio::test]
async fn a_navigation_operation_needs_the_cursor() {
    let dir = tempfile::tempdir().expect("a temp workspace");
    let tool = LspTool::new(Arc::new(OneServer(Arc::new(LspServer {
        language: "rust".to_string(),
        command: "true".to_string(),
        args: Vec::new(),
    }))));
    let context = Context::new("s1".to_string(), dir.path().to_path_buf());

    let out = tool
        .call(
            json!({
                "file_path": "main.rs",
                "language": "rust",
                "operation": "goToDefinition",
            }),
            &context,
        )
        .await;

    let text = out
        .content
        .iter()
        .find_map(|part| part.text().map(str::to_string))
        .unwrap_or_default();
    assert!(
        text.contains("needs the cursor"),
        "the refusal names what is missing: {text}"
    );
}

#[test]
fn an_operation_name_is_the_one_the_reference_harness_publishes() {
    assert_eq!(
        client::Operation::parse("goToDefinition"),
        Ok(client::Operation::Definition)
    );
    assert_eq!(
        client::Operation::parse("findReferences"),
        Ok(client::Operation::References)
    );
    assert_eq!(
        client::Operation::parse("goToImplementation"),
        Ok(client::Operation::Implementation)
    );
    assert_eq!(
        client::Operation::parse("hover"),
        Ok(client::Operation::Hover)
    );
    assert!(
        client::Operation::parse("formatDocument").is_err(),
        "an operation the protocol does not name is refused"
    );
}

// ---- The protocol's shapes -------------------------------------

#[test]
fn a_file_uri_percent_encodes_what_it_must() {
    let uri = client::file_uri(std::path::Path::new("/tmp/a b/c.rs"));
    assert_eq!(uri, "file:///tmp/a%20b/c.rs");
    assert_eq!(
        client::path_from_uri("file:///tmp/a%20b/c.rs"),
        Some(PathBuf::from("/tmp/a b/c.rs"))
    );
}

#[test]
fn a_severity_is_named_the_way_the_protocol_names_it() {
    assert_eq!(client::severity_label(Some(1)), "error");
    assert_eq!(client::severity_label(Some(2)), "warning");
    assert_eq!(client::severity_label(Some(3)), "info");
    assert_eq!(client::severity_label(Some(4)), "hint");
    assert_eq!(client::severity_label(None), "diagnostic");
}
