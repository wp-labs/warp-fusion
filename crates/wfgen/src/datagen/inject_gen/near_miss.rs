use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rand::Rng;
use rand::rngs::StdRng;
use wf_lang::WindowSchema;

use super::helpers::{
    compute_near_miss_counts, compute_window_bounds, generate_cluster_events, generate_key_values,
};
use super::structures::{InjectOverrides, RuleStructure};
use crate::datagen::stream_gen::GenEvent;
use crate::wfg_ast::StreamBlock;

#[allow(clippy::too_many_arguments)]
pub(super) fn generate_near_miss_clusters(
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
) -> anyhow::Result<Vec<GenEvent>> {
    let steps = &rule_struct.steps;
    if steps.is_empty() {
        return Ok(Vec::new());
    }

    let near_miss_counts = compute_near_miss_counts(steps, overrides);

    // Total events per cluster
    let events_per_cluster: u64 = near_miss_counts.iter().sum();
    if events_per_cluster == 0 {
        return Ok(Vec::new());
    }

    // Compute number of clusters from the near-miss step's budget
    let nm_step_idx = overrides
        .steps_completed
        .unwrap_or(steps.len() - 1)
        .min(steps.len() - 1);
    let primary_step = &steps[nm_step_idx];
    let stream_total = *stream_totals
        .get(&primary_step.scenario_alias)
        .unwrap_or(&0);
    let budget = (stream_total as f64 * percent / 100.0).round() as u64;
    let nm_count = near_miss_counts[nm_step_idx];
    let num_clusters = if nm_count > 0 { budget / nm_count } else { 0 };

    if num_clusters == 0 {
        return Ok(Vec::new());
    }

    // Update inject counts
    for (i, step) in steps.iter().enumerate() {
        *inject_counts
            .entry(step.scenario_alias.clone())
            .or_insert(0) += near_miss_counts[i] * num_clusters;
    }

    let dur_secs = duration.as_secs_f64();
    let window_dur = overrides.within.unwrap_or(rule_struct.window_dur);
    let (window_secs, max_start_offset) = compute_window_bounds(dur_secs, window_dur);

    let mut events = Vec::new();

    for (entity_counter, _cluster_idx) in (0_u64..).zip(0..num_clusters) {
        let key_overrides = generate_key_values(
            &rule_struct.keys,
            entity_counter,
            "nm",
            schemas,
            &rule_struct.steps,
        );

        let cluster_start_secs = if max_start_offset > 0.0 {
            rng.random_range(0.0..max_start_offset)
        } else {
            0.0
        };

        generate_cluster_events(
            steps,
            &near_miss_counts,
            &key_overrides,
            &overrides.use_steps,
            cluster_start_secs,
            window_secs,
            schemas,
            scenario_streams,
            start,
            rng,
            &mut events,
        )?;
    }

    Ok(events)
}
