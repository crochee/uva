//! Contract test for [`ToolDescriptor::visibility`].
//!
//! The registry historically answered "is this tool advertised?"
//! from two independent fields: [`ToolDescriptor::is_hidden`]
//! (privacy flag, refuses dispatch) and [`ToolDescriptor::exposure`]
//! / [`ToolExposure`] (prompt-economy knob, advertisement-only).
//!
//! [`ToolDescriptor::visibility`] is the **single source of truth**
//! the projection consumes, derived from those two fields. The
//! rules below pin the derivation so future drift between the two
//! fields and `visibility()` is caught by `cargo test`:
//!
//! | `is_hidden` | `exposure` | `visibility()` |
//! |---|---|---|
//! | `true` | any | `Hidden` (privacy wins) |
//! | `false` | `Direct` | `Visible` |
//! | `false` | `Deferred` | `Deferred` |
//! | `false` | `Hidden` | `Hidden` |
//!
//! [`ToolDescriptor::is_hidden`]: uva_tool::ToolDescriptor::is_hidden
//! [`ToolDescriptor::exposure`]: uva_tool::ToolDescriptor::exposure

use uva_tool::{
    ToolAnnotations,
    ToolCategory,
    ToolDescriptor,
    ToolExposure,
    Visibility,
};

fn descriptor(is_hidden: bool, exposure: ToolExposure) -> ToolDescriptor {
    ToolDescriptor {
        name: "stub".to_string(),
        description: "stub".to_string(),
        parameters: serde_json::json!({}),
        category: ToolCategory::Utility,
        is_hidden,
        exposure,
        annotations: None,
    }
}

#[test]
fn hidden_flag_dominates_any_exposure() {
    for exposure in [
        ToolExposure::Direct,
        ToolExposure::Deferred,
        ToolExposure::Hidden,
    ] {
        let d = descriptor(true, exposure);
        assert_eq!(
            d.visibility(),
            Visibility::Hidden,
            "is_hidden=true dominates: exposure={exposure:?}"
        );
    }
}

#[test]
fn direct_exposure_is_visible() {
    assert_eq!(
        descriptor(false, ToolExposure::Direct).visibility(),
        Visibility::Visible
    );
}

#[test]
fn deferred_exposure_is_deferred() {
    assert_eq!(
        descriptor(false, ToolExposure::Deferred).visibility(),
        Visibility::Deferred
    );
}

#[test]
fn hidden_exposure_with_visible_flag_is_hidden() {
    // The softer `ToolExposure::Hidden` is advertisement-only, but
    // the resulting visibility is still Hidden—the projection
    // must not advertise it.
    assert_eq!(
        descriptor(false, ToolExposure::Hidden).visibility(),
        Visibility::Hidden
    );
}

#[test]
fn visibility_default_is_visible() {
    // `Default` for `Visibility` is `Visible`—the most permissive
    // state—so a freshly-registered tool with no exposure override
    // lands Visible. This matches `ToolExposure::default() = Direct`.
    assert_eq!(Visibility::default(), Visibility::Visible);
}

#[test]
fn visibility_serializes_snake_case() {
    // The on-wire form is the same snake_case as `ToolExposure` so a
    // future cutover () can swap `exposure` -> `visibility` in a
    // descriptor's JSON without breaking parsers.
    for (v, expected) in [
        (Visibility::Visible, "\"visible\""),
        (Visibility::Deferred, "\"deferred\""),
        (Visibility::Hidden, "\"hidden\""),
    ] {
        assert_eq!(serde_json::to_string(&v).unwrap(), expected);
    }
}

#[test]
fn tool_annotations_default_does_not_change_visibility() {
    // Sanity: annotations are advisory (MCP hints); they must not
    // influence visibility, and `Visibility` derivation must remain
    // stable whether annotations are present or absent.
    let without = descriptor(false, ToolExposure::Direct);
    let mut with = descriptor(false, ToolExposure::Direct);
    with.annotations = Some(ToolAnnotations {
        read_only_hint: Some(true),
        destructive_hint: None,
        idempotent_hint: None,
        open_world_hint: None,
    });
    assert_eq!(without.visibility(), with.visibility());
    assert_eq!(without.visibility(), Visibility::Visible);
}
