//! End-to-end integration test driven by `wfgen`.
//!
//! Uses the full datagen pipeline: `.wfg` scenario → event generation → oracle
//! prediction → Reactor execution → alert verification.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use chrono::{DateTime, Utc};

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, fmt};
use wf_config::{FusionConfig, RawFusionConfigTree};
use wf_runtime::lifecycle::Reactor;
use wf_runtime::tracing_init::{DomainFormat, FileFields};
use wfgen::verify::ActualAlert;

const ARROW_FRAME_CHUNK_ROWS: usize = 2048;

fn read_alerts_from_sink_dir(
    alert_dir: &std::path::Path,
) -> Result<Vec<ActualAlert>, Box<dyn std::error::Error + Send + Sync>> {
    let mut alert_files = std::fs::read_dir(alert_dir)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect::<Vec<_>>();
    alert_files.sort();

    let mut alerts = Vec::new();
    for path in alert_files {
        alerts.extend(
            wfgen::output::jsonl::read_alerts_jsonl(&path)
                .map_err(|err| err.into_dyn_std().into_boxed())?,
        );
    }

    let mut seen = HashSet::new();
    alerts.retain(|alert| {
        seen.insert(format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
            alert.rule_name,
            alert.score,
            alert.entity_type,
            alert.entity_id,
            alert.origin,
            alert.fired_at
        ))
    });
    Ok(alerts)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_datagen_brute_force() {
    // ---- Artifact directory ----
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let artifact_dir = manifest_dir.join("../../target/test-artifacts/e2e_datagen");
    std::fs::create_dir_all(&artifact_dir).expect("failed to create artifact dir");
    let alert_dir = artifact_dir.join("alerts");
    let source_path = artifact_dir.join("input/events.arrow_framed");
    std::fs::create_dir_all(&alert_dir).expect("failed to create alert dir");
    if let Ok(entries) = std::fs::read_dir(&alert_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "jsonl") {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    if let Some(parent) = source_path.parent() {
        std::fs::create_dir_all(parent).expect("failed to create source dir");
    }
    let _ = std::fs::remove_file(&source_path);

    // ---- Tracing (test_writer + file) ----
    let log_file_name = "e2e_datagen.log";
    let _ = std::fs::remove_file(artifact_dir.join(log_file_name));
    let file_appender = tracing_appender::rolling::never(&artifact_dir, log_file_name);
    let (non_blocking, _log_guard) = tracing_appender::non_blocking(file_appender);
    let _ = tracing_subscriber::registry()
        .with(
            fmt::layer()
                .event_format(DomainFormat::new())
                .with_test_writer()
                .with_filter(EnvFilter::try_new("info").unwrap()),
        )
        .with(
            fmt::layer()
                .event_format(DomainFormat::new())
                .fmt_fields(FileFields::default())
                .with_ansi(false)
                .with_writer(non_blocking)
                .with_filter(EnvFilter::try_new("debug").unwrap()),
        )
        .try_init();

    // ---- Load scenario (.wfg → schemas + rules) ----
    let base_dir = manifest_dir.join("examples");
    let wfg_path = manifest_dir.join("examples/count/scenarios/brute_force.wfg");
    let vars = HashMap::from([("FAIL_THRESHOLD".into(), "3".into())]);
    let loaded = wfgen::loader::load_scenario(&wfg_path, &vars).expect("failed to load scenario");

    // ---- Validate scenario ----
    let validation_errors =
        wfgen::validate::validate_wfg(&loaded.wfg, &loaded.schemas, &loaded.wfl_files);
    assert!(
        validation_errors.is_empty(),
        "scenario validation failed: {:?}",
        validation_errors
    );

    // ---- Generate events ----
    let gen_result = wfgen::datagen::generate(&loaded.wfg, &loaded.schemas, &loaded.rule_plans)
        .expect("event generation failed");
    let events = gen_result.events;
    assert!(
        !events.is_empty(),
        "datagen produced zero events — scenario misconfigured?"
    );

    // ---- Oracle prediction (SC7: only injected rules) ----
    let start: DateTime<Utc> = loaded
        .wfg
        .scenario
        .time_clause
        .start
        .parse()
        .expect("invalid scenario start time");
    let duration = loaded.wfg.scenario.time_clause.duration;

    let injected_rules =
        wfgen::injection_targets::injected_rule_names(&loaded.wfg).expect("injected rules");
    let oracle_result = wfgen::oracle::run_oracle(
        &events,
        &loaded.rule_plans,
        &start,
        &duration,
        Some(&injected_rules),
    )
    .expect("oracle evaluation failed");
    let oracle_alerts = &oracle_result.alerts;
    assert!(
        !oracle_alerts.is_empty(),
        "oracle produced zero alerts; injected_rules={:?}",
        injected_rules
    );
    let tolerances = loaded
        .wfg
        .scenario
        .oracle
        .as_ref()
        .map(wfgen::oracle::extract_oracle_tolerances)
        .unwrap_or_default();

    // ---- Build FusionConfig (inline TOML, file source, connector-based sinks) ----
    let toml_str = format!(
        r#"
mode = "batch"
sinks = "sinks"
work_root = "{}"

[[sources]]
type = "file"
name = "ingress"
path = "{}"
data_format = "arrow_framed"
stream = ""

[runtime]
executor_parallelism = 2
rule_exec_timeout = "30s"
schemas = "count/schemas/*.wfs"
rules   = "count/rules/*.wfl"

[window_defaults]
evict_interval = "30s"
max_window_bytes = "256MB"
max_total_bytes = "2GB"
evict_policy = "time_first"
watermark = "5s"
allowed_lateness = "0s"
late_policy = "drop"

[window.auth_events]
mode = "local"
max_window_bytes = "256MB"
over_cap = "30m"

[window.security_alerts]
mode = "local"
max_window_bytes = "64MB"
over_cap = "1h"

[vars]
FAIL_THRESHOLD = "3"
"#,
        artifact_dir.display(),
        source_path.display()
    );
    let config: FusionConfig = toml_str.parse().expect("failed to parse config TOML");
    // Raw tree is the reload baseline; these e2e tests never reload, so a
    // minimal tree parsed from the same toml is sufficient.
    let raw = RawFusionConfigTree::from_toml_str(&toml_str, &base_dir).expect("parse raw toml");

    // ---- Convert GenEvents → typed Arrow batches → framed Arrow file ----
    let batches = wfgen::output::arrow_ipc::events_to_typed_batches(&events, &loaded.schemas)
        .expect("events_to_typed_batches failed");
    let mut framed = Vec::new();
    for (stream_name, batch) in &batches {
        for offset in (0..batch.num_rows()).step_by(ARROW_FRAME_CHUNK_ROWS) {
            let len = (batch.num_rows() - offset).min(ARROW_FRAME_CHUNK_ROWS);
            let chunk = batch.slice(offset, len);
            let ipc_payload = wp_arrow::ipc::encode_ipc(stream_name, &chunk)
                .unwrap_or_else(|e| panic!("encode_ipc failed for '{stream_name}': {e}"));
            framed.extend_from_slice(&(ipc_payload.len() as u32).to_be_bytes());
            framed.extend_from_slice(&ipc_payload);
        }
    }
    std::fs::write(&source_path, framed)
        .unwrap_or_else(|e| panic!("failed to write source file {}: {e}", source_path.display()));

    // ---- Start engine ----
    let reactor = Reactor::start(config, raw, &base_dir)
        .await
        .expect("Reactor::start failed");

    // ---- Batch mode should auto-exit after replay + drain ----
    tokio::time::timeout(Duration::from_secs(10), reactor.wait())
        .await
        .expect("batch reactor did not auto-exit in time")
        .expect("reactor.wait failed");

    // ---- Read actual alerts from all routed sink outputs ----
    let actual = read_alerts_from_sink_dir(&alert_dir)
        .unwrap_or_else(|e| panic!("failed to read alerts from {}: {e}", alert_dir.display()));
    // In `file + batch` mode, final alerts are emitted during rule-task flush on
    // shutdown. That lifecycle uses `close:flush`, while the oracle models the
    // finite scenario boundary as EOF (`close:eos`). Normalize the origin so the
    // content comparison still validates the generated alerts.
    let mut actual_normalized = actual.clone();
    let mut normalized_flush = 0usize;
    for alert in &mut actual_normalized {
        if alert.origin == "close:flush" {
            alert.origin = "close:eos".to_string();
            normalized_flush += 1;
        }
    }

    // ---- Run verify and write diagnostic report ----
    let report = wfgen::verify::verify(
        oracle_alerts,
        &actual_normalized,
        tolerances.score_tolerance,
        tolerances.time_tolerance_secs,
    );
    let mut report_md = report.to_markdown();
    if normalized_flush > 0 {
        report_md.push_str(&format!(
            "\n### Notes\n\n- Normalized shutdown `close:flush` alerts to `close:eos`: {normalized_flush}\n"
        ));
    }
    let report_path = artifact_dir.join("verify_report.md");
    std::fs::write(&report_path, &report_md)
        .unwrap_or_else(|e| panic!("failed to write report to {}: {e}", report_path.display()));

    // ---- Verify report must pass ----
    assert_eq!(
        report.status, "pass",
        "verify report failed:\n{}",
        report_md
    );

    // Check: all alerts reference the correct rule
    for alert in &actual {
        assert_eq!(
            alert.rule_name, "brute_force_then_scan",
            "unexpected rule_name: {}",
            alert.rule_name
        );
    }
}
