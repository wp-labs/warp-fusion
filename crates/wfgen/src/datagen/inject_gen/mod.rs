mod dispatch;
mod extract;
mod helpers;
mod hit;
mod near_miss;
mod non_hit;
mod structures;

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rand::rngs::StdRng;
use std::time::Duration;
use wf_lang::WindowSchema;
use wf_lang::plan::RulePlan;

use crate::error::{self, WfgenReason, WfgenResult};
use crate::injection_targets::{injected_rule_names, unique_expected_rule};
use crate::wfg_ast::WfgFile;

use dispatch::{build_alias_map, build_alias_map_for_syntax_case, compute_stream_totals};
use extract::extract_rule_structure;
pub use structures::InjectGenResult;

/// Generate inject events driven by rule plans.
///
/// For each inject block in the scenario, generates hit / near-miss / non-hit
/// event clusters according to the rule's structure and thresholds.
pub fn generate_inject_events(
    wfg: &WfgFile,
    rule_plans: &[RulePlan],
    schemas: &[WindowSchema],
    start: &DateTime<Utc>,
    duration: &Duration,
    rng: &mut StdRng,
) -> WfgenResult<InjectGenResult> {
    let scenario = &wfg.scenario;
    let stream_totals = compute_stream_totals(scenario);

    let mut all_events = Vec::new();
    let mut inject_counts: HashMap<String, u64> = HashMap::new();

    if let Some(injection) = wfg
        .syntax
        .as_ref()
        .and_then(|syntax| syntax.injection.as_ref())
    {
        let _ = injected_rule_names(wfg)?;
        let default_rule = wfg
            .syntax
            .as_ref()
            .and_then(|syntax| syntax.expect.as_ref())
            .and_then(unique_expected_rule)
            .unwrap_or_default();

        for case in &injection.cases {
            let rule_plan = resolve_rule_plan(
                case.target_rule.as_deref().unwrap_or(default_rule),
                rule_plans,
            )?;
            let alias_map = build_alias_map_for_syntax_case(case, &scenario.streams, rule_plan)?;
            let rule_struct = extract_rule_structure(rule_plan, &alias_map)?;
            let events = dispatch::generate_for_syntax_case(
                case,
                &rule_struct,
                &stream_totals,
                schemas,
                &scenario.streams,
                start,
                duration,
                rng,
                &mut inject_counts,
            )?;
            all_events.extend(events);
        }

        return Ok(InjectGenResult {
            events: all_events,
            inject_counts,
        });
    }

    for inject_block in &scenario.injects {
        let rule_plan = resolve_rule_plan(&inject_block.rule, rule_plans)?;

        let alias_map = build_alias_map(&inject_block.streams, &scenario.streams, rule_plan)?;
        let rule_struct = extract_rule_structure(rule_plan, &alias_map)?;

        for inject_line in &inject_block.lines {
            let events = dispatch::generate_for_line(
                inject_line,
                &rule_struct,
                &stream_totals,
                schemas,
                &scenario.streams,
                start,
                duration,
                rng,
                &mut inject_counts,
            )?;
            all_events.extend(events);
        }
    }

    Ok(InjectGenResult {
        events: all_events,
        inject_counts,
    })
}

fn resolve_rule_plan(
    inject_rule: impl AsRef<str>,
    rule_plans: &[RulePlan],
) -> WfgenResult<&RulePlan> {
    let inject_rule = inject_rule.as_ref();
    if inject_rule.is_empty() {
        if rule_plans.len() == 1 {
            return Ok(&rule_plans[0]);
        }
        return error::fail(
            WfgenReason::Validation,
            format!(
                "injection target rule is ambiguous: expect(...) is missing and {} rules are loaded",
                rule_plans.len()
            ),
        );
    }

    rule_plans
        .iter()
        .find(|p| p.name == inject_rule)
        .ok_or_else(|| {
            error::error(
                WfgenReason::Validation,
                format!(
                    "inject references rule '{}' not found in compiled plans",
                    inject_rule
                ),
            )
        })
}
