//! A third-party tool plugin, written the way a plugin author would
//! write it: implement the trait, register the tool, call it.
//!
//! The framework entry is **one dependency** (`uva`) — the `Tool`
//! trait, the registry and the kernel plugin contract all come from
//! it, so a plugin author never has to hunt for "which crate holds the
//! seam I implement".

use std::sync::Arc;

use uva::tool::{Context, Tool, ToolEntry, ToolOutput, ToolRegistry};
use uva::prelude::async_trait;

/// The plugin's tool: a greeting with a name.
struct GreetTool;

#[async_trait]
impl Tool for GreetTool {
    fn name(&self) -> &str {
        "greet"
    }

    fn description(&self) -> &str {
        "Greet someone by name."
    }

    fn parameters(&self) -> uva::serde_json::Value {
        uva::serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Who to greet." }
            },
            "required": ["name"]
        })
    }

    async fn call(
        &self,
        input: uva::serde_json::Value,
        _context: &Context,
    ) -> ToolOutput {
        // A tool parses its own input and reports a *model-facing*
        // error instead of panicking.
        let Some(name) = input.get("name").and_then(|v| v.as_str()) else {
            return ToolOutput::error("missing argument: name");
        };
        ToolOutput::text(format!("hello, {name}"))
    }
}

fn main() {
    // 1. compose the tool surface: one registry, the tools you want.
    let registry = ToolRegistry::new();
    assert!(
        registry.register_entry(ToolEntry::new(Arc::new(GreetTool))),
        "the tool registers"
    );

    // 2. call it, the way the agent loop would.
    let output = futures::executor::block_on(async {
        GreetTool
            .call(
                uva::serde_json::json!({ "name": "world" }),
                &Context::default(),
            )
            .await
    });

    let text = output
        .content
        .iter()
        .filter_map(|part| match part {
            uva::provider::ContentPart::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    assert_eq!(text, "hello, world");

    println!("PLUGIN-AUTHOR: OK");
}
