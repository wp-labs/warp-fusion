use std::collections::HashMap;

use wf_lang::ast::Measure;

use super::*;

use crate::datagen::inject_gen::structures::{InjectOverrides, InjectUseStepOverrides, StepInfo};

#[test]
fn matched_use_predicates_are_capped_to_step_event_count() {
    let steps = vec![StepInfo {
        bind_alias: "auth_fail".to_string(),
        scenario_alias: "LoginWindow".to_string(),
        window_name: "LoginWindow".to_string(),
        measure: Measure::Count,
        threshold: 5,
        filter_overrides: HashMap::from([(
            "success".to_string(),
            serde_json::Value::Bool(false),
        )]),
    }];
    let use_steps = vec![InjectUseStepOverrides {
        count: 1_000,
        predicates: HashMap::from([("success".to_string(), serde_json::Value::Bool(false))]),
    }];

    let mapped = map_use_predicates_to_rule_steps(&steps, &use_steps, &[4], true).unwrap();

    assert_eq!(mapped.len(), 1);
    assert_eq!(
        mapped[0].len(),
        4,
        "matched filter predicates must not allocate beyond generated event count"
    );
    assert!(
        mapped[0].iter().all(
            |predicates| predicates.get("success") == Some(&serde_json::Value::Bool(false))
        )
    );
}

#[test]
fn use_step_counts_return_empty_for_empty_steps() {
    let use_steps = vec![InjectUseStepOverrides {
        count: 1,
        predicates: HashMap::from([("success".to_string(), serde_json::Value::Bool(false))]),
    }];

    let counts = compute_use_step_counts(&[], &use_steps).unwrap();

    assert!(counts.is_empty());
}

#[test]
fn planned_use_steps_bind_by_declaration_order() {
    let steps = vec![
        StepInfo {
            bind_alias: "auth_fail".to_string(),
            scenario_alias: "LoginWindow".to_string(),
            window_name: "LoginWindow".to_string(),
            measure: Measure::Count,
            threshold: 1,
            filter_overrides: HashMap::from([(
                "success".to_string(),
                serde_json::Value::Bool(false),
            )]),
        },
        StepInfo {
            bind_alias: "followup".to_string(),
            scenario_alias: "LoginWindow".to_string(),
            window_name: "LoginWindow".to_string(),
            measure: Measure::Count,
            threshold: 1,
            filter_overrides: HashMap::new(),
        },
    ];
    let use_steps = vec![
        InjectUseStepOverrides {
            count: 1,
            predicates: HashMap::from([(
                "success".to_string(),
                serde_json::Value::Bool(false),
            )]),
        },
        InjectUseStepOverrides {
            count: 1,
            predicates: HashMap::from([("dport".to_string(), serde_json::json!(22))]),
        },
    ];

    let counts = compute_use_step_counts(&steps, &use_steps).unwrap();
    let mapped = map_use_predicates_to_rule_steps(&steps, &use_steps, &[1, 1], true).unwrap();

    assert_eq!(counts, vec![1, 1]);
    assert_eq!(
        mapped[0][0].get("success"),
        Some(&serde_json::Value::Bool(false))
    );
    assert_eq!(mapped[1][0].get("dport"), Some(&serde_json::json!(22)));
}

#[test]
fn one_use_step_does_not_spill_across_rule_steps() {
    let steps = vec![
        StepInfo {
            bind_alias: "first".to_string(),
            scenario_alias: "LoginWindow".to_string(),
            window_name: "LoginWindow".to_string(),
            measure: Measure::Count,
            threshold: 1,
            filter_overrides: HashMap::new(),
        },
        StepInfo {
            bind_alias: "second".to_string(),
            scenario_alias: "LoginWindow".to_string(),
            window_name: "LoginWindow".to_string(),
            measure: Measure::Count,
            threshold: 1,
            filter_overrides: HashMap::new(),
        },
    ];
    let use_steps = vec![InjectUseStepOverrides {
        count: 2,
        predicates: HashMap::from([("dport".to_string(), serde_json::json!(22))]),
    }];

    let counts = compute_use_step_counts(&steps, &use_steps).unwrap();
    let mapped = map_use_predicates_to_rule_steps(&steps, &use_steps, &[2, 1], true).unwrap();

    assert_eq!(counts, vec![2, 0]);
    assert_eq!(mapped[0].len(), 2);
    assert!(
        mapped[1][0].is_empty(),
        "one use(...) clause must not spill predicates into the next rule step"
    );
}

#[test]
fn extra_use_step_errors_when_rule_steps_exhausted() {
    let steps = vec![StepInfo {
        bind_alias: "auth_fail".to_string(),
        scenario_alias: "LoginWindow".to_string(),
        window_name: "LoginWindow".to_string(),
        measure: Measure::Count,
        threshold: 5,
        filter_overrides: HashMap::from([(
            "success".to_string(),
            serde_json::Value::Bool(false),
        )]),
    }];
    let use_steps = vec![
        InjectUseStepOverrides {
            count: 5,
            predicates: HashMap::from([(
                "success".to_string(),
                serde_json::Value::Bool(false),
            )]),
        },
        InjectUseStepOverrides {
            count: 1,
            predicates: HashMap::from([("success".to_string(), serde_json::Value::Bool(true))]),
        },
    ];

    let err = compute_use_step_counts(&steps, &use_steps).unwrap_err();
    let rendered = err.report().render().to_string();

    assert!(
        rendered.contains("exceeds rule step count"),
        "unexpected error: {rendered}"
    );
}

#[test]
fn zero_count_use_step_errors() {
    let steps = vec![StepInfo {
        bind_alias: "auth_fail".to_string(),
        scenario_alias: "LoginWindow".to_string(),
        window_name: "LoginWindow".to_string(),
        measure: Measure::Count,
        threshold: 5,
        filter_overrides: HashMap::new(),
    }];
    let use_steps = vec![InjectUseStepOverrides {
        count: 0,
        predicates: HashMap::new(),
    }];

    let err = compute_use_step_counts(&steps, &use_steps).unwrap_err();
    let rendered = err.report().render().to_string();

    assert!(
        rendered.contains("count must be greater than 0"),
        "unexpected error: {rendered}"
    );
}

#[test]
fn conflicting_use_step_predicates_error() {
    let steps = vec![StepInfo {
        bind_alias: "auth_fail".to_string(),
        scenario_alias: "LoginWindow".to_string(),
        window_name: "LoginWindow".to_string(),
        measure: Measure::Count,
        threshold: 5,
        filter_overrides: HashMap::from([(
            "success".to_string(),
            serde_json::Value::Bool(false),
        )]),
    }];
    let use_steps = vec![InjectUseStepOverrides {
        count: 5,
        predicates: HashMap::from([("success".to_string(), serde_json::Value::Bool(true))]),
    }];

    let err = compute_use_step_counts(&steps, &use_steps).unwrap_err();
    let rendered = err.report().render().to_string();

    assert!(
        rendered.contains("conflicts with rule step filter"),
        "unexpected error: {rendered}"
    );
}

#[test]
fn near_miss_use_steps_drop_events_after_near_miss_step() {
    let steps = vec![
        StepInfo {
            bind_alias: "step0".to_string(),
            scenario_alias: "LoginWindow".to_string(),
            window_name: "LoginWindow".to_string(),
            measure: Measure::Count,
            threshold: 1,
            filter_overrides: HashMap::from([(
                "stage".to_string(),
                serde_json::json!("first"),
            )]),
        },
        StepInfo {
            bind_alias: "step1".to_string(),
            scenario_alias: "LoginWindow".to_string(),
            window_name: "LoginWindow".to_string(),
            measure: Measure::Count,
            threshold: 2,
            filter_overrides: HashMap::new(),
        },
        StepInfo {
            bind_alias: "step2".to_string(),
            scenario_alias: "LoginWindow".to_string(),
            window_name: "LoginWindow".to_string(),
            measure: Measure::Count,
            threshold: 1,
            filter_overrides: HashMap::from([(
                "stage".to_string(),
                serde_json::json!("after"),
            )]),
        },
    ];
    let overrides = InjectOverrides {
        entity_field: None,
        count_per_entity: None,
        steps_completed: Some(1),
        within: None,
        use_steps: vec![
            InjectUseStepOverrides {
                count: 1,
                predicates: HashMap::from([("stage".to_string(), serde_json::json!("first"))]),
            },
            InjectUseStepOverrides {
                count: 1,
                predicates: HashMap::from([("stage".to_string(), serde_json::json!("after"))]),
            },
        ],
    };

    let counts = compute_near_miss_counts(&steps, &overrides).unwrap();

    assert_eq!(
        counts,
        vec![1, 1, 0],
        "near_miss boundary should come from the planned step, not raw use count"
    );
}
