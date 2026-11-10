//! Tests for the row enablement field and the effective row set.

use crate::profile::{EnableCheck, Enabled, Profile};

#[test]
fn enabled_defaults_to_on_and_parses_both_other_forms() {
    let text = r#"
[core]
entries = [
  { id = "a", runtime = "native" },
  { id = "b", runtime = "native", enabled = false },
  { id = "c", runtime = "native", enabled = { service = "skill-checks", check = "is_enabled", arg = "x" } },
]
"#;
    let profile = Profile::from_toml_str(text).expect("parse");
    assert_eq!(profile.core[0].enabled, Enabled::On);
    assert_eq!(profile.core[1].enabled, Enabled::Off);
    assert_eq!(
        profile.core[2].enabled,
        Enabled::Check(EnableCheck {
            service: "skill-checks".into(),
            check: "is_enabled".into(),
            arg: serde_json::json!("x"),
        })
    );
}

#[test]
fn a_bad_enabled_shape_is_a_parse_error_not_a_silent_ignore() {
    let text = r#"
[core]
entries = [{ id = "a", runtime = "native", enabled = "maybe" }]
"#;
    assert!(Profile::from_toml_str(text).is_err());
}

#[test]
fn a_patch_replacement_flips_the_enable_bit() {
    let profile = Profile::from_toml_str(
        r#"
[core]
entries = [{ id = "a", runtime = "native" }]
"#,
    )
    .expect("parse");
    let patch = crate::profile::Patch::from_toml_str(
        r#"
[[replace]]
id = "a"
enabled = false
"#,
    )
    .expect("patch");
    let resolved = crate::profile::resolve(
        &[crate::profile::ProfileSource::Profile(profile)],
        Some(patch),
    )
    .expect("resolve");
    assert_eq!(resolved.core[0].enabled, Enabled::Off);
}

// -- the kernel lookup contract -------------------------------------

/// The host resolves a judge the same way it resolves every other
/// trait-object service (`Arc<dyn Memory>` under `memory`): the value
/// is provided as an `Arc` and read back through `Context::get`.
#[tokio::test]
async fn a_judge_published_as_a_trait_object_is_resolvable() {
    use std::sync::Arc;

    use elysper::{
        core::enable::EnableChecks,
        kernel::{
            Context,
            EffectHandle,
            PLUGIN_PROTOCOL_VERSION,
            Plugin as KernelPlugin,
            PluginError,
            PluginManifest,
            mount_all,
        },
    };

    struct Judge;

    impl EnableChecks for Judge {
        fn check(&self, method: &str, arg: &serde_json::Value) -> Option<bool> {
            (method == "is_enabled").then(|| arg == "yes")
        }
    }

    struct JudgePlugin {
        manifest: PluginManifest,
    }

    impl KernelPlugin for JudgePlugin {
        fn manifest(&self) -> &PluginManifest {
            &self.manifest
        }

        fn apply(
            &self,
            ctx: &Context,
        ) -> Result<Vec<EffectHandle>, PluginError> {
            let judge: Arc<dyn EnableChecks> = Arc::new(Judge);
            Ok(vec![ctx.provide("test-judge", judge)?])
        }
    }

    let ctx = Context::new();
    let plugin = JudgePlugin {
        manifest: PluginManifest::new("judge", PLUGIN_PROTOCOL_VERSION),
    };
    let _mounted =
        mount_all(&ctx, &[Arc::new(plugin) as Arc<dyn KernelPlugin>])
            .expect("the judge mounts");
    let judge = ctx
        .get::<Arc<dyn EnableChecks>>("test-judge")
        .expect("the lookup itself succeeds")
        .expect("the judge is present");
    assert_eq!(
        judge.check("is_enabled", &serde_json::json!("yes")),
        Some(true)
    );
    assert_eq!(
        judge.check("is_enabled", &serde_json::json!("no")),
        Some(false)
    );
    assert_eq!(judge.check("other", &serde_json::json!("yes")), None);
}

// -- the effective row set ------------------------------------------

use crate::profile::{
    Profile as ResolvedProfile,
    enabled::{CheckAnswer, CheckSource, effective},
};

struct FakeChecks;

impl CheckSource for FakeChecks {
    fn check(
        &self,
        service: &str,
        method: &str,
        arg: &serde_json::Value,
    ) -> CheckAnswer {
        match (service, method, arg.as_str()) {
            ("ok", "is_enabled", Some("on")) => CheckAnswer::Enabled,
            ("ok", "is_enabled", Some("off")) => CheckAnswer::Disabled,
            _ => CheckAnswer::Unavailable("judge absent".to_string()),
        }
    }
}

fn row(id: &str, enabled: Enabled) -> crate::profile::Entry {
    crate::profile::Entry {
        id: id.to_string(),
        runtime: crate::profile::RuntimeKind::Native,
        config: serde_json::Value::Null,
        capabilities: Vec::new(),
        enabled,
    }
}

fn check_row(id: &str, arg: &str) -> crate::profile::Entry {
    row(
        id,
        Enabled::Check(EnableCheck {
            service: "ok".into(),
            check: "is_enabled".into(),
            arg: serde_json::json!(arg),
        }),
    )
}

#[test]
fn effective_filters_off_and_fail_closed_check_rows_in_both_segments() {
    let profile = ResolvedProfile {
        core: vec![
            row("on", Enabled::On),
            row("off", Enabled::Off),
            check_row("check-on", "on"),
            check_row("check-off", "off"),
            check_row("check-missing", "missing"),
        ],
        ui: vec![row("ui-off", Enabled::Off), check_row("ui-check-on", "on")],
        hmr: true,
    };
    let assembly = effective(&profile, &FakeChecks);
    let kept: Vec<&str> = assembly
        .profile
        .core
        .iter()
        .map(|e| e.id.as_str())
        .collect();
    assert_eq!(kept, vec!["on", "check-on"]);
    let kept_ui: Vec<&str> =
        assembly.profile.ui.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(kept_ui, vec!["ui-check-on"]);
    let skipped: Vec<(&str, &str)> = assembly
        .skipped
        .iter()
        .map(|row| (row.id.as_str(), row.segment))
        .collect();
    assert_eq!(
        skipped,
        vec![
            ("off", "core"),
            ("check-off", "core"),
            ("check-missing", "core"),
            ("ui-off", "ui"),
        ]
    );
    let missing = assembly
        .skipped
        .iter()
        .find(|row| row.id == "check-missing")
        .expect("the unavailable row is reported");
    assert!(
        missing.reason.contains("judge absent"),
        "the reason names the unavailable check: {}",
        missing.reason
    );
    assert!(assembly.profile.hmr);
}

#[test]
fn effective_is_identity_when_every_row_is_on() {
    let profile = ResolvedProfile {
        core: vec![row("a", Enabled::On), row("b", Enabled::On)],
        ui: Vec::new(),
        hmr: false,
    };
    let assembly = effective(&profile, &FakeChecks);
    assert_eq!(assembly.profile, profile);
    assert!(assembly.skipped.is_empty());
}
