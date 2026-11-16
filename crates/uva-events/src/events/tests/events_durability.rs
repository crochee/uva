//! `AgentEvent::is_durable` matrix. The model family is never
//! durable: the harness writes those facts to the log itself, so
//! the host persists nothing from the event stream.

use super::{
    super::{AgentEvent, SystemEvent, WarningKind},
    helpers::{
        make_image,
        make_reasoning,
        make_resource,
        make_text,
        make_tool_result,
        make_tool_use,
        sampling_result_for_test,
    },
};

#[test]
fn is_durable_matrix() {
    assert!(!AgentEvent::Model(make_text("hello")).is_durable());
    assert!(!AgentEvent::Model(make_tool_use()).is_durable());
    assert!(!AgentEvent::Model(make_tool_result()).is_durable());
    assert!(!AgentEvent::Model(make_resource()).is_durable());

    assert!(!AgentEvent::Model(make_reasoning("x")).is_durable());
    assert!(!AgentEvent::Model(make_image()).is_durable());
    // Audio-ephemeral pinned through a JSON round trip.
    let audio_json = serde_json::json!({
        "type": "audio",
        "data": "",
        "mime_type": "audio/wav",
        "format": null
    });
    let audio_part: uva_provider::ContentPart =
        serde_json::from_value(audio_json).expect("parse audio");
    assert!(
        !AgentEvent::Model(audio_part).is_durable(),
        "Audio content parts MUST be ephemeral"
    );

    assert!(!AgentEvent::ModelDone(sampling_result_for_test()).is_durable());
    assert!(
        !AgentEvent::System(SystemEvent::Warning {
            kind: WarningKind::Hook,
            message: "disk".to_string(),
            iteration: None,
        })
        .is_durable()
    );
}
