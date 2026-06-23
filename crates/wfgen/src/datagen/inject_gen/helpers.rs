use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rand::rngs::StdRng;
use wf_lang::{BaseType, FieldType, WindowSchema};

use super::structures::{InjectOverrides, InjectUseStepOverrides, StepInfo};
use crate::datagen::field_gen::generate_field_value;
use crate::datagen::stream_gen::GenEvent;
use crate::error::{self, WfgenReason, WfgenResult};
use crate::wfg_ast::StreamBlock;

#[derive(Clone)]
pub(super) struct UseStepPlan {
    pub(super) rule_step_idx: usize,
    pub(super) count: u64,
    pub(super) predicates: HashMap<String, serde_json::Value>,
}

/// Compute the time window bounds for cluster generation.
///
/// Returns `(window_secs, max_start_offset)` where `max_start_offset` is the
/// latest second at which a cluster can start without exceeding the duration.
pub(super) fn compute_window_bounds(dur_secs: f64, window_dur: Duration) -> (f64, f64) {
    let window_secs = window_dur.as_secs_f64();
    let max_start_offset = (dur_secs - window_secs).max(0.0);
    (window_secs, max_start_offset)
}

/// Compute per-step event counts for near-miss clusters.
///
/// With ordered `use(...)` declarations, the last declared use step is the
/// near-miss boundary. Previous unspecified steps are filled to threshold,
/// the boundary is clamped to `threshold - 1`, and later steps get 0 events.
/// Without `use(...)`, legacy `steps_completed`/last-step behavior applies.
pub(super) fn compute_near_miss_counts(
    steps: &[StepInfo],
    overrides: &InjectOverrides,
) -> WfgenResult<Vec<u64>> {
    if !overrides.use_steps.is_empty() {
        let planned = plan_use_steps(steps, &overrides.use_steps, true)?;
        if !planned.is_empty() {
            let mut counts = vec![0_u64; steps.len()];
            for planned in &planned {
                counts[planned.rule_step_idx] += planned.count;
            }
            let nm_step_idx =
                near_miss_step_idx_from_plan(&planned, steps.len()).unwrap_or(steps.len() - 1);
            for (idx, count) in counts.iter_mut().enumerate().take(nm_step_idx) {
                if *count == 0 {
                    *count = steps[idx].threshold;
                }
            }
            counts[nm_step_idx] =
                counts[nm_step_idx].min(steps[nm_step_idx].threshold.saturating_sub(1));
            for count in counts.iter_mut().skip(nm_step_idx + 1) {
                *count = 0;
            }
            return Ok(counts);
        }
    }

    let effective_threshold_nm = overrides
        .count_per_entity
        .unwrap_or(steps[steps.len() - 1].threshold);

    let steps_completed = overrides.steps_completed.unwrap_or(steps.len() - 1);
    let nm_step_idx = steps_completed.min(steps.len() - 1);

    Ok(steps
        .iter()
        .enumerate()
        .map(|(i, step)| {
            if i > nm_step_idx {
                0
            } else if i == nm_step_idx {
                effective_threshold_nm.saturating_sub(1)
            } else {
                overrides.count_per_entity.unwrap_or(step.threshold)
            }
        })
        .collect())
}

/// Compute the number of clusters based on per-stream event budgets.
pub(super) fn compute_cluster_count(
    percent: f64,
    steps: &[StepInfo],
    stream_totals: &HashMap<String, u64>,
) -> u64 {
    let mut min_clusters = u64::MAX;

    for step in steps {
        let stream_total = *stream_totals.get(&step.scenario_alias).unwrap_or(&0);
        let budget = (stream_total as f64 * percent / 100.0).round() as u64;
        if step.threshold > 0 {
            let clusters = budget.checked_div(step.threshold).unwrap_or(0);
            min_clusters = min_clusters.min(clusters);
        }
    }

    if min_clusters == u64::MAX {
        0
    } else {
        min_clusters
    }
}

pub(super) fn compute_cluster_count_for_step_counts(
    percent: f64,
    steps: &[StepInfo],
    step_event_counts: &[u64],
    stream_totals: &HashMap<String, u64>,
) -> u64 {
    let mut per_stream_events: HashMap<&str, u64> = HashMap::new();
    for (step, count) in steps.iter().zip(step_event_counts.iter().copied()) {
        *per_stream_events
            .entry(step.scenario_alias.as_str())
            .or_insert(0) += count;
    }

    let mut min_clusters = u64::MAX;
    for (stream, events_per_cluster) in per_stream_events {
        if events_per_cluster == 0 {
            continue;
        }
        let stream_total = *stream_totals.get(stream).unwrap_or(&0);
        let budget = (stream_total as f64 * percent / 100.0).round() as u64;
        min_clusters = min_clusters.min(budget.checked_div(events_per_cluster).unwrap_or(0));
    }

    if min_clusters == u64::MAX {
        0
    } else {
        min_clusters
    }
}

pub(super) fn compute_repeat_count_for_step_counts(
    percent: f64,
    steps: &[StepInfo],
    step_event_counts: &[u64],
    stream_totals: &HashMap<String, u64>,
) -> u64 {
    compute_cluster_count_for_step_counts(percent, steps, step_event_counts, stream_totals)
}

pub(super) fn compute_hit_counts(
    steps: &[StepInfo],
    overrides: &InjectOverrides,
) -> WfgenResult<Vec<u64>> {
    if overrides.use_steps.is_empty() {
        return Ok(steps.iter().map(|step| step.threshold).collect());
    }

    let mut counts = compute_use_step_counts(steps, &overrides.use_steps)?;
    for (count, step) in counts.iter_mut().zip(steps) {
        if *count == 0 {
            *count = step.threshold;
        }
    }

    Ok(counts)
}

pub(super) fn compute_use_step_counts(
    steps: &[StepInfo],
    use_steps: &[InjectUseStepOverrides],
) -> WfgenResult<Vec<u64>> {
    compute_use_step_counts_with_filter_validation(steps, use_steps, true)
}

pub(super) fn plan_use_steps_allowing_filter_conflicts(
    steps: &[StepInfo],
    use_steps: &[InjectUseStepOverrides],
) -> WfgenResult<Vec<UseStepPlan>> {
    plan_use_steps(steps, use_steps, false)
}

fn compute_use_step_counts_with_filter_validation(
    steps: &[StepInfo],
    use_steps: &[InjectUseStepOverrides],
    validate_filter_conflicts: bool,
) -> WfgenResult<Vec<u64>> {
    if steps.is_empty() {
        return Ok(Vec::new());
    }

    let mut counts = vec![0_u64; steps.len()];
    for planned in plan_use_steps(steps, use_steps, validate_filter_conflicts)? {
        counts[planned.rule_step_idx] += planned.count;
    }

    Ok(counts)
}

fn plan_use_steps(
    steps: &[StepInfo],
    use_steps: &[InjectUseStepOverrides],
    validate_filter_conflicts: bool,
) -> WfgenResult<Vec<UseStepPlan>> {
    if steps.is_empty() {
        return Ok(Vec::new());
    }

    let mut planned = Vec::new();
    for (step_idx, use_step) in use_steps.iter().enumerate() {
        if step_idx >= steps.len() {
            return error::fail(
                WfgenReason::Validation,
                format!(
                    "injection use step {} exceeds rule step count {}; each use(...) maps to one rule step",
                    step_idx + 1,
                    steps.len()
                ),
            );
        }
        if use_step.count == 0 {
            return error::fail(
                WfgenReason::Validation,
                format!(
                    "injection use step {} count must be greater than 0",
                    step_idx + 1
                ),
            );
        }
        if validate_filter_conflicts {
            validate_use_step_predicates(step_idx, use_step, &steps[step_idx])?;
        }
        planned.push(UseStepPlan {
            rule_step_idx: step_idx,
            count: use_step.count,
            predicates: use_step.predicates.clone(),
        });
    }

    Ok(planned)
}

fn validate_use_step_predicates(
    step_idx: usize,
    use_step: &InjectUseStepOverrides,
    step: &StepInfo,
) -> WfgenResult<()> {
    for (field, expected) in &step.filter_overrides {
        let Some(actual) = use_step.predicates.get(field) else {
            continue;
        };
        if actual != expected {
            return error::fail(
                WfgenReason::Validation,
                format!(
                    "injection use step {} field '{}' conflicts with rule step filter: use has {}, rule requires {}",
                    step_idx + 1,
                    field,
                    actual,
                    expected
                ),
            );
        }
    }
    Ok(())
}

fn near_miss_step_idx_from_plan(planned: &[UseStepPlan], steps_len: usize) -> Option<usize> {
    planned
        .iter()
        .map(|planned| planned.rule_step_idx)
        .max()
        .map(|idx| idx.min(steps_len.saturating_sub(1)))
}

/// Generate cluster events across all steps.
#[allow(clippy::too_many_arguments)]
pub(super) fn generate_cluster_events(
    steps: &[StepInfo],
    step_event_counts: &[u64],
    key_overrides: &HashMap<String, serde_json::Value>,
    use_step_overrides: &[InjectUseStepOverrides],
    cluster_start_secs: f64,
    window_secs: f64,
    schemas: &[WindowSchema],
    scenario_streams: &[StreamBlock],
    start: &DateTime<Utc>,
    rng: &mut StdRng,
    out: &mut Vec<GenEvent>,
) -> WfgenResult<()> {
    generate_cluster_events_with_filter_validation(
        steps,
        step_event_counts,
        key_overrides,
        use_step_overrides,
        cluster_start_secs,
        window_secs,
        schemas,
        scenario_streams,
        start,
        rng,
        out,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn generate_cluster_events_with_filter_validation(
    steps: &[StepInfo],
    step_event_counts: &[u64],
    key_overrides: &HashMap<String, serde_json::Value>,
    use_step_overrides: &[InjectUseStepOverrides],
    cluster_start_secs: f64,
    window_secs: f64,
    schemas: &[WindowSchema],
    scenario_streams: &[StreamBlock],
    start: &DateTime<Utc>,
    rng: &mut StdRng,
    out: &mut Vec<GenEvent>,
    validate_filter_conflicts: bool,
) -> WfgenResult<()> {
    let step_predicate_overrides = map_use_predicates_to_rule_steps(
        steps,
        use_step_overrides,
        step_event_counts,
        validate_filter_conflicts,
    )?;

    // Track cumulative time offset across steps for multi-step ordering
    let mut cumulative_offset = 0.0;
    let per_step_window = if steps.len() > 1 {
        window_secs / steps.len() as f64
    } else {
        window_secs
    };

    for (step_idx, step) in steps.iter().enumerate() {
        let event_count = step_event_counts.get(step_idx).copied().unwrap_or(0);
        if event_count == 0 {
            continue;
        }

        let schema = schemas
            .iter()
            .find(|s| s.name == step.window_name)
            .ok_or_else(|| {
                error::error(
                    WfgenReason::Validation,
                    format!("schema not found for '{}'", step.window_name),
                )
            })?;

        let stream_block = scenario_streams
            .iter()
            .find(|s| s.alias == step.scenario_alias)
            .unwrap();

        let overrides_map: HashMap<&str, &crate::wfg_ast::GenExpr> = stream_block
            .overrides
            .iter()
            .map(|o| (o.field_name.as_str(), &o.gen_expr))
            .collect();

        let empty_predicates: HashMap<String, serde_json::Value> = HashMap::new();
        let step_event_predicates = step_predicate_overrides.get(step_idx);

        for i in 0..event_count {
            let event_offset_secs = cluster_start_secs
                + cumulative_offset
                + (per_step_window * i as f64 / event_count.max(1) as f64);
            let ts = *start + ChronoDuration::nanoseconds((event_offset_secs * 1e9) as i64);
            let per_event_predicates = step_event_predicates
                .and_then(|v| v.get(i as usize))
                .unwrap_or(&empty_predicates);

            let fields = build_event_fields_with_predicates(
                schema,
                &overrides_map,
                key_overrides,
                &step.filter_overrides,
                per_event_predicates,
                &ts,
                rng,
            );

            // Use the actual stream name from schema (e.g., "syslog")
            let stream_name = schema
                .streams
                .first()
                .cloned()
                .unwrap_or_else(|| schema.name.clone());

            out.push(GenEvent {
                stream_name,
                window_name: step.window_name.clone(),
                timestamp: ts,
                fields,
            });
        }

        cumulative_offset += per_step_window;
    }

    Ok(())
}

fn map_use_predicates_to_rule_steps(
    steps: &[StepInfo],
    use_steps: &[InjectUseStepOverrides],
    step_event_counts: &[u64],
    validate_filter_conflicts: bool,
) -> WfgenResult<Vec<Vec<HashMap<String, serde_json::Value>>>> {
    let mut per_rule_step = vec![Vec::new(); step_event_counts.len()];
    if use_steps.is_empty() || step_event_counts.is_empty() {
        return Ok(per_rule_step);
    }

    for planned in plan_use_steps(steps, use_steps, validate_filter_conflicts)? {
        if let Some(step_predicates) = per_rule_step.get_mut(planned.rule_step_idx) {
            let expected = step_event_counts
                .get(planned.rule_step_idx)
                .copied()
                .unwrap_or(0) as usize;
            let remaining_slots = expected.saturating_sub(step_predicates.len());
            for _ in 0..remaining_slots.min(planned.count as usize) {
                step_predicates.push(planned.predicates.clone());
            }
        }
    }

    // Fill missing event slots with empty predicates.
    for (idx, expected) in step_event_counts.iter().copied().enumerate() {
        let step_predicates = &mut per_rule_step[idx];
        while step_predicates.len() < expected as usize {
            step_predicates.push(HashMap::new());
        }
    }

    Ok(per_rule_step)
}

/// Build event fields with key, filter, and predicate overrides applied.
pub(super) fn build_event_fields_with_predicates(
    schema: &WindowSchema,
    overrides_map: &HashMap<&str, &crate::wfg_ast::GenExpr>,
    key_overrides: &HashMap<String, serde_json::Value>,
    filter_overrides: &HashMap<String, serde_json::Value>,
    predicate_overrides: &HashMap<String, serde_json::Value>,
    ts: &DateTime<Utc>,
    rng: &mut StdRng,
) -> serde_json::Map<String, serde_json::Value> {
    let mut fields = serde_json::Map::new();

    for field_def in &schema.fields {
        // 1. Key field override (highest priority)
        if let Some(value) = key_overrides.get(&field_def.name) {
            fields.insert(field_def.name.clone(), value.clone());
            continue;
        }

        // 2. Predicate override from use(...) (second priority)
        if let Some(value) = predicate_overrides.get(&field_def.name) {
            fields.insert(field_def.name.clone(), value.clone());
            continue;
        }

        // 3. Filter override (bind filter constraints)
        if let Some(value) = filter_overrides.get(&field_def.name) {
            fields.insert(field_def.name.clone(), value.clone());
            continue;
        }

        // 4. Time field
        if matches!(&field_def.field_type, FieldType::Base(BaseType::Time)) {
            let override_expr = overrides_map.get(field_def.name.as_str()).copied();
            if override_expr.is_none()
                || matches!(override_expr, Some(crate::wfg_ast::GenExpr::GenFunc { name, .. }) if name == "timestamp")
            {
                fields.insert(
                    field_def.name.clone(),
                    serde_json::json!(ts.timestamp_nanos_opt().unwrap_or(0)),
                );
                continue;
            }
        }

        // 5. Normal field with possible stream override
        let override_expr = overrides_map.get(field_def.name.as_str()).copied();
        let value = generate_field_value(&field_def.field_type, override_expr, rng);
        fields.insert(field_def.name.clone(), value);
    }

    fields
}

/// Build event fields with key and filter overrides applied.
#[allow(dead_code)]
pub(super) fn build_event_fields(
    schema: &WindowSchema,
    overrides_map: &HashMap<&str, &crate::wfg_ast::GenExpr>,
    key_overrides: &HashMap<String, serde_json::Value>,
    filter_overrides: &HashMap<String, serde_json::Value>,
    ts: &DateTime<Utc>,
    rng: &mut StdRng,
) -> serde_json::Map<String, serde_json::Value> {
    build_event_fields_with_predicates(
        schema,
        overrides_map,
        key_overrides,
        filter_overrides,
        &HashMap::new(),
        ts,
        rng,
    )
}

/// Generate unique key values for a cluster entity.
///
/// Uses the entity counter and a prefix to produce deterministic unique values
/// based on the field type from the schema.
pub(super) fn generate_key_values(
    key_names: &[String],
    entity_counter: u64,
    prefix: &str,
    schemas: &[WindowSchema],
    steps: &[StepInfo],
    entity_field: Option<&str>,
) -> HashMap<String, serde_json::Value> {
    let mut overrides = HashMap::new();

    // Find field types from the first step's schema
    let first_schema = steps
        .first()
        .and_then(|s| schemas.iter().find(|sch| sch.name == s.window_name));

    let mut names = key_names.to_vec();
    if let Some(field) = entity_field
        && !names.iter().any(|name| name == field)
    {
        names.push(field.to_string());
    }

    for (i, key_name) in names.iter().enumerate() {
        let field_type = first_schema.and_then(|sch| {
            sch.fields
                .iter()
                .find(|f| &f.name == key_name)
                .map(|f| &f.field_type)
        });

        let value = match field_type {
            Some(FieldType::Base(BaseType::Ip)) => {
                let id = entity_counter + i as u64;
                let a = ((id >> 16) & 0xFF) as u8;
                let b = ((id >> 8) & 0xFF) as u8;
                let c = (id & 0xFF) as u8;
                serde_json::Value::String(format!("10.{a}.{b}.{c}"))
            }
            Some(FieldType::Base(BaseType::Digit)) => {
                serde_json::json!(entity_counter as i64 + i as i64)
            }
            Some(FieldType::Base(BaseType::Float)) => {
                serde_json::json!(entity_counter as f64 + i as f64)
            }
            _ => {
                // Default: string
                serde_json::Value::String(format!(
                    "{prefix}_{key}_{id:06}",
                    key = key_name,
                    id = entity_counter
                ))
            }
        };

        overrides.insert(key_name.clone(), value);
    }

    overrides
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use wf_lang::ast::Measure;

    use super::*;

    #[test]
    fn matched_use_predicates_are_capped_to_step_event_count() {
        let steps = vec![StepInfo {
            bind_alias: "auth_fail".to_string(),
            scenario_alias: "LoginWindow".to_string(),
            window_name: "LoginWindow".to_string(),
            measure: Measure::Count,
            threshold: 5,
            filter_overrides: HashMap::from([(
                "success".to_string(),
                serde_json::Value::Bool(false),
            )]),
        }];
        let use_steps = vec![InjectUseStepOverrides {
            count: 1_000,
            predicates: HashMap::from([("success".to_string(), serde_json::Value::Bool(false))]),
        }];

        let mapped = map_use_predicates_to_rule_steps(&steps, &use_steps, &[4], true).unwrap();

        assert_eq!(mapped.len(), 1);
        assert_eq!(
            mapped[0].len(),
            4,
            "matched filter predicates must not allocate beyond generated event count"
        );
        assert!(
            mapped[0].iter().all(
                |predicates| predicates.get("success") == Some(&serde_json::Value::Bool(false))
            )
        );
    }

    #[test]
    fn use_step_counts_return_empty_for_empty_steps() {
        let use_steps = vec![InjectUseStepOverrides {
            count: 1,
            predicates: HashMap::from([("success".to_string(), serde_json::Value::Bool(false))]),
        }];

        let counts = compute_use_step_counts(&[], &use_steps).unwrap();

        assert!(counts.is_empty());
    }

    #[test]
    fn planned_use_steps_bind_by_declaration_order() {
        let steps = vec![
            StepInfo {
                bind_alias: "auth_fail".to_string(),
                scenario_alias: "LoginWindow".to_string(),
                window_name: "LoginWindow".to_string(),
                measure: Measure::Count,
                threshold: 1,
                filter_overrides: HashMap::from([(
                    "success".to_string(),
                    serde_json::Value::Bool(false),
                )]),
            },
            StepInfo {
                bind_alias: "followup".to_string(),
                scenario_alias: "LoginWindow".to_string(),
                window_name: "LoginWindow".to_string(),
                measure: Measure::Count,
                threshold: 1,
                filter_overrides: HashMap::new(),
            },
        ];
        let use_steps = vec![
            InjectUseStepOverrides {
                count: 1,
                predicates: HashMap::from([(
                    "success".to_string(),
                    serde_json::Value::Bool(false),
                )]),
            },
            InjectUseStepOverrides {
                count: 1,
                predicates: HashMap::from([("dport".to_string(), serde_json::json!(22))]),
            },
        ];

        let counts = compute_use_step_counts(&steps, &use_steps).unwrap();
        let mapped = map_use_predicates_to_rule_steps(&steps, &use_steps, &[1, 1], true).unwrap();

        assert_eq!(counts, vec![1, 1]);
        assert_eq!(
            mapped[0][0].get("success"),
            Some(&serde_json::Value::Bool(false))
        );
        assert_eq!(mapped[1][0].get("dport"), Some(&serde_json::json!(22)));
    }

    #[test]
    fn one_use_step_does_not_spill_across_rule_steps() {
        let steps = vec![
            StepInfo {
                bind_alias: "first".to_string(),
                scenario_alias: "LoginWindow".to_string(),
                window_name: "LoginWindow".to_string(),
                measure: Measure::Count,
                threshold: 1,
                filter_overrides: HashMap::new(),
            },
            StepInfo {
                bind_alias: "second".to_string(),
                scenario_alias: "LoginWindow".to_string(),
                window_name: "LoginWindow".to_string(),
                measure: Measure::Count,
                threshold: 1,
                filter_overrides: HashMap::new(),
            },
        ];
        let use_steps = vec![InjectUseStepOverrides {
            count: 2,
            predicates: HashMap::from([("dport".to_string(), serde_json::json!(22))]),
        }];

        let counts = compute_use_step_counts(&steps, &use_steps).unwrap();
        let mapped = map_use_predicates_to_rule_steps(&steps, &use_steps, &[2, 1], true).unwrap();

        assert_eq!(counts, vec![2, 0]);
        assert_eq!(mapped[0].len(), 2);
        assert!(
            mapped[1][0].is_empty(),
            "one use(...) clause must not spill predicates into the next rule step"
        );
    }

    #[test]
    fn extra_use_step_errors_when_rule_steps_exhausted() {
        let steps = vec![StepInfo {
            bind_alias: "auth_fail".to_string(),
            scenario_alias: "LoginWindow".to_string(),
            window_name: "LoginWindow".to_string(),
            measure: Measure::Count,
            threshold: 5,
            filter_overrides: HashMap::from([(
                "success".to_string(),
                serde_json::Value::Bool(false),
            )]),
        }];
        let use_steps = vec![
            InjectUseStepOverrides {
                count: 5,
                predicates: HashMap::from([(
                    "success".to_string(),
                    serde_json::Value::Bool(false),
                )]),
            },
            InjectUseStepOverrides {
                count: 1,
                predicates: HashMap::from([("success".to_string(), serde_json::Value::Bool(true))]),
            },
        ];

        let err = compute_use_step_counts(&steps, &use_steps).unwrap_err();
        let rendered = err.report().render().to_string();

        assert!(
            rendered.contains("exceeds rule step count"),
            "unexpected error: {rendered}"
        );
    }

    #[test]
    fn zero_count_use_step_errors() {
        let steps = vec![StepInfo {
            bind_alias: "auth_fail".to_string(),
            scenario_alias: "LoginWindow".to_string(),
            window_name: "LoginWindow".to_string(),
            measure: Measure::Count,
            threshold: 5,
            filter_overrides: HashMap::new(),
        }];
        let use_steps = vec![InjectUseStepOverrides {
            count: 0,
            predicates: HashMap::new(),
        }];

        let err = compute_use_step_counts(&steps, &use_steps).unwrap_err();
        let rendered = err.report().render().to_string();

        assert!(
            rendered.contains("count must be greater than 0"),
            "unexpected error: {rendered}"
        );
    }

    #[test]
    fn conflicting_use_step_predicates_error() {
        let steps = vec![StepInfo {
            bind_alias: "auth_fail".to_string(),
            scenario_alias: "LoginWindow".to_string(),
            window_name: "LoginWindow".to_string(),
            measure: Measure::Count,
            threshold: 5,
            filter_overrides: HashMap::from([(
                "success".to_string(),
                serde_json::Value::Bool(false),
            )]),
        }];
        let use_steps = vec![InjectUseStepOverrides {
            count: 5,
            predicates: HashMap::from([("success".to_string(), serde_json::Value::Bool(true))]),
        }];

        let err = compute_use_step_counts(&steps, &use_steps).unwrap_err();
        let rendered = err.report().render().to_string();

        assert!(
            rendered.contains("conflicts with rule step filter"),
            "unexpected error: {rendered}"
        );
    }

    #[test]
    fn near_miss_use_steps_drop_events_after_near_miss_step() {
        let steps = vec![
            StepInfo {
                bind_alias: "step0".to_string(),
                scenario_alias: "LoginWindow".to_string(),
                window_name: "LoginWindow".to_string(),
                measure: Measure::Count,
                threshold: 1,
                filter_overrides: HashMap::from([(
                    "stage".to_string(),
                    serde_json::json!("first"),
                )]),
            },
            StepInfo {
                bind_alias: "step1".to_string(),
                scenario_alias: "LoginWindow".to_string(),
                window_name: "LoginWindow".to_string(),
                measure: Measure::Count,
                threshold: 2,
                filter_overrides: HashMap::new(),
            },
            StepInfo {
                bind_alias: "step2".to_string(),
                scenario_alias: "LoginWindow".to_string(),
                window_name: "LoginWindow".to_string(),
                measure: Measure::Count,
                threshold: 1,
                filter_overrides: HashMap::from([(
                    "stage".to_string(),
                    serde_json::json!("after"),
                )]),
            },
        ];
        let overrides = InjectOverrides {
            entity_field: None,
            count_per_entity: None,
            steps_completed: Some(1),
            within: None,
            use_steps: vec![
                InjectUseStepOverrides {
                    count: 1,
                    predicates: HashMap::from([("stage".to_string(), serde_json::json!("first"))]),
                },
                InjectUseStepOverrides {
                    count: 1,
                    predicates: HashMap::from([("stage".to_string(), serde_json::json!("after"))]),
                },
            ],
        };

        let counts = compute_near_miss_counts(&steps, &overrides).unwrap();

        assert_eq!(
            counts,
            vec![1, 1, 0],
            "near_miss boundary should come from the planned step, not raw use count"
        );
    }
}
