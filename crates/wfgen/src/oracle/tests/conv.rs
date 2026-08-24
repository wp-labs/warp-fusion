use super::*;

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
            trigger_event_needed: true,
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
        stats_plan: None,
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

