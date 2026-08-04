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
    out: Option<PathBuf>,
    ws: Vec<PathBuf>,
    wfl: Vec<PathBuf>,
    no_wfl: bool,
    no_oracle: bool,
    send: bool,
    addr: String,
) -> WfgenResult<()> {
    // At least one sink must be requested: write files via --out, stream via
    // --send, or both. Having neither is a usage error, not a silent no-op.
    if out.is_none() && !send {
        return error::fail(
            WfgenReason::Validation,
            "no output target: specify --out to write files, --send to stream over TCP, or both",
        );
    }

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

    // `--no-wfl` and `--no-oracle` both skip the entire WFL pipeline: no rule
    // loading, no `_global.wfl` / yield-preset evaluation, no compilation, no
    // oracle / expected output. Generation falls back to baseline events.
    let skip_wfl = no_wfl || no_oracle;

    let (mut schemas, mut wfl_files) = load_from_uses(&wfg, &scenario, &HashMap::new(), skip_wfl)?;
    schemas.extend(load_ws_files(&ws)?);
    if !skip_wfl {
        wfl_files.extend(load_wfl_files(&wfl)?);
    }

    let errors = validate_wfg(&wfg, &schemas, &wfl_files, skip_wfl);
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

    // Expected output is requested by either:
    // - legacy oracle block, or
    // - new syntax expect block.
    // If requested, WFL compile failures must be fatal.
    let expect_requested = wfg
        .syntax
        .as_ref()
        .and_then(|s| s.expect.as_ref())
        .is_some();
    // `--no-oracle` (like `--no-wfl`) skips the entire WFL pipeline, so oracle
    // / expected output is disabled too. `expected_requested` stays false and
    // `rule_plans` stays empty.
    let expected_requested = (wfg.scenario.oracle.is_some() || expect_requested) && !skip_wfl;

    // Compile WFL rules. Skipped entirely by `--no-wfl` and `--no-oracle`: no
    // compilation, no `_global.wfl` / yield-preset evaluation, no injection-aware
    // generation, no oracle. `rule_plans` stays empty, so generation falls back
    // to baseline background events.
    let mut rule_plans = Vec::new();
    if !skip_wfl {
        let mut compile_errors = Vec::new();
        for wfl_file in &wfl_files {
            match wf_lang::compile_wfl(wfl_file, &schemas) {
                Ok(plans) => rule_plans.extend(plans),
                Err(e) => compile_errors.push(e),
            }
        }

        if !compile_errors.is_empty() {
            if expected_requested {
                for e in &compile_errors {
                    eprintln!("Error: WFL compilation failed: {}", e.report().render());
                }
                return error::fail(
                    WfgenReason::Validation,
                    "WFL compilation failed while expected output is enabled; \
                     fix the WFL errors or use --no-oracle / --no-wfl to skip expected output",
                );
            } else {
                for e in &compile_errors {
                    eprintln!("Warning: WFL compilation failed: {}", e.report().render());
                }
            }
        }
    }

    // Generate clean events
    let result = generate(&wfg, &schemas, &rule_plans)?;

    // Expected alert generation (on CLEAN events, before faults).
    let expected_enabled = expected_requested && !rule_plans.is_empty();
    // Oracle/expected output was requested, not opted out (--no-wfl /
    // --no-oracle) but there is nowhere to write it (--send only, no --out).
    // Warn rather than silently drop it.
    if expected_requested && out.is_none() {
        eprintln!(
            "Warning: oracle/expected output requested but --out not set; \
             skipping expected generation"
        );
    }
    if expected_enabled && let Some(out) = out.as_ref() {
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
    if expected_enabled
        && has_faults
        && let Some(out) = out.as_ref()
    {
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
    if let Some(out) = out.as_ref() {
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

#[cfg(test)]
mod tests {
    use super::*;

    // The arg-validation check is the very first thing `run` does, before any
    // file IO, so a nonexistent scenario path is fine here — the function must
    // return the usage error without touching the filesystem.
    #[tokio::test]
    async fn run_requires_out_or_send() {
        let err = run(
            PathBuf::from("nonexistent.wfg"),
            "jsonl".to_string(),
            None,       // out
            Vec::new(), // ws
            Vec::new(), // wfl
            false,      // no_wfl
            false,      // no_oracle
            false,      // send
            "127.0.0.1:1".to_string(),
        )
        .await;
        let err = err.unwrap_err();
        let msg = err.report().render();
        assert!(
            msg.contains("no output target"),
            "expected 'no output target' error, got: {}",
            msg
        );
    }
}
