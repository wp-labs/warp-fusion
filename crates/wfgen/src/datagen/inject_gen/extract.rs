use std::collections::HashMap;

use wf_lang::ast::{BinOp, Expr, FieldRef};
use wf_lang::plan::RulePlan;
use wf_lang::plan::WindowSpec;

use super::structures::{
    AliasMap, InjectOverrides, InjectUseStepOverrides, RuleStructure, StepInfo,
};
use crate::wfg_ast::{InjectLine, ParamValue};

pub(super) fn extract_rule_structure(
    rule_plan: &RulePlan,
    alias_map: &AliasMap,
) -> anyhow::Result<RuleStructure> {
    let window_dur = match rule_plan.match_plan.window_spec {
        WindowSpec::Sliding(d) | WindowSpec::Fixed(d) | WindowSpec::Session(d) => d,
    };

    let keys: Vec<String> = rule_plan
        .match_plan
        .keys
        .iter()
        .map(|fr| field_ref_field_name(fr).to_string())
        .collect();

    let mut steps = Vec::new();
    for step_plan in &rule_plan.match_plan.event_steps {
        // P1: take first branch
        let branch = step_plan
            .branches
            .first()
            .ok_or_else(|| anyhow::anyhow!("step has no branches"))?;

        let bind_alias = &branch.source;

        // SC6: inject streams are a *subset* of rule aliases.
        // Skip steps whose bind alias is not covered by inject.
        let (scenario_alias, window_name) = match alias_map.bind_to_scenario.get(bind_alias) {
            Some(pair) => pair,
            None => continue,
        };

        let threshold = eval_const_threshold(&branch.agg.threshold)
            .ok_or_else(|| anyhow::anyhow!("cannot evaluate threshold as constant"))?
            as u64;

        // Extract filter constraints from the corresponding bind
        let filter_overrides = rule_plan
            .binds
            .iter()
            .find(|b| b.alias == *bind_alias)
            .and_then(|b| b.filter.as_ref())
            .map(extract_filter_constraints)
            .unwrap_or_default();

        steps.push(StepInfo {
            bind_alias: bind_alias.clone(),
            scenario_alias: scenario_alias.clone(),
            window_name: window_name.clone(),
            measure: branch.agg.measure,
            threshold,
            filter_overrides,
        });
    }

    if steps.is_empty() {
        anyhow::bail!(
            "no inject streams map to any step in rule '{}'; \
             at least one inject alias must match a rule bind alias",
            rule_plan.name
        );
    }

    let entity_id_field = extract_entity_id_field(&rule_plan.entity_plan.entity_id_expr);

    Ok(RuleStructure {
        keys,
        window_dur,
        steps,
        entity_id_field,
    })
}

/// Extract a constant numeric value from an expression (L1 thresholds).
pub(crate) fn eval_const_threshold(expr: &Expr) -> Option<f64> {
    match expr {
        Expr::Number(n) => Some(*n),
        Expr::Neg(inner) => eval_const_threshold(inner).map(|v| -v),
        _ => None,
    }
}

pub(crate) fn field_ref_field_name(fr: &FieldRef) -> &str {
    match fr {
        FieldRef::Simple(name) => name,
        FieldRef::Qualified(_, name) | FieldRef::Bracketed(_, name) => name,
        _ => "",
    }
}

pub(super) fn extract_inject_overrides(inject_line: &InjectLine) -> InjectOverrides {
    let mut overrides = InjectOverrides {
        count_per_entity: None,
        steps_completed: None,
        within: None,
        use_steps: Vec::new(),
    };

    for param in &inject_line.params {
        match param.name.as_str() {
            "count_per_entity" => {
                if let ParamValue::Number(n) = &param.value {
                    overrides.count_per_entity = Some(*n as u64);
                }
            }
            "steps_completed" => {
                if let ParamValue::Number(n) = &param.value {
                    overrides.steps_completed = Some(*n as usize);
                }
            }
            "within" => {
                if let ParamValue::Duration(d) = &param.value {
                    overrides.within = Some(*d);
                }
            }
            _ => {}
        }
    }

    for use_step in &inject_line.use_steps {
        let mut predicates = HashMap::new();
        for pred in &use_step.predicates {
            if let Some(value) = attr_value_to_json(&pred.value) {
                predicates.insert(pred.field.clone(), value);
            }
        }
        overrides.use_steps.push(InjectUseStepOverrides {
            count: use_step.count,
            predicates,
        });
    }

    overrides
}

fn attr_value_to_json(value: &crate::wfg_ast::AttrValue) -> Option<serde_json::Value> {
    match value {
        crate::wfg_ast::AttrValue::String(s) => Some(serde_json::Value::String(s.clone())),
        crate::wfg_ast::AttrValue::Number(n) => Some(serde_json::json!(*n)),
        crate::wfg_ast::AttrValue::Bool(b) => Some(serde_json::Value::Bool(*b)),
        crate::wfg_ast::AttrValue::Duration(d) => {
            Some(serde_json::Value::String(format!("{:?}", d)))
        }
    }
}

fn extract_entity_id_field(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Field(fr) => Some(field_ref_field_name(fr).to_string()),
        _ => None,
    }
}

/// Extract field equality constraints from a filter expression.
///
/// Supports:
/// - `field == "value"`, `field == number`, `field == bool`
/// - `cond1 && cond2` (recursively extracts from both sides)
pub(crate) fn extract_filter_constraints(filter: &Expr) -> HashMap<String, serde_json::Value> {
    let mut constraints = HashMap::new();
    extract_filter_constraints_recursive(filter, &mut constraints);
    constraints
}

fn extract_filter_constraints_recursive(
    expr: &Expr,
    constraints: &mut HashMap<String, serde_json::Value>,
) {
    if let Expr::BinOp { op, left, right } = expr {
        match op {
            BinOp::And => {
                // Recursively handle AND-connected conditions
                extract_filter_constraints_recursive(left, constraints);
                extract_filter_constraints_recursive(right, constraints);
            }
            BinOp::Eq => {
                // Extract field == value
                if let Expr::Field(fr) = left.as_ref() {
                    let field_name = field_ref_field_name(fr);
                    if let Some(value) = expr_to_json_value(right.as_ref()) {
                        constraints.insert(field_name.to_string(), value);
                    }
                }
                // Also handle value == field
                if let Expr::Field(fr) = right.as_ref() {
                    let field_name = field_ref_field_name(fr);
                    if let Some(value) = expr_to_json_value(left.as_ref()) {
                        constraints.insert(field_name.to_string(), value);
                    }
                }
            }
            _ => {}
        }
    }
}

fn expr_to_json_value(expr: &Expr) -> Option<serde_json::Value> {
    match expr {
        Expr::StringLit(s) => Some(serde_json::Value::String(s.clone())),
        Expr::Number(n) => Some(serde_json::json!(*n)),
        Expr::Bool(b) => Some(serde_json::Value::Bool(*b)),
        _ => None,
    }
}
