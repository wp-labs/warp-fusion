#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use wf_core::rule::{CepStateMachine, Event, RuleExecutor, StepResult, Value};
use wf_lang::plan::{ConvPlan, RulePlan};

use crate::datagen::stream_gen::GenEvent;

/// An oracle alert produced by the reference evaluator.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OracleAlert {
    pub rule_name: String,
    pub score: f64,
    pub entity_type: String,
    pub entity_id: String,
    pub origin: String,
    /// ISO 8601 — logical time (triggering event's timestamp).
    pub emit_time: String,
}

/// Result of oracle evaluation.
pub struct OracleResult {
    pub alerts: Vec<OracleAlert>,
}

/// Run the reference evaluator on generated events.
///
/// Creates a `CepStateMachine` + `RuleExecutor` per rule, feeds events in
/// timestamp order, and collects oracle alerts. Uses event-time nanoseconds
/// for deterministic window expiry.
///
/// SC7: when `injected_rules` is `Some`, only the rules whose names appear
/// in the set are evaluated. Rules without `inject` coverage are skipped so
/// the oracle doesn't generate spurious expected hits from baseline traffic.
pub fn run_oracle(
    events: &[GenEvent],
    rule_plans: &[RulePlan],
    scenario_start: &DateTime<Utc>,
    scenario_duration: &Duration,
    injected_rules: Option<&std::collections::HashSet<String>>,
) -> anyhow::Result<OracleResult> {
    if rule_plans.is_empty() {
        return Ok(OracleResult { alerts: vec![] });
    }

    // Build per-rule engines, filtering to injected rules only (SC7)
    let mut engines: Vec<RuleEngine> = rule_plans
        .iter()
        .filter(|plan| {
            injected_rules
                .map(|set| set.contains(&plan.name))
                .unwrap_or(true)
        })
        .map(|plan| {
            let alias_map = build_window_alias_map(plan);
            RuleEngine {
                sm: CepStateMachine::new(plan.name.clone(), plan.match_plan.clone(), None),
                executor: RuleExecutor::new(plan.clone()),
                conv_plan: plan.conv_plan.clone(),
                alias_map,
            }
        })
        .collect();

    let mut alerts = Vec::new();

    // Process events in order (caller should have sorted by timestamp)
    for event in events {
        let event_nanos = event.timestamp.timestamp_nanos_opt().unwrap_or(0);

        let core_event = gen_event_to_core(event);

        for engine in &mut engines {
            // Scan for expired instances first (with conv)
            let expired = engine
                .sm
                .scan_expired_at_with_conv(event_nanos, engine.conv_plan.as_ref());
            for close_out in expired {
                if let Ok(Some(alert_record)) = engine.executor.execute_close(&close_out) {
                    alerts.push(OracleAlert {
                        rule_name: alert_record.rule_name,
                        score: alert_record.score,
                        entity_type: alert_record.entity_type,
                        entity_id: alert_record.entity_id,
                        origin: alert_record.origin.as_str().to_string(),
                        emit_time: alert_record.fired_at.clone(),
                    });
                }
            }

            // Find bind aliases for this event's window
            let bind_aliases = match engine.alias_map.get(&event.window_name) {
                Some(aliases) => aliases,
                None => continue, // this rule doesn't use this window
            };

            // Advance the state machine for each alias bound to this window
            for bind_alias in bind_aliases {
                if !engine
                    .executor
                    .event_matches_alias(bind_alias, &core_event, None)
                {
                    continue;
                }
                let result = engine.sm.advance_at(bind_alias, &core_event, event_nanos);

                if let StepResult::Matched(ctx) = result
                    && let Ok(alert_record) = engine.executor.execute_match(&ctx)
                {
                    alerts.push(OracleAlert {
                        rule_name: alert_record.rule_name,
                        score: alert_record.score,
                        entity_type: alert_record.entity_type,
                        entity_id: alert_record.entity_id,
                        origin: alert_record.origin.as_str().to_string(),
                        emit_time: alert_record.fired_at.clone(),
                    });
                }
            }
        }
    }

    // End-of-scenario sweep: flush remaining instances
    let eos_time =
        *scenario_start + chrono::Duration::from_std(*scenario_duration).unwrap_or_default();
    let eos_nanos = eos_time.timestamp_nanos_opt().unwrap_or(i64::MAX);

    for engine in &mut engines {
        let expired = engine
            .sm
            .scan_expired_at_with_conv(eos_nanos, engine.conv_plan.as_ref());

        for close_out in expired {
            if let Ok(Some(alert_record)) = engine.executor.execute_close(&close_out) {
                alerts.push(OracleAlert {
                    rule_name: alert_record.rule_name,
                    score: alert_record.score,
                    entity_type: alert_record.entity_type,
                    entity_id: alert_record.entity_id,
                    origin: alert_record.origin.as_str().to_string(),
                    emit_time: alert_record.fired_at.clone(),
                });
            }
        }
    }

    Ok(OracleResult { alerts })
}

// ---------------------------------------------------------------------------
// Internal types and helpers
// ---------------------------------------------------------------------------

struct RuleEngine {
    sm: CepStateMachine,
    executor: RuleExecutor,
    conv_plan: Option<ConvPlan>,
    /// window_name → Vec<bind_alias> for routing events to all matching aliases
    alias_map: HashMap<String, Vec<String>>,
}

/// Build a mapping from window name to ALL bind aliases for a rule.
fn build_window_alias_map(plan: &RulePlan) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for bind in &plan.binds {
        map.entry(bind.window.clone())
            .or_default()
            .push(bind.alias.clone());
    }
    map
}

/// Convert a GenEvent to a wf_core Event.
fn gen_event_to_core(event: &GenEvent) -> Event {
    let mut fields = HashMap::new();
    for (k, v) in &event.fields {
        if let Some(core_v) = json_to_core_value(v) {
            fields.insert(k.clone(), core_v);
        }
    }
    Event { fields }
}

fn json_to_core_value(v: &serde_json::Value) -> Option<Value> {
    match v {
        serde_json::Value::String(s) => Some(Value::Str(s.clone())),
        serde_json::Value::Number(n) => n.as_f64().map(Value::Number),
        serde_json::Value::Bool(b) => Some(Value::Bool(*b)),
        _ => None,
    }
}

/// Tolerance settings extracted from the oracle block params.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OracleTolerances {
    /// Time tolerance for verify matching (default 1s).
    pub time_tolerance_secs: f64,
    /// Score tolerance for verify matching (default 0.01).
    pub score_tolerance: f64,
}

impl Default for OracleTolerances {
    fn default() -> Self {
        Self {
            time_tolerance_secs: 1.0,
            score_tolerance: 0.01,
        }
    }
}

/// Extract tolerance parameters from the parsed oracle block.
pub fn extract_oracle_tolerances(oracle: &crate::wfg_ast::OracleBlock) -> OracleTolerances {
    let mut tolerances = OracleTolerances::default();
    for param in &oracle.params {
        match param.name.as_str() {
            "time_tolerance" => {
                if let crate::wfg_ast::ParamValue::Duration(d) = &param.value {
                    tolerances.time_tolerance_secs = d.as_secs_f64();
                }
            }
            "score_tolerance" => {
                if let crate::wfg_ast::ParamValue::Number(n) = &param.value {
                    tolerances.score_tolerance = *n;
                }
            }
            _ => {}
        }
    }
    tolerances
}
