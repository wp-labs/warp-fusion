use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rand::rngs::StdRng;
use wf_lang::WindowSchema;
use wf_lang::plan::RulePlan;

use super::extract::extract_inject_overrides;
use super::hit::generate_hit_clusters;
use super::near_miss::generate_near_miss_clusters;
use super::non_hit::generate_non_hit_events;
use super::structures::{AliasMap, RuleStructure};
use crate::datagen::stream_gen::GenEvent;
use crate::wfg_ast::{InjectLine, InjectMode, StreamBlock};

pub(super) fn compute_stream_totals(
    scenario: &crate::wfg_ast::ScenarioDecl,
) -> HashMap<String, u64> {
    let total = scenario.total;
    let total_rate: f64 = scenario
        .streams
        .iter()
        .map(|s| s.rate.events_per_second())
        .sum();

    if total_rate == 0.0 {
        return HashMap::new();
    }

    let mut result = HashMap::new();
    let mut remaining = total;

    for (i, stream) in scenario.streams.iter().enumerate() {
        let proportion = stream.rate.events_per_second() / total_rate;
        let count = if i == scenario.streams.len() - 1 {
            remaining
        } else {
            let c = (total as f64 * proportion).round() as u64;
            let c = c.min(remaining);
            remaining -= c;
            c
        };
        result.insert(stream.alias.clone(), count);
    }

    result
}

pub(super) fn build_alias_map(
    inject_streams: &[String],
    scenario_streams: &[StreamBlock],
    rule_plan: &RulePlan,
) -> anyhow::Result<AliasMap> {
    let mut bind_to_scenario = HashMap::new();

    // New syntax uses stream names without exposing bind aliases.
    // We first resolve scenario stream by name, then map to a rule bind by:
    // 1) exact alias match, 2) unique bind on the same window.
    for stream_name in inject_streams {
        let stream_block = scenario_streams
            .iter()
            .find(|s| &s.alias == stream_name)
            .ok_or_else(|| {
                anyhow::anyhow!("inject stream '{}' not found in scenario", stream_name)
            })?;

        let bind_alias = if rule_plan.binds.iter().any(|b| b.alias == *stream_name) {
            stream_name.clone()
        } else {
            let mut bind_iter = rule_plan
                .binds
                .iter()
                .filter(|b| b.window == stream_block.window)
                .map(|b| b.alias.clone());
            let first = bind_iter.next().ok_or_else(|| {
                anyhow::anyhow!(
                    "inject stream '{}' cannot be mapped to any bind in rule '{}'",
                    stream_name,
                    rule_plan.name
                )
            })?;
            if bind_iter.next().is_some() {
                anyhow::bail!(
                    "inject stream '{}' maps to multiple binds in rule '{}'; \
                     use explicit bind aliases in .wfg",
                    stream_name,
                    rule_plan.name
                );
            }
            first
        };

        bind_to_scenario.insert(
            bind_alias,
            (stream_block.alias.clone(), stream_block.window.clone()),
        );
    }

    Ok(AliasMap { bind_to_scenario })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn generate_for_line(
    inject_line: &InjectLine,
    rule_struct: &RuleStructure,
    stream_totals: &HashMap<String, u64>,
    schemas: &[WindowSchema],
    scenario_streams: &[StreamBlock],
    start: &DateTime<Utc>,
    duration: &Duration,
    rng: &mut StdRng,
    inject_counts: &mut HashMap<String, u64>,
) -> anyhow::Result<Vec<GenEvent>> {
    let overrides = extract_inject_overrides(inject_line);

    match inject_line.mode {
        InjectMode::Hit => generate_hit_clusters(
            inject_line.percent,
            rule_struct,
            stream_totals,
            schemas,
            scenario_streams,
            start,
            duration,
            rng,
            inject_counts,
            &overrides,
        ),
        InjectMode::NearMiss => generate_near_miss_clusters(
            inject_line.percent,
            rule_struct,
            stream_totals,
            schemas,
            scenario_streams,
            start,
            duration,
            rng,
            inject_counts,
            &overrides,
        ),
        InjectMode::NonHit => generate_non_hit_events(
            inject_line.percent,
            rule_struct,
            stream_totals,
            schemas,
            scenario_streams,
            start,
            duration,
            rng,
            inject_counts,
        ),
    }
}
