use std::path::PathBuf;

use orion_error::conversion::SourceErr;

use wfgen::error::{WfgenReason, WfgenResult};
use wfgen::oracle::OracleTolerances;
use wfgen::output::jsonl::{read_alerts_jsonl, read_oracle_jsonl};
use wfgen::verify::verify;

pub(crate) fn run(
    expected: PathBuf,
    actual: PathBuf,
    score_tolerance: Option<f64>,
    time_tolerance: Option<f64>,
    meta: Option<PathBuf>,
    format: String,
) -> WfgenResult<()> {
    // Load tolerances: CLI flags > meta file > defaults
    let base_tolerances = if let Some(meta_path) = &meta {
        let content = std::fs::read_to_string(meta_path).source_err(
            WfgenReason::Io,
            format!("reading meta: {}", meta_path.display()),
        )?;
        serde_json::from_str::<OracleTolerances>(&content).source_err(
            WfgenReason::Serialization,
            format!("parsing meta: {}", meta_path.display()),
        )?
    } else {
        OracleTolerances::default()
    };

    let effective_score_tol = score_tolerance.unwrap_or(base_tolerances.score_tolerance);
    let effective_time_tol = time_tolerance.unwrap_or(base_tolerances.time_tolerance_secs);

    let oracle_alerts = read_oracle_jsonl(&expected)?;
    let actual_alerts = read_alerts_jsonl(&actual)?;

    let report = verify(
        &oracle_alerts,
        &actual_alerts,
        effective_score_tol,
        effective_time_tol,
    );

    match format.as_str() {
        "markdown" | "md" => {
            println!("{}", report.to_markdown());
        }
        _ => {
            let json = serde_json::to_string_pretty(&report)
                .source_err(WfgenReason::Serialization, "serializing verify report")?;
            println!("{}", json);
        }
    }

    if report.status == "pass" {
        std::process::exit(0);
    } else {
        std::process::exit(1);
    }
}
