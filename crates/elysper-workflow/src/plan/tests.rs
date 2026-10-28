use super::*;
use crate::spec::{AgentStep, BestOfStep, FanOutStep, PipelineStep};

fn agent(id: &str) -> Step {
    Step::Agent(AgentStep {
        id: id.to_owned(),
        agent: format!("{id}-agent"),
        prompt: format!("prompt for {id}"),
        gate: None,
        isolation: false,
    })
}

fn best_of(id: &str, items: &[&str]) -> BestOfStep {
    BestOfStep {
        id: id.to_owned(),
        agent: "coder".to_owned(),
        items: items.iter().map(|item| (*item).to_owned()).collect(),
        gate: None,
        max_items: None,
        concurrency: None,
    }
}

fn spec(steps: Vec<Step>) -> WorkflowSpec {
    WorkflowSpec {
        id: "wf".to_owned(),
        steps,
        phases: Vec::new(),
    }
}

fn fan_out(items: usize) -> Step {
    Step::FanOut(FanOutStep {
        id: "fan".to_owned(),
        agent: "fan-agent".to_owned(),
        items: (0..items).map(|index| format!("item {index}")).collect(),
        concurrency: None,
        max_items: None,
    })
}

#[test]
fn positions_follow_the_document_in_pre_order() {
    let document = spec(vec![
        agent("first"),
        Step::Pipeline(PipelineStep {
            id: "pipe".to_owned(),
            stages: vec![agent("stage-one"), fan_out(2)],
        }),
        agent("last"),
    ]);

    let plan = document.plan(&WorkflowCaps::default()).unwrap();
    let ids: Vec<&str> = plan
        .calls()
        .iter()
        .map(|call| call.step_id.as_str())
        .collect();
    assert_eq!(ids, ["first", "stage-one", "fan", "fan", "last"]);
    assert_eq!(plan.call_count(), 5);
    for (position, call) in plan.calls().iter().enumerate() {
        assert_eq!(call.position, position);
    }
    // Only the pipeline's stages are nested.
    let depths: Vec<usize> = plan.calls().iter().map(|c| c.depth).collect();
    assert_eq!(depths, [0, 1, 1, 1, 0]);
}

#[test]
fn identities_change_with_what_the_agent_sees() {
    let baseline = spec(vec![agent("only")])
        .plan(&WorkflowCaps::default())
        .unwrap();
    let renamed = WorkflowSpec {
        steps: vec![Step::Agent(AgentStep {
            id: "only".to_owned(),
            agent: "other-agent".to_owned(),
            prompt: "prompt for only".to_owned(),
            gate: None,
            isolation: false,
        })],
        ..spec(vec![agent("only")])
    }
    .plan(&WorkflowCaps::default())
    .unwrap();
    assert_ne!(baseline.calls()[0].identity, renamed.calls()[0].identity);
}

#[test]
fn a_selection_plans_one_call_per_candidate_under_the_step_gate() {
    let document = spec(vec![
        Step::BestOf(BestOfStep {
            id: "pick".to_owned(),
            agent: "coder".to_owned(),
            items: vec!["one".to_owned(), "two".to_owned()],
            gate: Some(GateRef {
                command: "cargo test".to_owned(),
                label: None,
            }),
            max_items: None,
            concurrency: Some(2),
        }),
        agent("after"),
    ]);
    let plan = document.plan(&WorkflowCaps::default()).unwrap();

    assert_eq!(
        plan.steps()[0],
        PlannedStep::BestOf {
            id: "pick".to_owned(),
            concurrency: Some(2),
            items: vec![0, 1],
        }
    );
    assert_eq!(plan.call_count(), 3);
    assert_eq!(
        plan.calls()[0].step_id.as_str(),
        plan.calls()[1].step_id.as_str()
    );
    // The step's gate rides on every candidate, so each one is
    // judged on its own; the step after it declares none.
    for call in &plan.calls()[..2] {
        assert_eq!(
            call.gate.as_ref().map(|gate| gate.command.as_str()),
            Some("cargo test")
        );
    }
    assert!(plan.calls()[2].gate.is_none());
    // Same agent, same gate: the prompt and the position are what
    // tell two candidates apart, in the journal as in the plan.
    assert_ne!(plan.calls()[0].identity, plan.calls()[1].identity);
    assert_eq!(plan.calls()[0].prompt, "one");
    assert_eq!(plan.calls()[1].prompt, "two");
    assert_eq!(plan.calls()[0].depth, 0);
}

#[test]
fn caps_reject_before_anything_runs() {
    let caps = WorkflowCaps {
        max_agents: 2,
        ..WorkflowCaps::default()
    };
    let error = spec(vec![agent("a"), agent("b"), agent("c")])
        .plan(&caps)
        .unwrap_err();
    assert!(matches!(
        error,
        WorkflowError::CapExceeded {
            cap: "max_agents",
            limit: 2,
            requested: 3,
        }
    ));

    let step_capped = Step::FanOut(FanOutStep {
        id: "fan".to_owned(),
        agent: "fan-agent".to_owned(),
        items: vec!["one".to_owned(), "two".to_owned()],
        concurrency: None,
        max_items: Some(1),
    });
    let error = spec(vec![step_capped])
        .plan(&WorkflowCaps::default())
        .unwrap_err();
    assert!(matches!(
        error,
        WorkflowError::CapExceeded {
            cap: "max_items",
            limit: 1,
            requested: 2,
        }
    ));

    // A selection is held to the same item cap as a fan-out: its
    // candidates are what the cap is counting.
    let step_capped = Step::BestOf(BestOfStep {
        max_items: Some(1),
        ..best_of("pick", &["one", "two"])
    });
    let error = spec(vec![step_capped])
        .plan(&WorkflowCaps::default())
        .unwrap_err();
    assert!(matches!(
        error,
        WorkflowError::CapExceeded {
            cap: "max_items",
            limit: 1,
            requested: 2,
        }
    ));

    let run_capped = WorkflowCaps {
        max_items: 1,
        ..WorkflowCaps::default()
    };
    let error = spec(vec![Step::BestOf(best_of("pick", &["one", "two"]))])
        .plan(&run_capped)
        .unwrap_err();
    assert!(matches!(
        error,
        WorkflowError::CapExceeded {
            cap: "max_items",
            limit: 1,
            requested: 2,
        }
    ));

    let nested = Step::Pipeline(PipelineStep {
        id: "outer".to_owned(),
        stages: vec![Step::Pipeline(PipelineStep {
            id: "inner".to_owned(),
            stages: vec![agent("deep")],
        })],
    });
    let caps = WorkflowCaps {
        max_nested: 1,
        ..WorkflowCaps::default()
    };
    let error = spec(vec![nested]).plan(&caps).unwrap_err();
    assert!(matches!(
        error,
        WorkflowError::CapExceeded {
            cap: "max_nested",
            limit: 1,
            requested: 2,
        }
    ));
}

#[test]
fn a_document_that_cannot_run_is_rejected() {
    assert!(spec(Vec::new()).plan(&WorkflowCaps::default()).is_err());

    let duplicated = spec(vec![agent("same"), agent("same")]);
    assert!(matches!(
        duplicated.plan(&WorkflowCaps::default()),
        Err(WorkflowError::InvalidSpec { .. })
    ));

    let empty_prompt = WorkflowSpec {
        steps: vec![Step::Agent(AgentStep {
            id: "a".to_owned(),
            agent: "a-agent".to_owned(),
            prompt: String::new(),
            gate: None,
            isolation: false,
        })],
        ..spec(vec![agent("a")])
    };
    assert!(empty_prompt.plan(&WorkflowCaps::default()).is_err());

    let unknown_phase = WorkflowSpec {
        phases: vec![Phase {
            id: "p".to_owned(),
            label: "Phase".to_owned(),
            steps: vec!["ghost".to_owned()],
        }],
        ..spec(vec![agent("a")])
    };
    assert!(unknown_phase.plan(&WorkflowCaps::default()).is_err());

    // A selection has to have candidates worth choosing between, one
    // prompt each, and a gate that says something.
    let broken = [
        best_of("pick", &[]),
        best_of("pick", &["one", "  "]),
        BestOfStep {
            concurrency: Some(0),
            ..best_of("pick", &["one"])
        },
        BestOfStep {
            gate: Some(GateRef {
                command: "  ".to_owned(),
                label: None,
            }),
            ..best_of("pick", &["one"])
        },
    ];
    for step in broken {
        let document = spec(vec![Step::BestOf(step.clone())]);
        assert!(
            matches!(
                document.plan(&WorkflowCaps::default()),
                Err(WorkflowError::InvalidSpec { .. })
            ),
            "{step:?} should not plan"
        );
    }
}

#[test]
fn phases_label_calls_without_moving_them() {
    let document = WorkflowSpec {
        phases: vec![Phase {
            id: "p1".to_owned(),
            label: "Review".to_owned(),
            steps: vec!["b".to_owned(), "c".to_owned()],
        }],
        ..spec(vec![agent("a"), agent("b"), agent("c")])
    };
    let plan = document.plan(&WorkflowCaps::default()).unwrap();
    let labels: Vec<Option<&str>> = plan
        .calls()
        .iter()
        .map(|call| call.phase.as_deref())
        .collect();
    assert_eq!(labels, [None, Some("Review"), Some("Review")]);
}

#[test]
fn the_replay_prefix_ends_at_the_first_miss() {
    let plan = spec(vec![agent("a"), agent("b"), agent("c")])
        .plan(&WorkflowCaps::default())
        .unwrap();
    let first = plan.calls()[0].identity.clone();
    let second = plan.calls()[1].identity.clone();
    let third = plan.calls()[2].identity.clone();

    let complete = vec![
        JournalEntry::success(0, first.clone(), "one"),
        JournalEntry::success(1, second.clone(), "two"),
        JournalEntry::success(2, third, "three"),
    ];
    assert_eq!(plan.replay_prefix(&complete), 3);

    // A failure stops the prefix: the run must retry that call.
    let failed = vec![
        JournalEntry::success(0, first.clone(), "one"),
        JournalEntry::failure(1, second.clone(), "boom"),
        JournalEntry::success(2, plan.calls()[2].identity.clone(), "three"),
    ];
    assert_eq!(plan.replay_prefix(&failed), 1);

    // A changed prompt stops it at that position and no further.
    let changed = vec![
        JournalEntry::success(0, first.clone(), "one"),
        JournalEntry::success(1, "not-the-key".to_owned(), "two"),
        JournalEntry::success(2, plan.calls()[2].identity.clone(), "three"),
    ];
    assert_eq!(plan.replay_prefix(&changed), 1);

    // A hole is a miss even when every later entry matches.
    let holed = vec![
        JournalEntry::success(0, first, "one"),
        JournalEntry::success(2, plan.calls()[2].identity.clone(), "three"),
    ];
    assert_eq!(plan.replay_prefix(&holed), 1);

    assert_eq!(plan.replay_prefix(&[]), 0);
}

#[test]
fn a_lost_candidate_replays_once_its_step_has_a_winner() {
    let document = spec(vec![
        Step::BestOf(best_of("pick", &["one", "two"])),
        agent("after"),
    ]);
    let plan = document.plan(&WorkflowCaps::default()).unwrap();
    let first = plan.calls()[0].identity.clone();
    let second = plan.calls()[1].identity.clone();
    let third = plan.calls()[2].identity.clone();

    let loss = JournalEntry::failure(0, first.clone(), "boom");
    let win = JournalEntry::success(1, second.clone(), "answer: two");
    let next = JournalEntry::success(2, third.clone(), "done");

    // The step has a winner on record, so its candidate's failure is
    // a decided loss: the whole document still replays, winner and
    // following step included.
    assert_eq!(
        plan.replay_prefix(&[loss.clone(), win.clone(), next.clone()]),
        3
    );

    // A selection nobody won is not decided: the first failed
    // candidate is where the retry starts.
    let nobody = JournalEntry::failure(1, second.clone(), "boom too");
    assert_eq!(
        plan.replay_prefix(&[loss.clone(), nobody.clone(), next.clone()]),
        0
    );

    // A sibling that no longer matches the plan is not a winner, so
    // it cannot decide anything either.
    let stale = JournalEntry::success(1, "not-the-key".to_owned(), "x");
    assert_eq!(plan.replay_prefix(&[loss.clone(), stale, next.clone()]), 0);

    // Only a selection gets this treatment: a failed fan-out item is
    // still the first call a resume has to make.
    let fan = spec(vec![Step::FanOut(FanOutStep {
        id: "fan".to_owned(),
        agent: "fan-agent".to_owned(),
        items: vec!["one".to_owned(), "two".to_owned()],
        concurrency: None,
        max_items: None,
    })])
    .plan(&WorkflowCaps::default())
    .unwrap();
    let entries = vec![
        JournalEntry::failure(0, fan.calls()[0].identity.clone(), "boom"),
        JournalEntry::success(1, fan.calls()[1].identity.clone(), "two"),
    ];
    assert_eq!(fan.replay_prefix(&entries), 0);
    assert_eq!(fan.replay_prefix(&entries[1..]), 0);
}
