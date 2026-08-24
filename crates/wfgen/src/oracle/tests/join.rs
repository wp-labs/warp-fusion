use super::*;
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
            trigger_event_needed: false,
        },
        each_plan: None,
        joins: vec![wf_lang::plan::JoinPlan {
            right_window: "auction_events".to_string(),
            mode: wf_lang::ast::JoinMode::Snapshot,
            conds: vec![wf_lang::plan::JoinCondPlan {
                left: FieldRef::Qualified("b".into(), "auction".into()),
                right: FieldRef::Qualified("auction_events".into(), "id".into()),
            }],
            within: None,
            reduce: None,
            emit_at: None,
        }],
        r#where: None,
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
        stats_plan: None,
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
    fields.insert("auction".into(), serde_json::Value::Number(auction.into()));
    fields.insert("price".into(), serde_json::Value::Number(price.into()));
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
