use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rand::rngs::StdRng;
use wf_lang::WindowSchema;

use super::helpers::{build_event_fields, generate_key_values};
use super::structures::RuleStructure;
use crate::datagen::stream_gen::GenEvent;
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
) -> anyhow::Result<Vec<GenEvent>> {
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

        for i in 0..event_count {
            let key_overrides = generate_key_values(
                &rule_struct.keys,
                entity_counter,
                "bg",
                schemas,
                &rule_struct.steps,
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
