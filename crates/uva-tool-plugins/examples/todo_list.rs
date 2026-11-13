//! Runs `TodoWriteTool` over a three-item task list and prints the
//! rendered checklist.
use serde_json::json;
use uva_tool::{Context, Tool};
use uva_tool_plugins::todo::TodoWriteTool;

#[tokio::main]
async fn main() {
    let tool = TodoWriteTool::new();
    let context =
        Context::new("example".to_string(), std::path::PathBuf::from("/tmp"));

    let input = json!({
        "todos": [
            {"id": "t-1", "content": "Scaffold crate", "status": "completed"},
            {"id": "t-2", "content": "Transplant tool", "status": "in_progress"},
            {"id": "t-3", "content": "Verify tests", "status": "pending"}
        ]
    });

    let out = tool.call(input, &context).await;
    assert!(
        out.is_error.is_none(),
        "tool call failed: {:?}",
        out.is_error
    );

    for part in &out.content {
        if let uva_provider::types::ContentPart::Text(t) = part {
            println!("{}", t.text);
        }
    }

    println!("TODO-OK");
}
