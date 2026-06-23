use std::collections::HashMap;
use std::path::PathBuf;

use orion_error::conversion::SourceErr;
use rand::SeedableRng;
use rand::rngs::StdRng;

use crate::datagen::fault_gen::apply_faults;
use crate::datagen::generate;
use crate::error::{self, WfgenReason, WfgenResult};
use crate::injection_targets::injected_rule_names;
use crate::loader::load_from_uses;
use crate::oracle::{extract_oracle_tolerances, run_oracle};
use crate::output::arrow_ipc::write_arrow_ipc;
use crate::output::jsonl::{write_jsonl, write_oracle_jsonl};
use crate::validate::validate_wfg;
use crate::wfg_parser::parse_wfg;

use crate::cmd_helpers::{load_wfl_files, load_ws_files};
use crate::tcp_send::send_events;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    scenario: PathBuf,
    format: String,
    out: PathBuf,
    ws: Vec<PathBuf>,
    wfl: Vec<PathBuf>,
    no_oracle: bool,
    send: bool,
    addr: String,
) -> WfgenResult<()> {
    let normalized_format = match format.as_str() {
        "jsonl" => "jsonl",
        "arrow" | "arrow-ipc" | "ipc" => "arrow",
        _ => "",
    };
    if normalized_format.is_empty() {
        return error::fail(
            WfgenReason::Validation,
            format!(
                "unsupported format: '{}'. Supported: 'jsonl', 'arrow' ('arrow-ipc' alias).",
                format
            ),
        );
    }

    let wfg_content = std::fs::read_to_string(&scenario).source_err(
        WfgenReason::Io,
        format!("reading .wfg file: {}", scenario.display()),
    )?;
    let wfg = parse_wfg(&wfg_content)?;
    let output_case = scenario
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| wfg.scenario.name.clone());

    let (mut schemas, mut wfl_files) = load_from_uses(&wfg, &scenario, &HashMap::new())?;
    schemas.extend(load_ws_files(&ws)?);
    wfl_files.extend(load_wfl_files(&wfl)?);

    let errors = validate_wfg(&wfg, &schemas, &wfl_files);
    if !errors.is_empty() {
        eprintln!("Validation errors:");
        for e in &errors {
            eprintln!("  {}", e);
        }
        return error::fail(
            WfgenReason::Validation,
            format!("{} validation error(s) found", errors.len()),
        );
    }

    // Compile WFL rules
    let mut rule_plans = Vec::new();
    let mut compile_errors = Vec::new();
    for wfl_file in &wfl_files {
        match wf_lang::compile_wfl(wfl_file, &schemas) {
            Ok(plans) => rule_plans.extend(plans),
            Err(e) => {
                compile_errors.push(e);
            }
        }
    }

    // Expected output is requested by either:
    // - legacy oracle block, or
    // - new syntax expect block.
    // If requested, WFL compile failures must be fatal.
    let expect_requested = wfg
        .syntax
        .as_ref()
        .and_then(|s| s.expect.as_ref())
        .is_some();
    let expected_requested = (wfg.scenario.oracle.is_some() || expect_requested) && !no_oracle;
    if !compile_errors.is_empty() {
        if expected_requested {
            for e in &compile_errors {
                eprintln!("Error: WFL compilation failed: {}", e.report().render());
            }
            return error::fail(
                WfgenReason::Validation,
                "WFL compilation failed while expected output is enabled; \
                 fix the WFL errors or use --no-oracle",
            );
        } else {
            for e in &compile_errors {
                eprintln!("Warning: WFL compilation failed: {}", e.report().render());
            }
        }
    }

    // Generate clean events
    let result = generate(&wfg, &schemas, &rule_plans)?;

    // Expected alert generation (on CLEAN events, before faults).
    let expected_enabled = expected_requested && !rule_plans.is_empty();
    let mut expected_alert_count = 0;
    if expected_enabled {
        let start = wfg.scenario.time_clause.start.parse().map_err(|e| {
            error::error(
                WfgenReason::Generation,
                format!(
                    "invalid start time '{}': {}",
                    wfg.scenario.time_clause.start, e
                ),
            )
        })?;
        let duration = wfg.scenario.time_clause.duration;

        // SC7: only evaluate rules that have inject coverage
        let injected_rules = injected_rule_names(&wfg)?;

        let expected_result = run_oracle(
            &result.events,
            &rule_plans,
            &start,
            &duration,
            Some(&injected_rules),
        )?;
        expected_alert_count = expected_result.alerts.len();

        let expected_file = out.join(format!("{}.except.jsonl", output_case));
        write_oracle_jsonl(&expected_result.alerts, &expected_file)?;
        println!(
            "Expected: {} alerts -> {}",
            expected_result.alerts.len(),
            expected_file.display()
        );

        // Write tolerances sidecar so `verify` can read them as defaults
        let tolerances = wfg
            .scenario
            .oracle
            .as_ref()
            .map(extract_oracle_tolerances)
            .unwrap_or_default();
        let meta_file = out.join(format!("{}.except.meta.jsonl", output_case));
        let meta_json = serde_json::to_string(&tolerances).source_err(
            WfgenReason::Serialization,
            "serializing oracle tolerance metadata",
        )?;
        std::fs::write(&meta_file, meta_json)
            .source_err(WfgenReason::Io, format!("writing {}", meta_file.display()))?;
        println!("Expected meta -> {}", meta_file.display());
    }
    let _ = expected_alert_count;

    // Apply faults (after oracle, on clean events)
    let has_faults = wfg.scenario.faults.is_some();
    let output_events = if let Some(faults) = &wfg.scenario.faults {
        let mut fault_rng = StdRng::seed_from_u64(wfg.scenario.seed.wrapping_add(1));
        let fault_result = apply_faults(result.events, faults, &mut fault_rng);
        eprintln!("Faults applied: {}", fault_result.stats);
        fault_result.events
    } else {
        result.events
    };

    // Post-fault expected generation (M33 P2): run oracle again on faulted events
    // so verify can compare clean vs faulted outcomes.
    if expected_enabled && has_faults {
        let start = wfg.scenario.time_clause.start.parse().map_err(|e| {
            error::error(
                WfgenReason::Generation,
                format!(
                    "invalid start time '{}': {}",
                    wfg.scenario.time_clause.start, e
                ),
            )
        })?;
        let duration = wfg.scenario.time_clause.duration;

        let injected_rules = injected_rule_names(&wfg)?;

        let faulted_expected = run_oracle(
            &output_events,
            &rule_plans,
            &start,
            &duration,
            Some(&injected_rules),
        )?;

        let faulted_expected_file = out.join(format!("{}.faulted-except.jsonl", output_case));
        write_oracle_jsonl(&faulted_expected.alerts, &faulted_expected_file)?;
        println!(
            "Faulted expected: {} alerts -> {}",
            faulted_expected.alerts.len(),
            faulted_expected_file.display()
        );
    }

    // Write output
    match normalized_format {
        "jsonl" => {
            let output_file = out.join(format!("{}.jsonl", output_case));
            write_jsonl(&output_events, &output_file)?;
            println!(
                "Generated {} events -> {}",
                output_events.len(),
                output_file.display()
            );
        }
        "arrow" => {
            let output_file = out.join(format!("{}.arrow", output_case));
            write_arrow_ipc(&output_events, &output_file)?;
            println!(
                "Generated {} events -> {}",
                output_events.len(),
                output_file.display()
            );
        }
        _ => unreachable!(),
    }

    if send {
        let sent_frames = send_events(&output_events, &schemas, &addr).await?;
        println!(
            "Sent {} events as {} frame(s) -> {}",
            output_events.len(),
            sent_frames,
            addr
        );
    }

    Ok(())
}
