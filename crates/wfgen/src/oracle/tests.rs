use std::time::Duration;

use chrono::Utc;
use wf_lang::ast::{CloseMode, CmpOp, Expr, FieldRef, FieldSelector, Measure, Transform};
use wf_lang::plan::{
    AggPlan, BindPlan, BranchPlan, ConvChainPlan, ConvOpPlan, ConvPlan, EntityPlan, MatchPlan,
    RulePlan, ScorePlan, SortKeyPlan, StepPlan, WindowSpec, YieldPlan,
};

use crate::datagen::stream_gen::GenEvent;
use crate::oracle::run_oracle;

fn make_simple_rule_plan() -> RulePlan {
    RulePlan {
        name: "brute_force".to_string(),
        binds: vec![BindPlan {
            alias: "fail".to_string(),
            window: "LoginWindow".to_string(),
            filter: None,
        }],
        match_plan: MatchPlan {
            keys: vec![FieldRef::Simple("sip".to_string())],
            key_map: None,
            window_spec: WindowSpec::Sliding(Duration::from_secs(300)),
            event_steps: vec![StepPlan {
                branches: vec![BranchPlan {
                    label: Some("fail_count".to_string()),
                    source: "fail".to_string(),
                    field: None,
                    guard: None,
                    agg: AggPlan {
                        transforms: vec![],
                        measure: Measure::Count,
                        cmp: CmpOp::Ge,
                        threshold: Expr::Number(3.0),
                    },
                }],
            }],
            close_steps: vec![],
            close_mode: CloseMode::Or,
            tracked_bind_aliases: std::collections::HashSet::new(),
        },
        each_plan: None,
        joins: vec![],
        entity_plan: EntityPlan {
            entity_type: "ip".to_string(),
            entity_id_expr: Expr::Field(FieldRef::Simple("sip".to_string())),
        },
        yield_plan: YieldPlan {
            target: "alerts".to_string(),
            version: None,
            fields: vec![],
        },
        score_plan: ScorePlan {
            expr: Expr::Number(85.0),
        },
        pattern_origin: None,
        conv_plan: None,
        limits_plan: None,
    }
}

fn make_filtered_rule_plan() -> RulePlan {
    let mut plan = make_simple_rule_plan();
    plan.binds[0].filter = Some(Expr::BinOp {
        op: wf_lang::ast::BinOp::Eq,
        left: Box::new(Expr::Field(FieldRef::Simple("action".to_string()))),
        right: Box::new(Expr::StringLit("failed".to_string())),
    });
    plan
}

fn make_event(alias: &str, window: &str, sip: &str, ts: &str) -> GenEvent {
    let mut fields = serde_json::Map::new();
    fields.insert(
        "sip".to_string(),
        serde_json::Value::String(sip.to_string()),
    );
    fields.insert(
        "timestamp".to_string(),
        serde_json::Value::String(ts.to_string()),
    );

    GenEvent {
        stream_name: alias.to_string(),
        window_name: window.to_string(),
        timestamp: ts.parse().unwrap(),
        fields,
    }
}

fn make_action_event(alias: &str, window: &str, sip: &str, action: &str, ts: &str) -> GenEvent {
    let mut event = make_event(alias, window, sip, ts);
    event.fields.insert(
        "action".to_string(),
        serde_json::Value::String(action.to_string()),
    );
    event
}

#[test]
fn hit_cluster_triggers_alert() {
    let plan = make_simple_rule_plan();
    let start: chrono::DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let duration = Duration::from_secs(3600);

    // 3 events with same key → should trigger
    let events = vec![
        make_event("s1", "LoginWindow", "10.0.0.1", "2024-01-01T00:01:00Z"),
        make_event("s1", "LoginWindow", "10.0.0.1", "2024-01-01T00:02:00Z"),
        make_event("s1", "LoginWindow", "10.0.0.1", "2024-01-01T00:03:00Z"),
    ];

    let result = run_oracle(&events, &[plan], &start, &duration, None).unwrap();
    assert_eq!(result.alerts.len(), 1);
    assert_eq!(result.alerts[0].rule_name, "brute_force");
    assert_eq!(result.alerts[0].entity_id, "10.0.0.1");
    assert!((result.alerts[0].score - 85.0).abs() < f64::EPSILON);
    assert_eq!(result.alerts[0].origin, "event");
}

#[test]
fn near_miss_no_alert() {
    let plan = make_simple_rule_plan();
    let start: chrono::DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let duration = Duration::from_secs(3600);

    // 2 events (threshold is 3) → should NOT trigger
    let events = vec![
        make_event("s1", "LoginWindow", "10.0.0.1", "2024-01-01T00:01:00Z"),
        make_event("s1", "LoginWindow", "10.0.0.1", "2024-01-01T00:02:00Z"),
    ];

    let result = run_oracle(&events, &[plan], &start, &duration, None).unwrap();
    assert_eq!(result.alerts.len(), 0);
}

#[test]
fn different_keys_isolated() {
    let plan = make_simple_rule_plan();
    let start: chrono::DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let duration = Duration::from_secs(3600);

    // 2 events each for two different IPs → neither triggers (threshold=3)
    let events = vec![
        make_event("s1", "LoginWindow", "10.0.0.1", "2024-01-01T00:01:00Z"),
        make_event("s1", "LoginWindow", "10.0.0.2", "2024-01-01T00:01:30Z"),
        make_event("s1", "LoginWindow", "10.0.0.1", "2024-01-01T00:02:00Z"),
        make_event("s1", "LoginWindow", "10.0.0.2", "2024-01-01T00:02:30Z"),
    ];

    let result = run_oracle(&events, &[plan], &start, &duration, None).unwrap();
    assert_eq!(result.alerts.len(), 0);
}

#[test]
fn bind_filter_is_applied_during_oracle_eval() {
    let plan = make_filtered_rule_plan();
    let start: chrono::DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let duration = Duration::from_secs(3600);

    let events = vec![
        make_action_event(
            "s1",
            "LoginWindow",
            "10.0.0.1",
            "failed",
            "2024-01-01T00:01:00Z",
        ),
        make_action_event(
            "s1",
            "LoginWindow",
            "10.0.0.1",
            "success",
            "2024-01-01T00:02:00Z",
        ),
        make_action_event(
            "s1",
            "LoginWindow",
            "10.0.0.1",
            "success",
            "2024-01-01T00:03:00Z",
        ),
    ];

    let result = run_oracle(&events, &[plan], &start, &duration, None).unwrap();
    assert_eq!(
        result.alerts.len(),
        0,
        "oracle must honor bind filters instead of counting all same-window events"
    );
}

#[test]
fn empty_events_no_alerts() {
    let plan = make_simple_rule_plan();
    let start: chrono::DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let duration = Duration::from_secs(3600);

    let result = run_oracle(&[], &[plan], &start, &duration, None).unwrap();
    assert_eq!(result.alerts.len(), 0);
}

#[test]
fn multi_alias_same_window_both_receive_events() {
    // Rule with two binds on the same window: "a" and "b" both reference LoginWindow.
    // Step 1 uses "a" (count >= 2), step 2 uses "b" (count >= 2).
    // All events come from LoginWindow, so both aliases must receive them.
    let plan = RulePlan {
        name: "multi_bind".to_string(),
        binds: vec![
            BindPlan {
                alias: "a".to_string(),
                window: "LoginWindow".to_string(),
                filter: None,
            },
            BindPlan {
                alias: "b".to_string(),
                window: "LoginWindow".to_string(),
                filter: None,
            },
        ],
        match_plan: MatchPlan {
            keys: vec![FieldRef::Simple("sip".to_string())],
            key_map: None,
            window_spec: WindowSpec::Sliding(Duration::from_secs(300)),
            event_steps: vec![
                StepPlan {
                    branches: vec![BranchPlan {
                        label: Some("step_a".to_string()),
                        source: "a".to_string(),
                        field: None,
                        guard: None,
                        agg: AggPlan {
                            transforms: vec![],
                            measure: Measure::Count,
                            cmp: CmpOp::Ge,
                            threshold: Expr::Number(2.0),
                        },
                    }],
                },
                StepPlan {
                    branches: vec![BranchPlan {
                        label: Some("step_b".to_string()),
                        source: "b".to_string(),
                        field: None,
                        guard: None,
                        agg: AggPlan {
                            transforms: vec![],
                            measure: Measure::Count,
                            cmp: CmpOp::Ge,
                            threshold: Expr::Number(2.0),
                        },
                    }],
                },
            ],
            close_steps: vec![],
            close_mode: CloseMode::Or,
            tracked_bind_aliases: std::collections::HashSet::new(),
        },
        each_plan: None,
        joins: vec![],
        entity_plan: EntityPlan {
            entity_type: "ip".to_string(),
            entity_id_expr: Expr::Field(FieldRef::Simple("sip".to_string())),
        },
        yield_plan: YieldPlan {
            target: "alerts".to_string(),
            version: None,
            fields: vec![],
        },
        score_plan: ScorePlan {
            expr: Expr::Number(90.0),
        },
        pattern_origin: None,
        conv_plan: None,
        limits_plan: None,
    };

    let start: chrono::DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let duration = Duration::from_secs(3600);

    // 4 events to LoginWindow → alias "a" gets 4, alias "b" gets 4.
    // Step 1 (a >= 2) triggers after event 2, step 2 (b >= 2) triggers after event 4.
    let events = vec![
        make_event("s1", "LoginWindow", "10.0.0.1", "2024-01-01T00:01:00Z"),
        make_event("s1", "LoginWindow", "10.0.0.1", "2024-01-01T00:02:00Z"),
        make_event("s1", "LoginWindow", "10.0.0.1", "2024-01-01T00:03:00Z"),
        make_event("s1", "LoginWindow", "10.0.0.1", "2024-01-01T00:04:00Z"),
    ];

    let result = run_oracle(&events, &[plan], &start, &duration, None).unwrap();

    // With the old single-alias map, alias "b" would never receive events
    // and the rule would never fully match. With the fix, both aliases
    // receive events and the multi-step rule completes.
    assert!(
        !result.alerts.is_empty(),
        "multi-alias same-window rule should trigger when both aliases receive events"
    );
    assert_eq!(result.alerts[0].rule_name, "multi_bind");
}

#[test]
fn sc7_uninjected_rule_skipped() {
    let plan = make_simple_rule_plan(); // name = "brute_force"
    let start: chrono::DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let duration = Duration::from_secs(3600);

    // 3 events that would trigger the rule
    let events = vec![
        make_event("s1", "LoginWindow", "10.0.0.1", "2024-01-01T00:01:00Z"),
        make_event("s1", "LoginWindow", "10.0.0.1", "2024-01-01T00:02:00Z"),
        make_event("s1", "LoginWindow", "10.0.0.1", "2024-01-01T00:03:00Z"),
    ];

    // With injected_rules containing "brute_force" → alert generated
    let injected: std::collections::HashSet<String> =
        ["brute_force".to_string()].into_iter().collect();
    let result = run_oracle(
        &events,
        std::slice::from_ref(&plan),
        &start,
        &duration,
        Some(&injected),
    )
    .unwrap();
    assert_eq!(result.alerts.len(), 1);

    // With injected_rules NOT containing "brute_force" → no alert (SC7)
    let other: std::collections::HashSet<String> =
        ["some_other_rule".to_string()].into_iter().collect();
    let result = run_oracle(&events, &[plan], &start, &duration, Some(&other)).unwrap();
    assert_eq!(result.alerts.len(), 0);
}

// ===========================================================================
// Conv + mixed qualifying/non-qualifying: cross-layer e2e (oracle path)
// ===========================================================================

/// Build an oracle event with sip + dport fields.
fn make_scan_event(alias: &str, window: &str, sip: &str, dport: u16, ts: &str) -> GenEvent {
    let mut fields = serde_json::Map::new();
    fields.insert(
        "sip".to_string(),
        serde_json::Value::String(sip.to_string()),
    );
    fields.insert(
        "dport".to_string(),
        serde_json::Value::Number(serde_json::Number::from(dport)),
    );
    fields.insert(
        "action".to_string(),
        serde_json::Value::String("syn".to_string()),
    );
    fields.insert(
        "timestamp".to_string(),
        serde_json::Value::String(ts.to_string()),
    );

    GenEvent {
        stream_name: alias.to_string(),
        window_name: window.to_string(),
        timestamp: ts.parse().unwrap(),
        fields,
    }
}

/// Conv with mixed qualifying/non-qualifying outputs in the oracle path.
///
/// 4 IPs in one fixed window: 3 qualify via `on close (distinct >= 3)`, 1
/// does not (only 2 distinct ports). Conv `sort(-scan) | top(2)` must
/// operate only on qualifying outputs, producing 2 alerts.
#[test]
fn conv_top_filters_non_qualifying() {
    let plan = RulePlan {
        name: "conv_mixed".to_string(),
        binds: vec![BindPlan {
            alias: "c".to_string(),
            window: "ConnWindow".to_string(),
            filter: None,
        }],
        match_plan: MatchPlan {
            keys: vec![FieldRef::Simple("sip".to_string())],
            key_map: None,
            window_spec: WindowSpec::Fixed(Duration::from_secs(3600)),
            event_steps: vec![StepPlan {
                branches: vec![BranchPlan {
                    label: Some("c".to_string()),
                    source: "c".to_string(),
                    field: None,
                    guard: None,
                    agg: AggPlan {
                        transforms: vec![],
                        measure: Measure::Count,
                        cmp: CmpOp::Ge,
                        threshold: Expr::Number(1.0),
                    },
                }],
            }],
            close_steps: vec![StepPlan {
                branches: vec![BranchPlan {
                    label: Some("scan".to_string()),
                    source: "c".to_string(),
                    field: Some(FieldSelector::Dot("dport".to_string())),
                    guard: None,
                    agg: AggPlan {
                        transforms: vec![Transform::Distinct],
                        measure: Measure::Count,
                        cmp: CmpOp::Ge,
                        threshold: Expr::Number(3.0),
                    },
                }],
            }],
            close_mode: CloseMode::And,
            tracked_bind_aliases: std::collections::HashSet::new(),
        },
        each_plan: None,
        joins: vec![],
        entity_plan: EntityPlan {
            entity_type: "ip".to_string(),
            entity_id_expr: Expr::Field(FieldRef::Simple("sip".to_string())),
        },
        yield_plan: YieldPlan {
            target: "alerts".to_string(),
            version: None,
            fields: vec![],
        },
        score_plan: ScorePlan {
            expr: Expr::Number(80.0),
        },
        pattern_origin: None,
        conv_plan: Some(ConvPlan {
            chains: vec![ConvChainPlan {
                ops: vec![
                    ConvOpPlan::Sort(vec![SortKeyPlan {
                        expr: Expr::Field(FieldRef::Simple("scan".into())),
                        descending: true,
                    }]),
                    ConvOpPlan::Top(2),
                ],
            }],
        }),
        limits_plan: None,
    };

    let start: chrono::DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let duration = Duration::from_secs(7200); // 2h > 1h window → expires

    let mut events = Vec::new();
    let mut sec = 0;

    // IP-A: 5 distinct ports → qualifying (scan=5)
    for port in [80, 443, 8080, 22, 3306] {
        sec += 1;
        events.push(make_scan_event(
            "s1",
            "ConnWindow",
            "10.0.0.1",
            port,
            &format!("2024-01-01T00:{:02}:{:02}Z", sec / 60, sec % 60),
        ));
    }

    // IP-B: 4 distinct ports → qualifying (scan=4)
    for port in [80, 443, 8080, 22] {
        sec += 1;
        events.push(make_scan_event(
            "s1",
            "ConnWindow",
            "10.0.0.2",
            port,
            &format!("2024-01-01T00:{:02}:{:02}Z", sec / 60, sec % 60),
        ));
    }

    // IP-C: 3 distinct ports → qualifying (scan=3)
    for port in [80, 443, 8080] {
        sec += 1;
        events.push(make_scan_event(
            "s1",
            "ConnWindow",
            "10.0.0.3",
            port,
            &format!("2024-01-01T00:{:02}:{:02}Z", sec / 60, sec % 60),
        ));
    }

    // IP-D: 2 distinct ports → NON-qualifying (scan=2 < 3)
    for port in [80, 443] {
        sec += 1;
        events.push(make_scan_event(
            "s1",
            "ConnWindow",
            "10.0.0.4",
            port,
            &format!("2024-01-01T00:{:02}:{:02}Z", sec / 60, sec % 60),
        ));
    }

    let result = run_oracle(&events, &[plan], &start, &duration, None).unwrap();

    // 3 qualifying, conv top(2) keeps 2; non-qualifying IP-D produces no alert
    assert_eq!(
        result.alerts.len(),
        2,
        "expected 2 alerts after conv top(2)"
    );

    let mut ids: Vec<&str> = result.alerts.iter().map(|a| a.entity_id.as_str()).collect();
    ids.sort();
    assert_eq!(ids, vec!["10.0.0.1", "10.0.0.2"]);
}
