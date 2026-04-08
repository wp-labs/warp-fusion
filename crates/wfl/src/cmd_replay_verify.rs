use std::io::{BufReader, IsTerminal};
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;

use wf_vars::ConfigVarContext;
use wfgen::oracle::OracleTolerances;
use wfgen::output::jsonl::read_oracle_jsonl;
use wfgen::verify::{ActualAlert, verify};

const GREEN: &str = "\x1b[1;32m";
const RED: &str = "\x1b[1;31m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

#[allow(clippy::too_many_arguments)]
pub fn run(
    file: Option<PathBuf>,
    case: Option<String>,
    data_dir: PathBuf,
    schemas: Vec<String>,
    input: Option<PathBuf>,
    vars: Vec<String>,
    expected: Option<PathBuf>,
    score_tolerance: Option<f64>,
    time_tolerance: Option<f64>,
    meta: Option<PathBuf>,
    format: String,
) -> anyhow::Result<()> {
    use wf_config::project::{load_schemas, load_wfl_with_context, parse_vars};

    let resolved = resolve_paths(file, case.as_deref(), &data_dir, input, expected, meta)?;

    let cwd = std::env::current_dir()?;
    let mut var_map = parse_vars(&vars)?;
    var_map
        .entry("WORK_DIR".to_string())
        .or_insert_with(|| cwd.to_string_lossy().to_string());
    let ctx = ConfigVarContext::from_explicit_vars(var_map);
    let color = std::io::stderr().is_terminal();

    let all_schemas = load_schemas(&schemas, &cwd)?;
    let source = load_wfl_with_context(&resolved.file, &ctx, Some(&cwd))?;

    let reader = BufReader::new(
        std::fs::File::open(&resolved.input)
            .map_err(|e| anyhow::anyhow!("failed to open {}: {}", resolved.input.display(), e))?,
    );
    let replay = crate::cmd_replay::replay_events_for_verify(&source, &all_schemas, reader, color)?;

    let actual: Vec<ActualAlert> = replay
        .alerts
        .into_iter()
        .map(|a| ActualAlert {
            rule_name: a.rule_name,
            score: a.score,
            entity_type: a.entity_type,
            entity_id: a.entity_id,
            origin: a.origin.as_str().to_string(),
            fired_at: a.fired_at,
        })
        .collect();

    let expected_alerts = read_oracle_jsonl(&resolved.expected)
        .with_context(|| format!("reading expected: {}", resolved.expected.display()))?;

    let base_tolerances = if let Some(meta_path) = &resolved.meta {
        let content = std::fs::read_to_string(meta_path)
            .with_context(|| format!("reading meta: {}", meta_path.display()))?;
        serde_json::from_str::<OracleTolerances>(&content)
            .with_context(|| format!("parsing meta: {}", meta_path.display()))?
    } else {
        OracleTolerances::default()
    };
    let effective_score_tol = score_tolerance.unwrap_or(base_tolerances.score_tolerance);
    let effective_time_tol = time_tolerance.unwrap_or(base_tolerances.time_tolerance_secs);

    let report = verify(
        &expected_alerts,
        &actual,
        effective_score_tol,
        effective_time_tol,
    );

    eprintln!("---");
    eprintln!(
        "Replay complete: {} events processed, {} matches, {} errors",
        replay.event_count, replay.match_count, replay.error_count
    );
    if color {
        if report.status == "pass" {
            eprintln!(
                "{GREEN}{BOLD}Verify PASS{RESET}: matched={}, missing=0, unexpected=0, field_mismatch=0",
                report.summary.matched
            );
        } else {
            eprintln!(
                "{RED}{BOLD}Verify FAIL{RESET}: matched={}, missing={}, unexpected={}, field_mismatch={}",
                report.summary.matched,
                report.summary.missing,
                report.summary.unexpected,
                report.summary.field_mismatch
            );
        }
    } else {
        eprintln!(
            "Verify {}: matched={}, missing={}, unexpected={}, field_mismatch={}",
            report.status.to_uppercase(),
            report.summary.matched,
            report.summary.missing,
            report.summary.unexpected,
            report.summary.field_mismatch
        );
    }

    match format.as_str() {
        "markdown" | "md" => {
            println!("{}", report.to_markdown());
        }
        _ => {
            let json = serde_json::to_string_pretty(&report)?;
            println!("{}", json);
        }
    }

    if report.status == "pass" {
        std::process::exit(0);
    } else {
        std::process::exit(1);
    }
}

struct ResolvedPaths {
    file: PathBuf,
    input: PathBuf,
    expected: PathBuf,
    meta: Option<PathBuf>,
}

fn resolve_paths(
    file: Option<PathBuf>,
    case: Option<&str>,
    data_dir: &Path,
    input: Option<PathBuf>,
    expected: Option<PathBuf>,
    meta: Option<PathBuf>,
) -> anyhow::Result<ResolvedPaths> {
    let resolved_file = resolve_rule_file(file, case)?;

    if let Some(case_name) = case {
        let input_path = input.unwrap_or_else(|| data_dir.join(format!("{case_name}.jsonl")));
        let expected_path =
            expected.unwrap_or_else(|| data_dir.join(format!("{case_name}.except.jsonl")));
        let meta_path = match meta {
            Some(path) => Some(path),
            None => {
                let auto = data_dir.join(format!("{case_name}.except.meta.jsonl"));
                auto.exists().then_some(auto)
            }
        };

        return Ok(ResolvedPaths {
            file: resolved_file,
            input: input_path,
            expected: expected_path,
            meta: meta_path,
        });
    }

    let input_path = input.ok_or_else(|| {
        anyhow::anyhow!(
            "missing --input. Provide --input/--expected, or use --case <name> [--data-dir <dir>]."
        )
    })?;
    let expected_path = expected.ok_or_else(|| {
        anyhow::anyhow!(
            "missing --expected. Provide --input/--expected, or use --case <name> [--data-dir <dir>]."
        )
    })?;

    Ok(ResolvedPaths {
        file: resolved_file,
        input: input_path,
        expected: expected_path,
        meta,
    })
}

fn resolve_rule_file(file: Option<PathBuf>, case: Option<&str>) -> anyhow::Result<PathBuf> {
    if let Some(path) = file {
        return Ok(path);
    }

    let case_name = case.ok_or_else(|| {
        anyhow::anyhow!("missing rule file. Provide <file>, or use --case so rule can be auto-resolved from rules/<case>.wfl.")
    })?;

    // 1) Exact: rules/<case>.wfl
    let direct = PathBuf::from("rules").join(format!("{case_name}.wfl"));
    if direct.exists() {
        return Ok(direct);
    }

    // 2) Progressive trim by '_' for names like brute_force_detect -> brute_force.wfl
    let mut parts: Vec<&str> = case_name.split('_').collect();
    while parts.len() > 1 {
        parts.pop();
        let candidate = PathBuf::from("rules").join(format!("{}.wfl", parts.join("_")));
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    anyhow::bail!(
        "failed to auto-resolve rule file for case `{}`. Tried rules/<case>.wfl and trimmed variants. Please pass the rule file explicitly.",
        case_name
    );
}
