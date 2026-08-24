use super::*;

fn make_deferred_q9_plan() -> RulePlan {
    use wf_lang::ast::{Bound, BoundVal, JoinMode, ReduceMeasure, TieSpec, WithinSpec};
    use wf_lang::plan::{EachPlan, JoinCondPlan, JoinPlan, YieldField};

    let mut plan = make_simple_rule_plan();
    plan.name = "q9_deferred".to_string();
    plan.binds = vec![BindPlan {
        alias: "a".to_string(),
        window: "auction_events".to_string(),
        filter: None,
    }];
    plan.each_plan = Some(EachPlan {
        alias: "a".to_string(),
        filter: None,
    });
    plan.match_plan.keys = vec![];
    plan.entity_plan = EntityPlan {
        entity_type: "digit".to_string(),
        entity_id_expr: Expr::Field(FieldRef::Simple("id".to_string())),
    };
    plan.joins = vec![JoinPlan {
        right_window: "bid_events".to_string(),
        mode: JoinMode::Inner,
        conds: vec![JoinCondPlan {
            left: FieldRef::Qualified("a".into(), "id".into()),
            right: FieldRef::Qualified("bid_events".into(), "auction".into()),
        }],
        within: Some(WithinSpec {
            lo: Bound {
                open: false,
                val: BoundVal::Expr(Expr::Field(FieldRef::Qualified(
                    "a".into(),
                    "dateTime".into(),
                ))),
            },
            hi: Bound {
                open: false,
                val: BoundVal::Expr(Expr::Field(FieldRef::Qualified(
                    "a".into(),
                    "expires".into(),
                ))),
            },
        }),
        reduce: Some(wf_lang::ast::ReduceClause {
            measure: ReduceMeasure::Maxrow {
                field: FieldRef::Simple("price".into()),
                tie: Some(TieSpec {
                    field: FieldRef::Simple("dateTime".into()),
                    desc: false,
                }),
            },
            label: Some("winner".into()),
        }),
        emit_at: Some(Expr::Field(FieldRef::Qualified(
            "a".into(),
            "expires".into(),
        ))),
    }];
    plan.yield_plan.fields = vec![
        YieldField {
            name: "id".into(),
            value: Expr::Field(FieldRef::Simple("id".into())),
        },
        YieldField {
            name: "winner_bidder".into(),
            value: Expr::Field(FieldRef::Path {
                alias: "winner".into(),
                segments: vec![wf_lang::ast::PathSegment::Field("bidder".into())],
            }),
        },
    ];
    plan
}

/// 驱动 auction：id/dateTime/expires（epoch nanos f64）。
fn make_q9_auction_event(id: u64, date_time: &str, expires: &str) -> GenEvent {
    let mut fields = serde_json::Map::new();
    fields.insert("id".into(), serde_json::Value::Number(id.into()));
    fields.insert(
        "dateTime".into(),
        serde_json::Value::Number(serde_json::Number::from_f64(dt_nanos(date_time)).unwrap()),
    );
    fields.insert(
        "expires".into(),
        serde_json::Value::Number(serde_json::Number::from_f64(dt_nanos(expires)).unwrap()),
    );
    GenEvent {
        stream_name: "auction".to_string(),
        window_name: "auction_events".to_string(),
        timestamp: date_time.parse().unwrap(),
        fields,
    }
}

/// 右窗 bid：auction/bidder/price/dateTime。
fn make_q9_bid_event(auction: u64, bidder: u64, price: u64, date_time: &str) -> GenEvent {
    let mut fields = serde_json::Map::new();
    fields.insert("auction".into(), serde_json::Value::Number(auction.into()));
    fields.insert("bidder".into(), serde_json::Value::Number(bidder.into()));
    fields.insert("price".into(), serde_json::Value::Number(price.into()));
    fields.insert(
        "dateTime".into(),
        serde_json::Value::Number(serde_json::Number::from_f64(dt_nanos(date_time)).unwrap()),
    );
    GenEvent {
        stream_name: "bid".to_string(),
        window_name: "bid_events".to_string(),
        timestamp: date_time.parse().unwrap(),
        fields,
    }
}

fn dt_nanos(ts: &str) -> f64 {
    ts.parse::<chrono::DateTime<Utc>>()
        .unwrap()
        .timestamp_nanos_opt()
        .unwrap() as f64
}

/// Q9 形状：auction 挂起 → 后续事件推进 watermark 过 expires → 到期评估输出
/// 胜者（maxrow(price)）；未到期不输出；无 bid 的 auction 到期不输出。
#[test]
fn deferred_q9_emits_winner_when_watermark_passes_expiry() {
    use crate::oracle::run_oracle_events_full;

    let plan = make_deferred_q9_plan();
    let schemas = vec![bid_events_schema()];
    let start: chrono::DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    let duration = Duration::from_secs(600);

    // 时间序：auction5(T, expires=T+60s) → bid 100(T+10s) → bid 200(T+20s)
    // → auction6(T+61s, 无 bid) → auction7(T+70s, 无 bid)
    let events = vec![
        make_q9_auction_event(5, "2024-01-01T00:01:00Z", "2024-01-01T00:02:00Z"),
        make_q9_bid_event(5, 1, 100, "2024-01-01T00:01:10Z"),
        make_q9_bid_event(5, 2, 200, "2024-01-01T00:01:20Z"),
        make_q9_auction_event(6, "2024-01-01T00:02:01Z", "2024-01-01T00:03:01Z"),
        make_q9_auction_event(7, "2024-01-01T00:02:10Z", "2024-01-01T00:03:10Z"),
    ];

    // close_at_eos = true：EOS flush 触发剩余（auction 6/7 无 bid → 不输出）
    let result =
        run_oracle_events_full(events, &[plan], &schemas, &start, &duration, None, true).unwrap();

    assert_eq!(result.alerts.len(), 1, "只有 auction 5 输出（其余无 bid）");
    assert_eq!(result.alerts[0].entity_id, "5");
    assert_eq!(result.alerts[0].origin, "deferred");
    // fired_at = 到期 watermark = a.expires
    assert_eq!(result.alerts[0].emit_time, "2024-01-01T00:02:00.000Z");
}

/// 未到期不输出（watermark 未过 expiry）；后续到达的 bid 不算（事件时间序保证）。
#[test]
fn deferred_q9_not_due_before_expiry() {
    use crate::oracle::run_oracle_events_full;

    let plan = make_deferred_q9_plan();
    let schemas = vec![bid_events_schema()];
    let start: chrono::DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
    // eos 水位 = start + duration = 00:01:00，在 auction5 expires（00:02:00）之前
    // → eos 扫不触发到期，尾部实例不输出（镜像引擎：水位不达 expiry 不输出）。
    let duration = Duration::from_secs(60);

    // auction5 expires=T+60s；后续事件 watermark 都不超过 T+60s
    let events = vec![
        make_q9_auction_event(5, "2024-01-01T00:01:00Z", "2024-01-01T00:02:00Z"),
        make_q9_bid_event(5, 1, 100, "2024-01-01T00:01:10Z"),
        make_q9_auction_event(6, "2024-01-01T00:01:30Z", "2024-01-01T00:02:30Z"),
    ];
    let result = run_oracle_events_full(
        events,
        &[plan],
        &schemas,
        &start,
        &duration,
        None,
        false, // verify 模式：不 EOS flush → 尾部未到期不输出
    )
    .unwrap();

    assert_eq!(
        result.alerts.len(),
        0,
        "watermark(00:01:30) < expires(00:02:00) 且 eos(00:01:00) < expires → 不输出"
    );
}
