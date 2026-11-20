//! The mount-point vocabulary: one wire spelling per point, and the
//! three groups the table dispatches over.

use std::collections::HashSet;

use super::{MountPoint, RegistrationOrigin};
use crate::arbitration::PanicPolicy;

#[test]
fn every_mount_point_round_trips_its_one_wire_spelling() {
    let mut spellings = HashSet::new();
    for mount in MountPoint::ALL {
        let wire = mount.as_str();
        assert!(spellings.insert(wire), "`{wire}` names two points");
        assert_eq!(
            MountPoint::parse(wire),
            Some(mount),
            "`{wire}` must parse back to its declaration"
        );
        assert_eq!(mount.to_string(), wire, "Display is the wire spelling");
    }
    assert_eq!(spellings.len(), 12, "twelve points, twelve spellings");
}

#[test]
fn the_six_pass_points_keep_their_kernel_key_spellings() {
    let cases = [
        (MountPoint::StepPre, "agent/step-pre"),
        (MountPoint::StepPost, "agent/step-post"),
        (MountPoint::ToolPre, "tool/pre-execute"),
        (MountPoint::ToolPost, "tool/post-execute"),
        (MountPoint::LlmRequest, "llm/request"),
        (MountPoint::LlmResult, "llm/result"),
    ];
    for (mount, key) in cases {
        assert_eq!(mount.as_str(), key, "{mount} keeps its pass key");
        assert!(mount.is_pass(), "{mount} is a pass point");
        assert!(!mount.is_observation(), "{mount} is not an observation");
    }
}

#[test]
fn the_five_observation_points_keep_their_event_spellings() {
    let cases = [
        (MountPoint::BeforeRun, "before_run"),
        (MountPoint::AfterRun, "after_run"),
        (MountPoint::BeforeProviderCall, "before_provider_call"),
        (MountPoint::OnError, "on_error"),
        (MountPoint::OnCompaction, "on_compaction"),
    ];
    for (mount, event) in cases {
        assert_eq!(mount.as_str(), event, "{mount} keeps its event name");
        assert!(mount.is_observation(), "{mount} is an observation");
        assert!(!mount.is_pass(), "{mount} is not a pass point");
    }
}

#[test]
fn the_collection_point_is_not_an_event_point() {
    assert!(MountPoint::PromptSections.is_sections());
    assert_eq!(MountPoint::PromptSections.as_str(), "prompt/sections");
    assert!(!MountPoint::PromptSections.is_event());
    for mount in MountPoint::ALL {
        assert_eq!(mount.is_event(), !mount.is_sections(), "{mount}");
        assert!(
            !(mount.is_pass() && mount.is_observation()),
            "{mount} belongs to one group"
        );
    }
}

#[test]
fn the_twelve_points_split_six_passes_five_observations_one_collection() {
    let passes = MountPoint::ALL
        .iter()
        .filter(|mount| mount.is_pass())
        .count();
    let observations = MountPoint::ALL
        .iter()
        .filter(|mount| mount.is_observation())
        .count();
    let collections = MountPoint::ALL
        .iter()
        .filter(|mount| mount.is_sections())
        .count();
    assert_eq!((passes, observations, collections), (6, 5, 1));
}

#[test]
fn parse_refuses_any_other_spelling() {
    for wire in ["tool/execute", "TOOL_PRE", "before-run", ""] {
        assert_eq!(MountPoint::parse(wire), None, "`{wire}`");
    }
}

#[test]
fn the_origin_declares_its_lane_and_panic_policy() {
    assert_eq!(RegistrationOrigin::Plugin.panic_policy(), PanicPolicy::Keep);
    assert_eq!(
        RegistrationOrigin::InProcess.panic_policy(),
        PanicPolicy::Withdraw
    );
    assert!(
        RegistrationOrigin::Plugin.lane()
            < RegistrationOrigin::InProcess.lane(),
        "the plugin lane runs first"
    );
}
