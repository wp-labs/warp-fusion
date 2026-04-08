use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rand::rngs::StdRng;
use wf_lang::{BaseType, FieldType, WindowSchema};

use super::structures::{InjectOverrides, InjectUseStepOverrides, StepInfo};
use crate::datagen::field_gen::generate_field_value;
use crate::datagen::stream_gen::GenEvent;
use crate::wfg_ast::StreamBlock;

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
/// For the near-miss step (determined by `steps_completed` override or the
/// last step by default): `threshold - 1` events. Steps before it get the
/// full threshold. Steps after it get 0 events.
pub(super) fn compute_near_miss_counts(
    steps: &[StepInfo],
    overrides: &InjectOverrides,
) -> Vec<u64> {
    let effective_threshold_nm = overrides
        .count_per_entity
        .unwrap_or(steps[steps.len() - 1].threshold);

    let steps_completed = overrides.steps_completed.unwrap_or(steps.len() - 1);
    let nm_step_idx = steps_completed.min(steps.len() - 1);

    steps
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
        .collect()
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
            let clusters = budget / step.threshold;
            min_clusters = min_clusters.min(clusters);
        }
    }

    if min_clusters == u64::MAX {
        0
    } else {
        min_clusters
    }
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
) -> anyhow::Result<()> {
    let step_predicate_overrides =
        map_use_predicates_to_rule_steps(use_step_overrides, step_event_counts);

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
            .ok_or_else(|| anyhow::anyhow!("schema not found for '{}'", step.window_name))?;

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
    use_steps: &[InjectUseStepOverrides],
    step_event_counts: &[u64],
) -> Vec<Vec<HashMap<String, serde_json::Value>>> {
    let mut per_rule_step = vec![Vec::new(); step_event_counts.len()];
    if use_steps.is_empty() || step_event_counts.is_empty() {
        return per_rule_step;
    }

    let mut rule_step_idx = 0usize;
    let mut remaining_in_step = step_event_counts[0];
    let mut overflow = false;

    for use_step in use_steps {
        let mut remaining_in_use = use_step.count;
        while remaining_in_use > 0 && rule_step_idx < per_rule_step.len() {
            if remaining_in_step == 0 {
                rule_step_idx += 1;
                if rule_step_idx >= per_rule_step.len() {
                    break;
                }
                remaining_in_step = step_event_counts[rule_step_idx];
                continue;
            }

            let assign = remaining_in_use.min(remaining_in_step);
            for _ in 0..assign {
                per_rule_step[rule_step_idx].push(use_step.predicates.clone());
            }
            remaining_in_use -= assign;
            remaining_in_step -= assign;
        }
        if remaining_in_use > 0 {
            overflow = true;
            break;
        }
        if rule_step_idx >= per_rule_step.len() {
            break;
        }
    }

    // Fill missing event slots with empty predicates.
    for (idx, expected) in step_event_counts.iter().copied().enumerate() {
        let step_predicates = &mut per_rule_step[idx];
        while step_predicates.len() < expected as usize {
            step_predicates.push(HashMap::new());
        }
    }

    // Backward-compatible fallback: if syntax `use` steps exceed rule steps,
    // keep legacy behavior by applying merged predicates globally.
    if overflow {
        let mut merged = HashMap::new();
        for use_step in use_steps {
            for (k, v) in &use_step.predicates {
                merged.insert(k.clone(), v.clone());
            }
        }
        for step_predicates in &mut per_rule_step {
            for event_predicates in step_predicates {
                for (k, v) in &merged {
                    event_predicates.insert(k.clone(), v.clone());
                }
            }
        }
    }

    per_rule_step
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
) -> HashMap<String, serde_json::Value> {
    let mut overrides = HashMap::new();

    // Find field types from the first step's schema
    let first_schema = steps
        .first()
        .and_then(|s| schemas.iter().find(|sch| sch.name == s.window_name));

    for (i, key_name) in key_names.iter().enumerate() {
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
