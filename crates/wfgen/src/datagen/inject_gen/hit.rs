use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rand::Rng;
use rand::rngs::StdRng;
use wf_lang::WindowSchema;

use super::helpers::{
    compute_cluster_count, compute_window_bounds, generate_cluster_events, generate_key_values,
};
use super::structures::{InjectOverrides, RuleStructure, StepInfo};
use crate::datagen::stream_gen::GenEvent;
use crate::wfg_ast::StreamBlock;

#[allow(clippy::too_many_arguments)]
pub(super) fn generate_hit_clusters(
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
    // Apply count_per_entity override: use overridden threshold for cluster sizing
    let effective_steps: Vec<StepInfo> = if let Some(cpe) = overrides.count_per_entity {
        rule_struct
            .steps
            .iter()
            .map(|s| StepInfo {
                bind_alias: s.bind_alias.clone(),
                scenario_alias: s.scenario_alias.clone(),
                window_name: s.window_name.clone(),
                measure: s.measure,
                threshold: cpe, // override
                filter_overrides: s.filter_overrides.clone(),
            })
            .collect()
    } else {
        rule_struct.steps.clone()
    };

    let num_clusters = compute_cluster_count(percent, &effective_steps, stream_totals);
    if num_clusters == 0 {
        return Ok(Vec::new());
    }

    // Update inject counts
    for step in &effective_steps {
        *inject_counts
            .entry(step.scenario_alias.clone())
            .or_insert(0) += step.threshold * num_clusters;
    }

    let dur_secs = duration.as_secs_f64();
    let window_dur = overrides.within.unwrap_or(rule_struct.window_dur);
    let (window_secs, max_start_offset) = compute_window_bounds(dur_secs, window_dur);

    let mut events = Vec::new();

    for (entity_counter, _cluster_idx) in (0_u64..).zip(0..num_clusters) {
        let key_overrides = generate_key_values(
            &rule_struct.keys,
            entity_counter,
            "hit",
            schemas,
            &effective_steps,
        );

        let cluster_start_secs = if max_start_offset > 0.0 {
            rng.random_range(0.0..max_start_offset)
        } else {
            0.0
        };
        let step_event_counts: Vec<u64> = effective_steps.iter().map(|s| s.threshold).collect();

        generate_cluster_events(
            &effective_steps,
            &step_event_counts,
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
