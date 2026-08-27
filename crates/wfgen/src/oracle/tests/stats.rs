//! StatsOracleEngine（2026-08-27 接入 oracle）单元测试:
//! fixed 窗口 bucket 对齐推进 / 窗口边界（含恰在边界）收口 / 容量阈值分批归并
//! 的行无丢失语义（OOM 修复回归）/ top 多条目 close 计数 / 流末收口 /
//! gen_event_to_row 字段转换。

use std::collections::HashMap;
use std::time::Duration;

use wf_engine::match_engine::Value;
use wf_lang::ast::{Expr, FieldRef};
use wf_lang::plan::{
    EntityPlan, RulePlan, ScorePlan, StatsAggPlan, StatsMeasurePlan, StatsOutputShapePlan,
    StatsPlan, WindowSpec, YieldPlan,
};

use crate::datagen::stream_gen::GenEvent;

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

/// fixed 窗口 stats 规则计划（桶键 = 可选单字段; 空 = 空键全局单桶）。
fn stats_rule(
    name: &str,
    window_secs: u64,
    key: Option<&str>,
    measures: Vec<StatsMeasurePlan>,
) -> RulePlan {
    let mut plan = make_simple_rule_plan();
    plan.name = name.to_string();
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

fn secs(t: u64) -> i64 {
    (t as i64) * 1_000_000_000
}

// ---- 窗口推进 / 边界收口 ----

#[test]
fn stats_oracle_advances_windows_and_closes_at_boundary() {
    let plan = stats_rule("s1", 10, Some("auction"), vec![count_measure("n")]);
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
    let plan = stats_rule("s_empty", 10, None, vec![count_measure("n")]);
    let mut se = StatsOracleEngine::new(&plan);
    assert!(se.close_tail().is_empty(), "无事件 → 无窗口 → 无 close");
}

// ---- top 多条目（每桶 n_records 条 close） ----

#[test]
fn stats_oracle_top_emits_n_records_per_bucket() {
    let plan = stats_rule("s_top", 10, None, vec![top_measure("top3", 3, "price")]);
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
