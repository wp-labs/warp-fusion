mod compat;
mod event;
mod fault;
mod inject;

use std::time::Duration;

use wf_lang::ast::{BinOp, CloseMode, CmpOp, Expr, FieldRef, Measure};
use wf_lang::plan::{
    AggPlan, BindPlan, BranchPlan, EntityPlan, MatchPlan, RulePlan, ScorePlan, StepPlan,
    WindowSpec, YieldPlan,
};
use wf_lang::{BaseType, FieldDef, FieldType, WindowSchema};

use super::generate;
use crate::wfg_parser::parse_wfg;

fn make_login_schema() -> WindowSchema {
    WindowSchema {
        name: "LoginWindow".to_string(),
        streams: vec!["login_events".to_string()],
        time_field: Some("timestamp".to_string()),
        over: Duration::from_secs(300),
        fields: vec![
            FieldDef {
                name: "timestamp".to_string(),
                field_type: FieldType::Base(BaseType::Time),
            },
            FieldDef {
                name: "src_ip".to_string(),
                field_type: FieldType::Base(BaseType::Ip),
            },
            FieldDef {
                name: "username".to_string(),
                field_type: FieldType::Base(BaseType::Chars),
            },
            FieldDef {
                name: "success".to_string(),
                field_type: FieldType::Base(BaseType::Bool),
            },
            FieldDef {
                name: "attempts".to_string(),
                field_type: FieldType::Base(BaseType::Digit),
            },
            FieldDef {
                name: "score".to_string(),
                field_type: FieldType::Base(BaseType::Float),
            },
            FieldDef {
                name: "request_id".to_string(),
                field_type: FieldType::Base(BaseType::Hex),
            },
        ],
    }
}

fn make_brute_force_plan() -> RulePlan {
    RulePlan {
        name: "brute_force".to_string(),
        binds: vec![BindPlan {
            alias: "fail".to_string(),
            window: "LoginWindow".to_string(),
            filter: None,
        }],
        match_plan: MatchPlan {
            keys: vec![FieldRef::Simple("src_ip".to_string())],
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
                        threshold: Expr::Number(5.0),
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
            entity_id_expr: Expr::Field(FieldRef::Simple("src_ip".to_string())),
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

/// Rule with a filter constraint: success == false
fn make_auth_fail_plan() -> RulePlan {
    RulePlan {
        name: "auth_fail_rule".to_string(),
        binds: vec![BindPlan {
            alias: "auth_fail".to_string(),
            window: "LoginWindow".to_string(),
            filter: Some(Expr::BinOp {
                op: wf_lang::ast::BinOp::Eq,
                left: Box::new(Expr::Field(FieldRef::Simple("success".to_string()))),
                right: Box::new(Expr::Bool(false)),
            }),
        }],
        match_plan: MatchPlan {
            keys: vec![FieldRef::Simple("src_ip".to_string())],
            key_map: None,
            window_spec: WindowSpec::Sliding(Duration::from_secs(300)),
            event_steps: vec![StepPlan {
                branches: vec![BranchPlan {
                    label: Some("fail_count".to_string()),
                    source: "auth_fail".to_string(),
                    field: None,
                    guard: None,
                    agg: AggPlan {
                        transforms: vec![],
                        measure: Measure::Count,
                        cmp: CmpOp::Ge,
                        threshold: Expr::Number(5.0),
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
            entity_id_expr: Expr::Field(FieldRef::Simple("src_ip".to_string())),
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
    }
}

/// Two-step rule on a single bind:
/// step1 requires success=false, then step2 requires success=true.
fn make_bool_chain_plan() -> RulePlan {
    RulePlan {
        name: "bool_chain".to_string(),
        binds: vec![BindPlan {
            alias: "evt".to_string(),
            window: "LoginWindow".to_string(),
            filter: None,
        }],
        match_plan: MatchPlan {
            keys: vec![FieldRef::Simple("username".to_string())],
            key_map: None,
            window_spec: WindowSpec::Sliding(Duration::from_secs(300)),
            event_steps: vec![
                StepPlan {
                    branches: vec![BranchPlan {
                        label: Some("step1".to_string()),
                        source: "evt".to_string(),
                        field: None,
                        guard: Some(Expr::BinOp {
                            op: BinOp::Eq,
                            left: Box::new(Expr::Field(FieldRef::Simple("success".to_string()))),
                            right: Box::new(Expr::Bool(false)),
                        }),
                        agg: AggPlan {
                            transforms: vec![],
                            measure: Measure::Count,
                            cmp: CmpOp::Ge,
                            threshold: Expr::Number(1.0),
                        },
                    }],
                },
                StepPlan {
                    branches: vec![BranchPlan {
                        label: Some("step2".to_string()),
                        source: "evt".to_string(),
                        field: None,
                        guard: Some(Expr::BinOp {
                            op: BinOp::Eq,
                            left: Box::new(Expr::Field(FieldRef::Simple("success".to_string()))),
                            right: Box::new(Expr::Bool(true)),
                        }),
                        agg: AggPlan {
                            transforms: vec![],
                            measure: Measure::Count,
                            cmp: CmpOp::Ge,
                            threshold: Expr::Number(1.0),
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
            entity_type: "user".to_string(),
            entity_id_expr: Expr::Field(FieldRef::Simple("username".to_string())),
        },
        yield_plan: YieldPlan {
            target: "alerts".to_string(),
            version: None,
            fields: vec![],
        },
        score_plan: ScorePlan {
            expr: Expr::Number(70.0),
        },
        pattern_origin: None,
        conv_plan: None,
        limits_plan: None,
    }
}
