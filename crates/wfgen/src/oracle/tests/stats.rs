//! StatsOracleEngine（2026-08-27 接入 oracle）单元测试:
//! fixed 窗口 bucket 对齐推进 / 窗口边界（含恰在边界）收口 / 容量阈值分批归并
//! 的行无丢失语义（OOM 修复回归）/ top 多条目 close 计数 / 流末收口 /
//! gen_event_to_row 字段转换。

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use wf_engine::match_engine::Value;
use wf_lang::ast::{
    Bound, BoundVal, Expr, FieldRef, JoinMode, ReduceClause, ReduceMeasure, WithinSpec,
};
use wf_lang::plan::{
    BindPlan, EachPlan, JoinCondPlan, JoinPlan, RulePlan, StatsAggPlan, StatsMeasurePlan,
    StatsOutputShapePlan, StatsPlan, WindowSpec, YieldField, YieldPlan,
};

use crate::datagen::stream_gen::GenEvent;
use crate::oracle::run_oracle_events_full;

use super::super::{FLUSH_PENDING_ROWS, StatsOracleEngine, gen_event_to_row};
use super::*;

// ---- helpers ----

fn row(fields: &[(&str, Value)]) -> HashMap<String, Value> {
    fields
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

fn count_measure(label: &str) -> StatsMeasurePlan {
    StatsMeasurePlan {
        label: label.to_string(),
        source_alias: "b".to_string(),
        where_expr: None,
        agg: StatsAggPlan::Count,
        field: None,
        arg: None,
    }
}

fn top_measure(label: &str, n: u64, field: &str) -> StatsMeasurePlan {
    StatsMeasurePlan {
        label: label.to_string(),
        source_alias: "b".to_string(),
        where_expr: None,
        agg: StatsAggPlan::Top,
        field: Some(FieldRef::Simple(field.to_string())),
        arg: Some(n),
    }
}

/// fixed 窗口 stats 规则计划（绑定单个源窗口; 桶键 = 可选单字段; 空 = 空键全局单桶）。
fn stats_rule(
    name: &str,
    bind_window: &str,
    window_secs: u64,
    key: Option<&str>,
    measures: Vec<StatsMeasurePlan>,
) -> RulePlan {
    let mut plan = make_simple_rule_plan();
    plan.name = name.to_string();
    plan.binds = vec![BindPlan {
        alias: "b".to_string(),
        window: bind_window.to_string(),
        filter: None,
    }];
    plan.stats_plan = Some(StatsPlan {
        window_spec: WindowSpec::Fixed(Duration::from_secs(window_secs)),
        keys: key
            .map(|k| Expr::Field(FieldRef::Simple(k.to_string())))
            .into_iter()
            .collect(),
        output_shape: StatsOutputShapePlan::Rows,
        measures,
        tracked_bind_fields: HashMap::new(),
    });
    plan
}

/// `on each` 逐事件 producer: 把绑定窗口的每行 yield 成中间窗口（q4a→finals 同款）。
fn each_producer_plan(name: &str, bind_window: &str, yield_target: &str, field: &str) -> RulePlan {
    let mut plan = make_simple_rule_plan();
    plan.name = name.to_string();
    plan.binds = vec![BindPlan {
        alias: "a".to_string(),
        window: bind_window.to_string(),
        filter: None,
    }];
    plan.each_plan = Some(EachPlan {
        alias: "a".to_string(),
        filter: None,
    });
    plan.yield_plan = YieldPlan {
        target: yield_target.to_string(),
        version: None,
        fields: vec![YieldField {
            name: field.to_string(),
            value: Expr::Field(FieldRef::Qualified("a".into(), field.into())),
        }],
    };
    plan
}

/// 构造带桶键字段的 GenEvent（时间 = epoch + t_secs）。
fn gen_event(window: &str, k: &str, t_secs: u64) -> GenEvent {
    let mut fields = serde_json::Map::new();
    fields.insert("k".to_string(), serde_json::json!(k));
    GenEvent {
        stream_name: window.to_string(),
        window_name: window.to_string(),
        timestamp: DateTime::<Utc>::from_timestamp(t_secs as i64, 0).unwrap(),
        fields,
    }
}

fn secs(t: u64) -> i64 {
    (t as i64) * 1_000_000_000
}

// ---- 窗口推进 / 边界收口 ----

#[test]
fn stats_oracle_advances_windows_and_closes_at_boundary() {
    let plan = stats_rule(
        "s1",
        "bid_events",
        10,
        Some("auction"),
        vec![count_measure("n")],
    );
    let mut se = StatsOracleEngine::new(&plan);

    // 窗口 [0,10s): A×1, B×1 → 恰在 t=10s（边界）的事件收口
    let mut total = 0usize;
    assert!(
        se.feed(secs(1), &row(&[("auction", Value::Str("A".into()))]))
            .is_empty()
    );
    assert!(
        se.feed(secs(5), &row(&[("auction", Value::Str("B".into()))]))
            .is_empty()
    );
    let closed = se.feed(secs(10), &row(&[("auction", Value::Str("A".into()))]));
    assert_eq!(
        closed.len(),
        2,
        "边界事件须先收口旧窗口 [0,10s)（A、B 两桶）"
    );
    total += closed.len();

    // 窗口 [10s,20s): B×1, A×1 → t=20s（边界）收口
    assert!(
        se.feed(secs(15), &row(&[("auction", Value::Str("B".into()))]))
            .is_empty()
    );
    let closed = se.feed(secs(20), &row(&[("auction", Value::Str("A".into()))]));
    assert_eq!(closed.len(), 2, "第二个边界收口 [10s,20s)（A、B 两桶）");
    total += closed.len();

    // 流末: 尾部窗口 [20s,30s) 仅 A
    let tail = se.close_tail();
    assert_eq!(tail.len(), 1, "流末收口尾部窗口（仅 A）");
    total += tail.len();
    assert_eq!(total, 5);

    // 每帧字段: 规则名/score/实体类型/origin/emit_time 口径
    for a in closed.iter().chain(tail.iter()) {
        assert_eq!(a.rule_name, "s1");
        assert_eq!(a.score, 85.0, "score 取 score_plan 数字");
        assert_eq!(a.entity_type, "ip", "entity_type 取 entity_plan");
        assert_eq!(a.origin, "close");
        assert!(
            chrono::DateTime::parse_from_rfc3339(&a.emit_time).is_ok(),
            "emit_time 须为 RFC3339, got {}",
            a.emit_time
        );
    }
    // 桶键 → entity_id（ScopeKey Debug 形状含键值）
    for a in tail.iter().chain(closed.iter()) {
        assert!(
            a.entity_id.contains("A") || a.entity_id.contains("B"),
            "entity_id 应含桶键值, got {}",
            a.entity_id
        );
    }
}

// ---- 空流 ----

#[test]
fn stats_oracle_empty_stream_emits_nothing() {
    let plan = stats_rule("s_empty", "bid_events", 10, None, vec![count_measure("n")]);
    let mut se = StatsOracleEngine::new(&plan);
    assert!(se.close_tail().is_empty(), "无事件 → 无窗口 → 无 close");
}

// ---- top 多条目（每桶 n_records 条 close） ----

#[test]
fn stats_oracle_top_emits_n_records_per_bucket() {
    let plan = stats_rule(
        "s_top",
        "bid_events",
        10,
        None,
        vec![top_measure("top3", 3, "price")],
    );
    let mut se = StatsOracleEngine::new(&plan);
    for (i, price) in [5.0, 1.0, 9.0, 3.0, 7.0].into_iter().enumerate() {
        let t = secs(1 + i as u64);
        assert!(
            se.feed(t, &row(&[("price", Value::Number(price))]))
                .is_empty()
        );
    }
    let closed = se.close_tail();
    assert_eq!(closed.len(), 3, "top(3) 每桶 3 条 → 空键单桶 → 3 条 close");
    for a in &closed {
        assert_eq!(a.rule_name, "s_top");
    }
}

// ---- 容量阈值分批归并: 行无丢失（OOM 修复回归） ----

#[test]
fn stats_oracle_flush_threshold_loses_no_rows() {
    let plan = stats_rule(
        "s_thresh",
        "bid_events",
        3600, // 1h 窗口: 整窗行数 > 阈值才触发 OOM 场景
        Some("auction"),
        vec![count_measure("n")],
    );
    let mut se = StatsOracleEngine::new(&plan);
    let n = FLUSH_PENDING_ROWS + 100; // 越过阈值 → 窗口中途 flush 一次 + close 前再 flush

    // 每行唯一桶键 → close 桶数 = 行数; 若分批归并丢/重任何行, 桶数即变。
    let mut alerts = 0usize;
    for i in 0..n {
        let key = format!("A{i:06}");
        alerts += se
            .feed(secs(1), &row(&[("auction", Value::Str(key.into()))]))
            .len();
    }
    assert!(alerts == 0, "未跨边界不应有 close");
    let tail = se.close_tail();
    assert_eq!(
        tail.len(),
        n,
        "阈值分批归并不得丢行: close 桶数须等于行数 ({}), got {}",
        n,
        tail.len()
    );
}

// ---- gen_event_to_row 字段转换 ----

#[test]
fn gen_event_to_row_converts_primitives_and_drops_structured() {
    let mut fields = serde_json::Map::new();
    fields.insert("id".to_string(), serde_json::json!(42));
    fields.insert("name".to_string(), serde_json::json!("alice"));
    fields.insert("active".to_string(), serde_json::json!(true));
    fields.insert("extra".to_string(), serde_json::json!({"nested": [1, 2]}));
    let ev = GenEvent {
        stream_name: "bid".to_string(),
        window_name: "bid_events".to_string(),
        timestamp: chrono::Utc::now(),
        fields,
    };
    let r = gen_event_to_row(&ev);
    assert_eq!(r.get("id"), Some(&Value::Number(42.0)));
    assert_eq!(r.get("name"), Some(&Value::Str("alice".into())));
    assert_eq!(r.get("active"), Some(&Value::Bool(true)));
    assert!(!r.contains_key("extra"), "复合类型丢弃（stats 度量不读）");
}

// ---- 绑定窗口过滤（2026-08-27 review: 过度喂入修复） ----

#[test]
fn stats_oracle_feeds_only_bound_window() {
    // 引擎 StatsTask 只消费规则 binds 的窗口（window_sources）; oracle 主循环
    // 也必须只喂绑定行——否则空键 + 无 where 度量会把非绑定流行计进去
    // （q15 `total` 10M 数据 920 万 bid 被计成 1000 万）; 键式规则靠键缺失跳过
    // 掩幸, 但值语义仍是错的, verify 只对拍 alert 数测不出。
    let plan = stats_rule(
        "s_bound",
        "bid_events",
        10,
        Some("k"),
        vec![count_measure("n")],
    );
    let se = StatsOracleEngine::new(&plan);
    assert!(se.accepts("bid_events"), "绑定窗口须接受");
    assert!(!se.accepts("auction_events"), "未绑定窗口不得喂入");

    // 端到端: 绑定 bid_events, 事件流混入 auction_events（也带 k 字段, 若喂入
    // 会建桶）——非绑定行不得计入桶, close 桶数 = 仅 bid 行键数 3（A/B/C）。
    let events = vec![
        gen_event("bid_events", "A", 1),
        gen_event("auction_events", "D", 2),
        gen_event("bid_events", "B", 3),
        gen_event("auction_events", "E", 4),
        gen_event("bid_events", "C", 5),
    ];
    let start = DateTime::<Utc>::from_timestamp(0, 0).unwrap();
    let res = run_oracle_events_full(
        events,
        &[plan],
        &[],
        &start,
        &Duration::from_secs(1000),
        None,
        false,
    )
    .unwrap();
    assert_eq!(
        res.alerts.len(),
        3,
        "只喂绑定窗口行: close 桶数 = 3（A/B/C）, 非绑定 D/E 不得建桶"
    );
}

// ---- 中间窗口喂入（2026-08-27 review: 欠喂中间流修复） ----

#[test]
fn stats_oracle_feeds_intermediate_window() {
    // q4a→auction_finals→q4b 双规则链: stats 规则绑定中间窗口, 必须收到
    // producer 的 yield 事件（此前只喂原始流——q4b oracle 只看到 auction_events
    // 同名 category 桶, 桶数巧合一致但 avg(f.final) 缺字段贡献 0, 值全错）。
    let producer = each_producer_plan("prod", "in", "mid", "k");
    let stats = stats_rule("s_mid", "mid", 10, Some("k"), vec![count_measure("n")]);
    let events = vec![
        gen_event("in", "A", 1),
        gen_event("in", "B", 2),
        gen_event("in", "A", 3),
    ];
    let start = DateTime::<Utc>::from_timestamp(0, 0).unwrap();
    let res = run_oracle_events_full(
        events,
        &[producer, stats],
        &[],
        &start,
        &Duration::from_secs(1000),
        None,
        false,
    )
    .unwrap();
    let stats_alerts: Vec<_> = res
        .alerts
        .iter()
        .filter(|a| a.rule_name == "s_mid")
        .collect();
    assert_eq!(
        stats_alerts.len(),
        2,
        "stats 收到 3 条中间行（A×2, B×1）→ 2 桶（A/B）; 原始流 in 行不得直接建桶"
    );
}

// ---- deferred producer 的中间输出也须入队（2026-08-27 review: push_alert→record_output） ----

/// deferred `on each a` + join reduce + `emit at a.expires` producer: 到期 yield
/// 中间窗口（q4a→auction_finals 同款）。
fn deferred_producer_plan(name: &str, yield_target: &str) -> RulePlan {
    let mut plan = make_simple_rule_plan();
    plan.name = name.to_string();
    plan.binds = vec![BindPlan {
        alias: "a".to_string(),
        window: "in".to_string(),
        filter: None,
    }];
    plan.each_plan = Some(EachPlan {
        alias: "a".to_string(),
        filter: None,
    });
    plan.yield_plan = YieldPlan {
        target: yield_target.to_string(),
        version: None,
        fields: vec![YieldField {
            name: "k".to_string(),
            value: Expr::Field(FieldRef::Qualified("a".into(), "k".into())),
        }],
    };
    plan.entity_plan = wf_lang::plan::EntityPlan {
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
        reduce: Some(ReduceClause {
            measure: ReduceMeasure::Maxrow {
                field: FieldRef::Simple("price".into()),
                tie: None,
            },
            label: Some("winner".into()),
        }),
        emit_at: Some(Expr::Field(FieldRef::Qualified(
            "a".into(),
            "expires".into(),
        ))),
    }];
    plan
}

/// 真实 epoch 基准（2024-01-01T00:00:00Z）——字段时间戳必须是真实量级,
/// 否则 `normalize_epoch_timestamp_float_nanos` 的量级启发式会把小数值误判为
/// 毫秒/微秒单位（epoch+秒数 → ×1e6 → 到期时间漂移到 ~694 天后, 永不触发）。
const BASE_NANOS: i64 = 1_704_067_200_000_000_000;
const BASE_SECS: i64 = 1_704_067_200;

fn gen_driver_event(id: u64, k: &str, t_secs: i64, expires_secs: i64) -> GenEvent {
    let mut fields = serde_json::Map::new();
    fields.insert("id".to_string(), serde_json::json!(id));
    fields.insert("k".to_string(), serde_json::json!(k));
    fields.insert(
        "dateTime".to_string(),
        serde_json::json!(BASE_NANOS + t_secs * 1_000_000_000),
    );
    fields.insert(
        "expires".to_string(),
        serde_json::json!(BASE_NANOS + expires_secs * 1_000_000_000),
    );
    GenEvent {
        stream_name: "in".to_string(),
        window_name: "in".to_string(),
        timestamp: DateTime::<Utc>::from_timestamp(BASE_SECS + t_secs, 0).unwrap(),
        fields,
    }
}

fn gen_right_bid_event(auction: u64, price: u64, t_secs: i64) -> GenEvent {
    let mut fields = serde_json::Map::new();
    fields.insert("auction".to_string(), serde_json::json!(auction));
    fields.insert("price".to_string(), serde_json::json!(price));
    fields.insert(
        "dateTime".to_string(),
        serde_json::json!(BASE_NANOS + t_secs * 1_000_000_000),
    );
    GenEvent {
        stream_name: "bid".to_string(),
        window_name: "bid_events".to_string(),
        timestamp: DateTime::<Utc>::from_timestamp(BASE_SECS + t_secs, 0).unwrap(),
        fields,
    }
}

#[test]
fn stats_oracle_feeds_deferred_producer_intermediate() {
    // deferred 输出走 record_output（review 改造前是 push_alert 直接计数, 中间
    // 输出不入 feed_queue → 绑定中间窗口的 stats 规则收不到, q4b 实为 0）。
    let producer = deferred_producer_plan("prod_d", "mid");
    let stats = stats_rule("s_mid_d", "mid", 10, Some("k"), vec![count_measure("n")]);
    let schemas = vec![bid_events_schema()];
    let start = DateTime::<Utc>::from_timestamp(0, 0).unwrap();
    // auction5(expires=60s) → bid(10s) → auction6(expires=90s) → bid(40s)
    // → auction7(65s, watermark 65s ≥ 60s → auction5 到期 yield 中间行 k=catA)
    let events = vec![
        gen_driver_event(5, "catA", 0, 60),
        gen_right_bid_event(5, 100, 10),
        gen_driver_event(6, "catB", 30, 90),
        gen_right_bid_event(6, 200, 40),
        gen_driver_event(7, "catA", 65, 125),
    ];
    let res = run_oracle_events_full(
        events,
        &[producer, stats],
        &schemas,
        &start,
        &Duration::from_secs(1000),
        None,
        false,
    )
    .unwrap();
    let stats_alerts: Vec<_> = res
        .alerts
        .iter()
        .filter(|a| a.rule_name == "s_mid_d")
        .collect();
    assert_eq!(
        stats_alerts.len(),
        1,
        "deferred 中间行（auction5→catA）须喂入 stats → 1 桶; 其余 auction 未到期不输出"
    );
}
