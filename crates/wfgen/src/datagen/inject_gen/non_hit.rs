use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rand::rngs::StdRng;
use wf_lang::WindowSchema;

use super::helpers::{
    build_event_fields, build_event_fields_with_predicates, compute_repeat_count_for_step_counts,
    generate_key_values, plan_use_steps_allowing_filter_conflicts,
};
use super::structures::{InjectOverrides, RuleStructure};
use crate::datagen::stream_gen::GenEvent;
use crate::error::{self, WfgenReason, WfgenResult};
use crate::wfg_ast::StreamBlock;

#[allow(clippy::too_many_arguments)]
pub(super) fn generate_non_hit_events(
    percent: f64,
    rule_struct: &RuleStructure,
    stream_totals: &HashMap<String, u64>,
    schemas: &[WindowSchema],
    scenario_streams: &[StreamBlock],
    start: &DateTime<Utc>,
    duration: &Duration,
    rng: &mut StdRng,
    inject_counts: &mut HashMap<String, u64>,
    overrides: &InjectOverrides,
) -> WfgenResult<Vec<GenEvent>> {
    if !overrides.use_steps.is_empty() {
        return generate_non_hit_use_step_events(
            percent,
            rule_struct,
            stream_totals,
            schemas,
            scenario_streams,
            start,
            duration,
            rng,
            inject_counts,
            overrides,
        );
    }

    let mut events = Vec::new();
    let dur_nanos = duration.as_nanos() as i64;

    // For non-hit, each event has a unique key -> no clustering -> no rule trigger.
    // Generate events on each participating stream.
    let mut entity_counter: u64 = 1_000_000; // offset to avoid collision with hit/nm

    for step in &rule_struct.steps {
        let stream_total = *stream_totals.get(&step.scenario_alias).unwrap_or(&0);
        let event_count = (stream_total as f64 * percent / 100.0).round() as u64;

        if event_count == 0 {
            continue;
        }

        *inject_counts
            .entry(step.scenario_alias.clone())
            .or_insert(0) += event_count;

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

        for i in 0..event_count {
            let key_overrides = generate_key_values(
                &rule_struct.keys,
                entity_counter,
                "bg",
                schemas,
                &rule_struct.steps,
                None,
            );
            entity_counter += 1;

            // Uniform timestamp distribution
            let offset_nanos = if event_count > 1 {
                dur_nanos * i as i64 / event_count as i64
            } else {
                dur_nanos / 2
            };
            let ts = *start + ChronoDuration::nanoseconds(offset_nanos);

            let fields = build_event_fields(
                schema,
                &overrides_map,
                &key_overrides,
                &step.filter_overrides,
                &ts,
                rng,
            );

            // Use the actual stream name from schema (e.g., "syslog")
            let stream_name = schema
                .streams
                .first()
                .cloned()
                .unwrap_or_else(|| schema.name.clone());

            events.push(GenEvent {
                stream_name,
                window_name: step.window_name.clone(),
                timestamp: ts,
                fields,
            });
        }
    }

    Ok(events)
}

#[allow(clippy::too_many_arguments)]
fn generate_non_hit_use_step_events(
    percent: f64,
    rule_struct: &RuleStructure,
    stream_totals: &HashMap<String, u64>,
    schemas: &[WindowSchema],
    scenario_streams: &[StreamBlock],
    start: &DateTime<Utc>,
    duration: &Duration,
    rng: &mut StdRng,
    inject_counts: &mut HashMap<String, u64>,
    overrides: &InjectOverrides,
) -> WfgenResult<Vec<GenEvent>> {
    let steps = &rule_struct.steps;
    if steps.is_empty() {
        return Ok(Vec::new());
    }

    let planned_use_steps = plan_use_steps_allowing_filter_conflicts(steps, &overrides.use_steps)?;
    let mut step_event_counts = vec![0_u64; steps.len()];
    let mut step_predicates = vec![None; steps.len()];
    for planned in planned_use_steps {
        step_event_counts[planned.rule_step_idx] += planned.count;
        step_predicates[planned.rule_step_idx] = Some(planned.predicates);
    }
    if step_event_counts.iter().all(|count| *count == 0) {
        return Ok(Vec::new());
    }

    let shape_repeats =
        compute_repeat_count_for_step_counts(percent, steps, &step_event_counts, stream_totals);
    if shape_repeats == 0 {
        return Ok(Vec::new());
    }

    for (step, event_count) in steps.iter().zip(step_event_counts.iter().copied()) {
        *inject_counts
            .entry(step.scenario_alias.clone())
            .or_insert(0) += event_count * shape_repeats;
    }

    let dur_nanos = duration.as_nanos() as i64;

    let mut events = Vec::new();
    let mut entity_counter = 1_000_000_u64;
    let mut event_index = 0_i64;
    let total_events = (shape_repeats * step_event_counts.iter().sum::<u64>()).max(1) as i64;

    for _repeat_idx in 0..shape_repeats {
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

            let predicates = step_predicates[step_idx].as_ref().ok_or_else(|| {
                error::error(
                    WfgenReason::Validation,
                    format!(
                        "injection use step {} is missing predicates for planned miss events",
                        step_idx + 1
                    ),
                )
            })?;

            for _ in 0..event_count {
                let key_overrides = generate_key_values(
                    &rule_struct.keys,
                    entity_counter,
                    "miss",
                    schemas,
                    steps,
                    overrides.entity_field.as_deref(),
                );
                entity_counter += 1;

                let offset_nanos = if total_events > 1 {
                    dur_nanos * event_index / total_events
                } else {
                    dur_nanos / 2
                };
                event_index += 1;
                let ts = *start + ChronoDuration::nanoseconds(offset_nanos);

                let fields = build_event_fields_with_predicates(
                    schema,
                    &overrides_map,
                    &key_overrides,
                    &step.filter_overrides,
                    predicates,
                    &ts,
                    rng,
                );

                let stream_name = schema
                    .streams
                    .first()
                    .cloned()
                    .unwrap_or_else(|| schema.name.clone());

                events.push(GenEvent {
                    stream_name,
                    window_name: step.window_name.clone(),
                    timestamp: ts,
                    fields,
                });
            }
        }
    }

    Ok(events)
}
