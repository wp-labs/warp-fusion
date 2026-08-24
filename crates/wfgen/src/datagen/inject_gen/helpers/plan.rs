use std::collections::HashMap;
use std::time::Duration;

use crate::datagen::inject_gen::structures::{InjectOverrides, InjectUseStepOverrides, StepInfo};
use crate::error::{self, WfgenReason, WfgenResult};


pub(crate) struct UseStepPlan {
    pub(crate) rule_step_idx: usize,
    pub(crate) count: u64,
    pub(crate) predicates: HashMap<String, serde_json::Value>,
}

/// Compute the time window bounds for cluster generation.
///
/// Returns `(window_secs, max_start_offset)` where `max_start_offset` is the
/// latest second at which a cluster can start without exceeding the duration.
pub(crate) fn compute_window_bounds(dur_secs: f64, window_dur: Duration) -> (f64, f64) {
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
pub(crate) fn compute_near_miss_counts(
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
pub(crate) fn compute_cluster_count(
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

pub(crate) fn compute_cluster_count_for_step_counts(
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

pub(crate) fn compute_repeat_count_for_step_counts(
    percent: f64,
    steps: &[StepInfo],
    step_event_counts: &[u64],
    stream_totals: &HashMap<String, u64>,
) -> u64 {
    compute_cluster_count_for_step_counts(percent, steps, step_event_counts, stream_totals)
}

pub(crate) fn compute_hit_counts(
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

pub(crate) fn compute_use_step_counts(
    steps: &[StepInfo],
    use_steps: &[InjectUseStepOverrides],
) -> WfgenResult<Vec<u64>> {
    compute_use_step_counts_with_filter_validation(steps, use_steps, true)
}

pub(crate) fn plan_use_steps_allowing_filter_conflicts(
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

pub(crate) fn plan_use_steps(
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
