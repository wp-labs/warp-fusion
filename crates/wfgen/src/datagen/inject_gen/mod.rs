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

use crate::wfg_ast::WfgFile;

use dispatch::{build_alias_map, compute_stream_totals};
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
) -> anyhow::Result<InjectGenResult> {
    let scenario = &wfg.scenario;
    let stream_totals = compute_stream_totals(scenario);

    let mut all_events = Vec::new();
    let mut inject_counts: HashMap<String, u64> = HashMap::new();

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

fn resolve_rule_plan<'a>(
    inject_rule: &str,
    rule_plans: &'a [RulePlan],
) -> anyhow::Result<&'a RulePlan> {
    if inject_rule.is_empty() {
        if rule_plans.len() == 1 {
            return Ok(&rule_plans[0]);
        }
        anyhow::bail!(
            "injection target rule is ambiguous: expect(...) is missing and {} rules are loaded",
            rule_plans.len()
        );
    }

    rule_plans
        .iter()
        .find(|p| p.name == inject_rule)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "inject references rule '{}' not found in compiled plans",
                inject_rule
            )
        })
}
