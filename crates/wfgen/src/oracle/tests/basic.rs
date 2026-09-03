use super::*;

#[test]
fn hop_oracle_closes_every_covered_window() {
    // hop(10s, 2s) + `and close` count：每覆盖窗口收口输出一条。
    // 事件 t=0/4/8s → 覆盖窗口并集 k=-4..4（9 个）；6 个在 eos 前 slide 边界
    // 收口（末 2..12s）+ 3 个由 eos close_all 收口（末 14..18s）。
    let mut plan = make_simple_rule_plan();
    // 默认 on-event 阈值为 3，改为 1（单事件即达标）。
    plan.match_plan.event_steps[0].branches[0].agg.threshold = Expr::Number(1.0);
    plan.match_plan.window_spec = WindowSpec::Hop {
        size: Duration::from_secs(10),
        slide: Duration::from_secs(2),
    };
    plan.match_plan.close_steps = vec![StepPlan {
        branches: vec![BranchPlan {
            label: Some("n".to_string()),
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
    let start: chrono::DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let duration = Duration::from_secs(12);
    let events = vec![
        make_event("s1", "LoginWindow", "10.0.0.1", "2024-01-01T00:00:00Z"),
        make_event("s1", "LoginWindow", "10.0.0.1", "2024-01-01T00:00:04Z"),
        make_event("s1", "LoginWindow", "10.0.0.1", "2024-01-01T00:00:08Z"),
    ];
    let result = run_oracle(&events, &[plan], &start, &duration, None).unwrap();
    // 2026-08-23 close_all 对齐 oracle/Flink 后：eos close_all 只收口**完整**
    // 窗口（w_end ≤ 最终事件时间 8s）——尾部 3 个未完整窗口（末 14/16/18s）
    // 释放实例但不发射（q5 修复同源）；仅 6 个在 slide 边界收口的完整窗口输出。
    assert_eq!(result.alerts.len(), 6, "6 个完整覆盖窗口各输出一条");
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
            key_exprs: Vec::new(),
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
            trigger_event_needed: false,
        },
        each_plan: None,
        joins: vec![],
        r#where: None,
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
        stats_plan: None,
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
