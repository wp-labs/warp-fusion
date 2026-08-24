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
            expr: Expr::Number(85.0),
        },
        pattern_origin: None,
        conv_plan: None,
        limits_plan: None,
        conv_window: None,
        stats_plan: None,
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

fn bid_events_schema() -> wf_lang::WindowSchema {
    wf_lang::WindowSchema {
        name: "bid_events".to_string(),
        streams: vec!["bid".to_string()],
        time_field: Some("dateTime".to_string()),
        over: Duration::from_secs(3600),
        fields: vec![
            wf_lang::FieldDef {
                name: "auction".to_string(),
                field_type: wf_lang::FieldType::Base(wf_lang::BaseType::Digit),
            },
            wf_lang::FieldDef {
                name: "bidder".to_string(),
                field_type: wf_lang::FieldType::Base(wf_lang::BaseType::Digit),
            },
            wf_lang::FieldDef {
                name: "price".to_string(),
                field_type: wf_lang::FieldType::Base(wf_lang::BaseType::Digit),
            },
            wf_lang::FieldDef {
                name: "dateTime".to_string(),
                field_type: wf_lang::FieldType::Base(wf_lang::BaseType::Time),
            },
        ],
    }
}

mod basic;
mod conv;
mod join;
mod deferred;
