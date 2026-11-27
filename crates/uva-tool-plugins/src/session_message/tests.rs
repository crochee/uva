//! The session-message tool's own tests: the delivery contract, the
//! refusals, and the messenger seam the tool calls through.

use std::sync::Mutex;

use serde_json::json;

use super::*;

/// A messenger the tests script: it answers with a fixed delivery and
/// remembers what it was asked to carry.
#[derive(Default)]
struct ScriptedMessenger {
    delivered: Mutex<Vec<(String, String, String)>>,
    answer: Option<Delivery>,
}

#[async_trait]
impl SessionMessenger for ScriptedMessenger {
    async fn send(
        &self,
        caller: &str,
        target: &str,
        message: &str,
    ) -> Result<Delivery, String> {
        if let Some(delivery) = self.answer {
            self.delivered.lock().expect("delivered mutex").push((
                caller.to_string(),
                target.to_string(),
                message.to_string(),
            ));
            return Ok(delivery);
        }
        Err(format!("the session '{target}' is not known"))
    }
}

/// The messenger as the seam's trait object, keeping the concrete
/// handle for the assertions.
fn messenger_of(
    messenger: &Arc<ScriptedMessenger>,
) -> Arc<dyn SessionMessenger> {
    messenger.clone()
}

fn make_context() -> Context {
    Context::new("caller".to_string(), std::path::PathBuf::from("/tmp"))
}

fn first_text(out: &ToolOutput) -> String {
    out.content
        .iter()
        .find_map(|part| part.text().map(str::to_string))
        .unwrap_or_default()
}

#[test]
fn the_tool_is_named_for_the_model() {
    let tool = SessionMessageTool::new(Arc::new(ScriptedMessenger::default()));
    assert_eq!(tool.name(), "session_send_message");
}

#[test]
fn the_delivery_runs_alone() {
    let tool = SessionMessageTool::new(Arc::new(ScriptedMessenger::default()));
    assert_eq!(tool.mode(), ExecutionMode::Sequential);
}

#[tokio::test]
async fn the_message_reaches_the_messenger_with_the_caller() {
    let messenger = Arc::new(ScriptedMessenger {
        answer: Some(Delivery::Steer),
        ..ScriptedMessenger::default()
    });
    let tool = SessionMessageTool::new(messenger_of(&messenger));
    let out = tool
        .call(
            json!({"session_id": "target", "message": "the plan changed"}),
            &make_context(),
        )
        .await;
    assert!(out.is_error.is_none(), "{:?}", out.content);
    let delivered = messenger.delivered.lock().expect("delivered mutex");
    assert_eq!(
        delivered.as_slice(),
        [(
            "caller".to_string(),
            "target".to_string(),
            "the plan changed".to_string(),
        )],
        "the messenger carries the caller, the target and the message"
    );
    drop(delivered);
    let text = first_text(&out);
    assert!(
        text.contains("Sent to session 'target'")
            && text.contains("running turn"),
        "the answer says where it landed: {text}"
    );
}

#[tokio::test]
async fn a_queued_delivery_says_so() {
    let tool = SessionMessageTool::new(Arc::new(ScriptedMessenger {
        answer: Some(Delivery::Queued),
        ..ScriptedMessenger::default()
    }));
    let out = tool
        .call(
            json!({"session_id": "target", "message": "hello"}),
            &make_context(),
        )
        .await;
    assert!(first_text(&out).contains("queued"));
}

#[tokio::test]
async fn a_message_to_this_session_is_refused() {
    let messenger = Arc::new(ScriptedMessenger {
        answer: Some(Delivery::Steer),
        ..ScriptedMessenger::default()
    });
    let tool = SessionMessageTool::new(messenger_of(&messenger));
    let out = tool
        .call(
            json!({"session_id": "caller", "message": "hello"}),
            &make_context(),
        )
        .await;
    assert_eq!(out.is_error, Some(true));
    assert!(
        first_text(&out).contains("the session you are running in"),
        "the refusal tells the model to speak directly: {}",
        first_text(&out)
    );
    assert!(
        messenger
            .delivered
            .lock()
            .expect("delivered mutex")
            .is_empty(),
        "a self-message never travels"
    );
}

#[tokio::test]
async fn a_blank_message_never_travels() {
    let messenger = Arc::new(ScriptedMessenger {
        answer: Some(Delivery::Steer),
        ..ScriptedMessenger::default()
    });
    let tool = SessionMessageTool::new(messenger_of(&messenger));
    let out = tool
        .call(
            json!({"session_id": "target", "message": "  "}),
            &make_context(),
        )
        .await;
    assert_eq!(out.is_error, Some(true));
    assert!(
        messenger
            .delivered
            .lock()
            .expect("delivered mutex")
            .is_empty()
    );
}

#[tokio::test]
async fn an_unknown_target_carries_the_reason() {
    let tool = SessionMessageTool::new(Arc::new(ScriptedMessenger::default()));
    let out = tool
        .call(
            json!({"session_id": "target", "message": "hello"}),
            &make_context(),
        )
        .await;
    assert_eq!(out.is_error, Some(true));
    assert!(
        first_text(&out).contains("'target' is not known"),
        "the refusal names the target"
    );
}

#[tokio::test]
async fn without_a_messenger_the_delivery_fails_closed() {
    let tool = SessionMessageTool::new(Arc::new(NoSessions));
    let out = tool
        .call(
            json!({"session_id": "target", "message": "hello"}),
            &make_context(),
        )
        .await;
    assert_eq!(out.is_error, Some(true));
}
