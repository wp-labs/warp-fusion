use std::time::Duration;

use chrono::Utc;
use wf_lang::ast::{
    CloseMode, CmpOp, Expr, FieldRef, FieldSelector, MatchMode, Measure, Transform,
};
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
        lets: Vec::new(),
        match_plan: MatchPlan {
            keys: vec![FieldRef::Simple("sip".to_string())],
            key_map: None,
            key_join: None,
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
            match_mode: MatchMode::Seq,
            accu: false,
            seq: None,
            tracked_bind_aliases: std::collections::HashSet::new(),
            tracked_bind_fields: std::collections::HashMap::new(),
            tracked_plain_fields: std::collections::HashSet::new(),
            needs_field_history: false,
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
        conv_window: None,
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

fn make_and_close_rule_plan() -> RulePlan {
    let mut plan = make_simple_rule_plan();
    plan.name = "close_rule".to_string();
    plan.match_plan.close_steps = vec![StepPlan {
        branches: vec![BranchPlan {
            label: Some("close_count".to_string()),
            source: "fail".to_string(),
            field: None,
            guard: None,
            agg: AggPlan {
                transforms: vec![],
                measure: Measure::Count,
                cmp: CmpOp::Ge,
                threshold: Expr::Number(1.0),
            },
        }],
    }];
    plan.match_plan.close_mode = CloseMode::And;
    plan
}

fn make_timeout_guard_close_rule_plan() -> RulePlan {
    let mut plan = make_and_close_rule_plan();
    plan.name = "timeout_close_rule".to_string();
    plan.match_plan.close_steps[0].branches[0].guard = Some(Expr::BinOp {
        op: wf_lang::ast::BinOp::Eq,
        left: Box::new(Expr::Field(FieldRef::Simple("close_reason".to_string()))),
        right: Box::new(Expr::StringLit("timeout".to_string())),
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
fn close_all_eos_fires_and_close_rule_at_scenario_end() {
    let plan = make_and_close_rule_plan();
    let start: chrono::DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let duration = Duration::from_secs(60);

    // The 5m match window has not expired by scenario end. The oracle must
    // still model finite replay EOF and close active instances.
    let events = vec![
        make_event("s1", "LoginWindow", "10.0.0.1", "2024-01-01T00:00:01Z"),
        make_event("s1", "LoginWindow", "10.0.0.1", "2024-01-01T00:00:02Z"),
        make_event("s1", "LoginWindow", "10.0.0.1", "2024-01-01T00:00:03Z"),
    ];

    let result = run_oracle(&events, &[plan], &start, &duration, None).unwrap();

    assert_eq!(result.alerts.len(), 1);
    assert_eq!(result.alerts[0].rule_name, "close_rule");
    assert_eq!(result.alerts[0].entity_id, "10.0.0.1");
    assert_eq!(result.alerts[0].origin, "close:eos");
}

#[test]
fn scenario_end_timeout_sweep_fires_timeout_guarded_close_rule() {
    let plan = make_timeout_guard_close_rule_plan();
    let start: chrono::DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let duration = Duration::from_secs(600);

    let events = vec![
        make_event("s1", "LoginWindow", "10.0.0.1", "2024-01-01T00:00:01Z"),
        make_event("s1", "LoginWindow", "10.0.0.1", "2024-01-01T00:00:02Z"),
        make_event("s1", "LoginWindow", "10.0.0.1", "2024-01-01T00:00:03Z"),
    ];

    let result = run_oracle(&events, &[plan], &start, &duration, None).unwrap();

    assert_eq!(result.alerts.len(), 1);
    assert_eq!(result.alerts[0].rule_name, "timeout_close_rule");
    assert_eq!(result.alerts[0].entity_id, "10.0.0.1");
    assert_eq!(result.alerts[0].origin, "close:timeout");
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
        lets: Vec::new(),
        match_plan: MatchPlan {
            keys: vec![FieldRef::Simple("sip".to_string())],
            key_map: None,
            key_join: None,
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
            match_mode: MatchMode::Seq,
            accu: false,
            seq: None,
            tracked_bind_aliases: std::collections::HashSet::new(),
            tracked_bind_fields: std::collections::HashMap::new(),
            tracked_plain_fields: std::collections::HashSet::new(),
            needs_field_history: false,
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
        conv_window: None,
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
        lets: Vec::new(),
        match_plan: MatchPlan {
            keys: vec![FieldRef::Simple("sip".to_string())],
            key_map: None,
            key_join: None,
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
            match_mode: MatchMode::Seq,
            accu: false,
            seq: None,
            tracked_bind_aliases: std::collections::HashSet::new(),
            tracked_bind_fields: std::collections::HashMap::new(),
            tracked_plain_fields: std::collections::HashSet::new(),
            needs_field_history: true,
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
        conv_window: None,
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

// ===========================================================================
// P2 (Path A): join-then-key — oracle maintains join window state
// ===========================================================================

use wf_lang::plan::JoinKeyPlan;
use wf_lang::{BaseType, FieldDef, FieldType, WindowSchema};

use crate::oracle::run_oracle_events_full;

fn bid_window_schema() -> WindowSchema {
    WindowSchema {
        name: "bid_events".to_string(),
        streams: vec!["bid".to_string()],
        time_field: Some("dateTime".to_string()),
        over: Duration::from_secs(600),
        fields: vec![
            FieldDef {
                name: "auction".to_string(),
                field_type: FieldType::Base(BaseType::Digit),
            },
            FieldDef {
                name: "price".to_string(),
                field_type: FieldType::Base(BaseType::Digit),
            },
        ],
    }
}

fn auction_window_schema() -> WindowSchema {
    WindowSchema {
        name: "auction_events".to_string(),
        streams: vec!["auction".to_string()],
        time_field: Some("dateTime".to_string()),
        over: Duration::from_secs(600),
        fields: vec![
            FieldDef {
                name: "id".to_string(),
                field_type: FieldType::Base(BaseType::Digit),
            },
            FieldDef {
                name: "category".to_string(),
                field_type: FieldType::Base(BaseType::Digit),
            },
        ],
    }
}

/// `match<category:10m>` join-then-key plan: key resolved from auction_events
/// via `b.auction == auction_events.id`, category read off the joined row.
fn make_join_key_rule_plan() -> RulePlan {
    RulePlan {
        name: "q4_cat".to_string(),
        binds: vec![BindPlan {
            alias: "b".to_string(),
            window: "bid_events".to_string(),
            filter: None,
        }],
        lets: Vec::new(),
        match_plan: MatchPlan {
            keys: vec![FieldRef::Simple("category".to_string())],
            key_map: None,
            key_join: Some(JoinKeyPlan {
                join_idx: 0,
                right_window: "auction_events".to_string(),
                left_field: FieldRef::Qualified("b".into(), "auction".into()),
                right_key_field: "id".to_string(),
                right_field: "category".to_string(),
                key_name: "category".to_string(),
            }),
            window_spec: WindowSpec::Sliding(Duration::from_secs(600)),
            event_steps: vec![StepPlan {
                branches: vec![BranchPlan {
                    label: Some("hits".to_string()),
                    source: "b".to_string(),
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
            close_steps: vec![],
            close_mode: CloseMode::Or,
            match_mode: MatchMode::Seq,
            accu: false,
            seq: None,
            tracked_bind_aliases: std::collections::HashSet::new(),
            tracked_bind_fields: std::collections::HashMap::new(),
            tracked_plain_fields: std::collections::HashSet::new(),
            needs_field_history: false,
        },
        each_plan: None,
        joins: vec![wf_lang::plan::JoinPlan {
            right_window: "auction_events".to_string(),
            mode: wf_lang::ast::JoinMode::Snapshot,
            conds: vec![wf_lang::plan::JoinCondPlan {
                left: FieldRef::Qualified("b".into(), "auction".into()),
                right: FieldRef::Qualified("auction_events".into(), "id".into()),
            }],
        }],
        entity_plan: EntityPlan {
            entity_type: "digit".to_string(),
            entity_id_expr: Expr::Field(FieldRef::Simple("category".to_string())),
        },
        yield_plan: YieldPlan {
            target: "alerts".to_string(),
            version: None,
            fields: vec![],
        },
        score_plan: ScorePlan {
            expr: Expr::Number(20.0),
        },
        pattern_origin: None,
        conv_plan: None,
        limits_plan: None,
        conv_window: None,
    }
}

fn make_auction_event(id: u64, category: u64, ts: &str) -> GenEvent {
    let mut fields = serde_json::Map::new();
    fields.insert("id".into(), serde_json::Value::Number(id.into()));
    fields.insert(
        "category".into(),
        serde_json::Value::Number(category.into()),
    );
    fields.insert(
        "timestamp".into(),
        serde_json::Value::String(ts.to_string()),
    );
    GenEvent {
        stream_name: "auction".to_string(),
        window_name: "auction_events".to_string(),
        timestamp: ts.parse().unwrap(),
        fields,
    }
}

fn make_bid_event_with_price(auction: u64, price: u64, ts: &str) -> GenEvent {
    let mut fields = serde_json::Map::new();
    fields.insert(
        "auction".into(),
        serde_json::Value::Number(auction.into()),
    );
    fields.insert(
        "price".into(),
        serde_json::Value::Number(price.into()),
    );
    fields.insert(
        "timestamp".into(),
        serde_json::Value::String(ts.to_string()),
    );
    GenEvent {
        stream_name: "bid".to_string(),
        window_name: "bid_events".to_string(),
        timestamp: ts.parse().unwrap(),
        fields,
    }
}

fn make_bid_event(auction: u64, ts: &str) -> GenEvent {
    let mut fields = serde_json::Map::new();
    fields.insert("auction".into(), serde_json::Value::Number(auction.into()));
    fields.insert(
        "timestamp".into(),
        serde_json::Value::String(ts.to_string()),
    );
    GenEvent {
        stream_name: "bid".to_string(),
        window_name: "bid_events".to_string(),
        timestamp: ts.parse().unwrap(),
        fields,
    }
}

#[test]
fn oracle_join_key_hit_uses_joined_key() {
    let plan = make_join_key_rule_plan();
    let start: chrono::DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let duration = Duration::from_secs(3600);
    let schemas = [bid_window_schema(), auction_window_schema()];

    // auction id=1 (category=7) arrives, then two bids reference it. Both bids
    // join to category=7 → same instance → the count>=1 step fires once per
    // bid (fire-and-reset), so 2 alerts, each entity_id = "7".
    let events = vec![
        make_auction_event(1, 7, "2024-01-01T00:01:00Z"),
        make_bid_event(1, "2024-01-01T00:02:00Z"),
        make_bid_event(1, "2024-01-01T00:03:00Z"),
    ];
    let result =
        run_oracle_events_full(events, &[plan], &schemas, &start, &duration, None, true).unwrap();
    assert_eq!(result.alerts.len(), 2, "each bid fires once");
    for alert in &result.alerts {
        assert_eq!(alert.entity_id, "7", "entity key = joined category");
    }
}

#[test]
fn oracle_join_key_miss_skips_event() {
    let plan = make_join_key_rule_plan();
    let start: chrono::DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let duration = Duration::from_secs(3600);
    let schemas = [bid_window_schema(), auction_window_schema()];

    // No auction row for id=2 → join miss → bid skipped.
    let events = vec![
        make_auction_event(1, 7, "2024-01-01T00:01:00Z"),
        make_bid_event(2, "2024-01-01T00:02:00Z"),
    ];
    let result =
        run_oracle_events_full(events, &[plan], &schemas, &start, &duration, None, true).unwrap();
    assert_eq!(result.alerts.len(), 0, "join miss → no instance, no alert");
}

#[test]
fn oracle_join_key_expires_rows_by_own_window_watermark() {
    // Retention is driven by the JOIN window's own watermark (auction events),
    // never by the driver's bid timestamps (2026-08 review finding). A bid at
    // T+11m does NOT evict the auction row — only a later auction event that
    // advances the auction watermark past `ts + over` expires it.
    let plan = make_join_key_rule_plan();
    let start: chrono::DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let duration = Duration::from_secs(3600);
    let schemas = [bid_window_schema(), auction_window_schema()];

    let events = vec![
        // auction 1 @00:01 (over 10m → expires when the auction watermark
        // reaches 00:11).
        make_auction_event(1, 7, "2024-01-01T00:01:00Z"),
        // Bid at T+11m: the bid timestamp must NOT evict the auction row →
        // still joins (engine: auction window watermark is driven by auction
        // events only).
        make_bid_event(1, "2024-01-01T00:12:00Z"),
        // A new auction event at 00:13 advances the auction watermark →
        // auction 1's row (ts 00:01 + 10m = 00:11 <= 00:13) is now expired.
        make_auction_event(2, 8, "2024-01-01T00:13:00Z"),
        // This bid still references auction 1 → join miss (row expired).
        make_bid_event(1, "2024-01-01T00:14:00Z"),
    ];
    let result =
        run_oracle_events_full(events, &[plan], &schemas, &start, &duration, None, true).unwrap();
    assert_eq!(
        result.alerts.len(),
        1,
        "T+11m bid joins (own-window watermark); post-auction-watermark bid misses"
    );
}

#[test]
fn oracle_preload_makes_future_join_rows_visible() {
    // The generator's bids randomly reference auctions whose event may arrive
    // LATER in the stream. The oracle preloads all join rows ahead of the main
    // loop (mirroring the engine's append-ahead window), so a bid that appears
    // BEFORE its auction in the stream still joins successfully.
    let plan = make_join_key_rule_plan();
    let start: chrono::DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let duration = Duration::from_secs(3600);
    let schemas = [bid_window_schema(), auction_window_schema()];

    // Bid for auction 9 arrives BEFORE auction 9's row (out-of-order reference).
    let events = vec![
        make_bid_event(9, "2024-01-01T00:02:00Z"),
        make_auction_event(9, 3, "2024-01-01T00:05:00Z"),
    ];
    let result =
        run_oracle_events_full(events, &[plan], &schemas, &start, &duration, None, true).unwrap();
    assert_eq!(
        result.alerts.len(),
        1,
        "preloaded join rows must make the 'future' auction visible to the earlier bid"
    );
    assert_eq!(result.alerts[0].entity_id, "3", "key = joined category");
}

#[test]
fn oracle_fixed_close_fires_once_per_bucket_boundary() {
    // Fixed 10m window + `and close`: the bucket-boundary scan must not double
    // fire or miss closes vs. per-event scanning. Bids in bucket 0, then a bid
    // crossing into bucket 1 closes bucket 0; the last bucket closes via the
    // preloaded watermark only when the flow ends (close_at_eos=true here).
    let mut plan = make_join_key_rule_plan();
    plan.match_plan.window_spec = WindowSpec::Fixed(Duration::from_secs(600));
    plan.match_plan.close_steps = vec![StepPlan {
        branches: vec![BranchPlan {
            label: Some("close_avg".to_string()),
            source: "b".to_string(),
            field: Some(FieldSelector::Dot("price".to_string())),
            guard: None,
            agg: AggPlan {
                transforms: vec![],
                measure: Measure::Avg,
                cmp: CmpOp::Ge,
                threshold: Expr::Number(10.0),
            },
        }],
    }];
    plan.match_plan.close_mode = CloseMode::And;

    let start: chrono::DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let duration = Duration::from_secs(3600);
    let schemas = [bid_window_schema(), auction_window_schema()];

    // Auction id=1 (category=7, over 10m → expires 00:10:30). Bids at 00:01
    // (bucket 0) and 00:10:15 (bucket 1, still inside the auction's over —
    // join hits) — the second bid's bucket crossing closes bucket 0.
    let events = vec![
        make_auction_event(1, 7, "2024-01-01T00:00:30Z"),
        make_bid_event_with_price(1, 100, "2024-01-01T00:01:00Z"),
        make_bid_event_with_price(1, 300, "2024-01-01T00:10:15Z"),
    ];
    let result =
        run_oracle_events_full(events, &[plan], &schemas, &start, &duration, None, true).unwrap();
    // `and close` (And mode): the on-event step only advances the instance —
    // output happens at close. Bucket 0 closes when the 00:11 bid crosses the
    // bucket boundary; bucket 1 closes at EOS (close_at_eos=true). Exactly 2
    // close alerts, both with close origins.
    assert_eq!(result.alerts.len(), 2, "boundary close + EOS close");
    for alert in &result.alerts {
        assert!(
            alert.origin.starts_with("close:"),
            "And-mode rules emit only on close, got origin {}",
            alert.origin
        );
    }
}
