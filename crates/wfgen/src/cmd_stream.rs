//! Continuous data generation — daemon mode.
//!
//! Loads multiple `.wfg` scenarios, cycles through them indefinitely,
//! and sends events via a persistent TCP connection.
//!
//! The scenario is generated in fixed *event-time slices*: each batch spans a
//! short slice of event time (default 1s) and the slice's start advances
//! monotonically between batches, so windows progress correctly on a long run
//! (the previous implementation regenerated the whole scenario over the same
//! fixed 60s window every time). The send loop paces each slice to span the
//! same wall-clock duration as its event-time span, i.e. data flows at the
//! scenario's declared rate (or an explicit `--rate` override). Batch size is
//! `rate × slice` and is capped so wfgen's memory stays bounded.
//!
//! Usage:
//!   wpgen stream --scenario-dir scenarios/ --ws schemas/*.wfs --wfl rules/*.wfl \
//!     --addr 127.0.0.1:9800 --rate 2000000 --slice-ms 1000

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{DateTime, SecondsFormat, Utc};
use orion_error::conversion::SourceErr;

use crate::datagen::generate;
use crate::error::{self, WfgenReason, WfgenResult};
use crate::loader::load_from_uses;
use crate::wfg_ast::WfgFile;
use crate::wfg_parser::parse_wfg;
use crate::{
    cmd_helpers::{load_wfl_files, load_ws_files},
    tcp_send::connect_sender,
};

use wf_lang::WindowSchema;

/// Events per send chunk. Larger chunks amortize the Arrow batch-encode
/// overhead (events_to_typed_batches builds builders per call); 40k ≈ the
/// per-frame row cap, so a chunk typically becomes 1-2 frames.
const CHUNK_SIZE: usize = 40_000;
/// Upper bound on one generated batch, so wfgen memory stays bounded even at
/// very high rates (batch = rate × slice, capped here).
const MAX_BATCH: u64 = 2_000_000;

/// A loaded scenario ready for continuous generation.
struct LoadedScenario {
    name: String,
    wfg: WfgFile,
    rule_plans: Vec<wf_lang::plan::RulePlan>,
}

pub async fn run(
    scenario_dir: PathBuf,
    ws: Vec<PathBuf>,
    wfl: Vec<PathBuf>,
    addr: String,
    interval_secs: u64,
    rate_eps_override: u64,
    slice_ms: u64,
) -> WfgenResult<()> {
    // 1. Load schemas
    let mut schemas: Vec<WindowSchema> = Vec::new();
    schemas.extend(load_ws_files(&ws)?);

    // 2. Compile WFL rules (for inject_gen hit/near_miss/miss)
    let wfl_files_loaded = load_wfl_files(&wfl)?;
    let mut all_rule_plans = Vec::new();
    for wfl_file in &wfl_files_loaded {
        match wf_lang::compile_wfl(wfl_file, &schemas) {
            Ok(plans) => all_rule_plans.extend(plans),
            Err(e) => {
                eprintln!("Warning: WFL compilation failed for {:?}: {}", wfl_file, e);
            }
        }
    }

    // 3. Load all .wfg scenarios from directory
    let scenarios = load_scenarios(&scenario_dir, &schemas, &all_rule_plans)?;
    if scenarios.is_empty() {
        return error::fail(WfgenReason::Io, "no .wfg scenarios found in directory");
    }

    eprintln!(
        "Loaded {} scenarios from {}",
        scenarios.len(),
        scenario_dir.display()
    );
    eprintln!(
        "Rate: override={} | slice={}ms | scenario interval={}s",
        rate_eps_override, slice_ms, interval_secs
    );
    eprintln!("Target: {}", addr);

    // 4. Connect to wfusion TCP via wp_core_connectors NetWriter (async)
    let mut writer = connect_sender(&addr).await?;
    eprintln!("Connected to {}", addr);

    // 5. Cycle through scenarios forever
    let scenario_dur = Duration::from_secs(interval_secs);
    let mut idx = 0usize;
    let mut total_events: u64 = 0;
    let mut total_frames: u64 = 0;
    let wall_start = Instant::now();

    loop {
        let scenario = &scenarios[idx];

        // Effective target rate: explicit --rate wins, else scenario declared rate.
        let base_rate: f64 = scenario
            .wfg
            .scenario
            .streams
            .iter()
            .map(|s| s.rate.events_per_second())
            .sum();
        if base_rate <= 0.0 && rate_eps_override == 0 {
            return error::fail(
                WfgenReason::Validation,
                format!(
                    "scenario '{}' declares no stream rate and no --rate override given",
                    scenario.name
                ),
            );
        }
        let rate: f64 = if rate_eps_override > 0 {
            rate_eps_override as f64
        } else {
            base_rate
        };

        let base_start: DateTime<Utc> =
            scenario
                .wfg
                .scenario
                .time_clause
                .start
                .parse()
                .map_err(|e| {
                    error::error(
                        WfgenReason::Generation,
                        format!(
                            "invalid scenario start '{}': {}",
                            scenario.wfg.scenario.time_clause.start, e
                        ),
                    )
                })?;
        let base_seed = scenario.wfg.scenario.seed;

        let phase_start = Instant::now();
        let mut phase_events: u64 = 0;
        let mut phase_frames: u64 = 0;
        let mut cursor_nanos: i128 = 0; // accumulated event-time offset

        eprintln!(
            "[{}] phase=start scenario={} (idx {}/{}) rate={:.0}/s",
            chrono::Local::now().format("%H:%M:%S"),
            scenario.name,
            idx,
            scenarios.len(),
            rate
        );

        while phase_start.elapsed() < scenario_dur {
            // Batch = rate × slice, bounded to keep wfgen memory in check.
            let batch_total =
                ((rate * slice_ms as f64 / 1000.0).round() as u64).clamp(1, MAX_BATCH);
            // Actual event-time span for this batch (seconds).
            let slice_secs = batch_total as f64 / rate;
            let slice_nanos = (slice_secs * 1e9).max(1.0) as u64;

            // Build a modified scenario: advancing start, slice duration, bounded
            // total, and a per-slice seed so field values differ across batches.
            let mut wfg = scenario.wfg.clone();
            wfg.scenario.seed = base_seed.wrapping_add(cursor_nanos as u64);
            wfg.scenario.time_clause.start = (base_start
                + chrono::Duration::nanoseconds(cursor_nanos as i64))
            .to_rfc3339_opts(SecondsFormat::Nanos, true);
            wfg.scenario.time_clause.duration = Duration::from_nanos(slice_nanos);
            wfg.scenario.total = batch_total;

            // Start the slice timer before generate() so generation time is counted
            // inside the slice budget — the send pacing below then makes each slice
            // span ~slice_secs of wall time end to end (generate + send).
            let batch_start = Instant::now();
            let result = generate(&wfg, &schemas, &scenario.rule_plans)?;
            let event_count = result.events.len() as u64;

            let mut gen_frames = 0u64;
            let num_chunks = result.events.len().div_ceil(CHUNK_SIZE).max(1);
            for (i, chunk) in result.events.chunks(CHUNK_SIZE).enumerate() {
                let sent =
                    crate::tcp_send::send_events_with_stream(chunk, &schemas, &mut writer).await?;
                gen_frames += sent as u64;

                // Pace each chunk so the whole slice spans ~slice_secs of wall
                // time (i.e. event-time rate ≈ real-time). Under daemon
                // backpressure the send itself takes longer and this sleep is
                // skipped automatically — the stream then flows at the daemon's
                // consumption rate instead of flooding it.
                let ideal = batch_start
                    + Duration::from_secs_f64(slice_secs * (i as f64 + 1.0) / num_chunks as f64);
                let now = Instant::now();
                if now < ideal {
                    tokio::time::sleep(ideal - now).await;
                }
            }

            cursor_nanos += slice_nanos as i128;
            total_events += event_count;
            total_frames += gen_frames;
            phase_events += event_count;
            phase_frames += gen_frames;
        }

        let elapsed = wall_start.elapsed().as_secs_f64();
        let eps = if elapsed > 0.0 {
            total_events as f64 / elapsed
        } else {
            0.0
        };
        eprintln!(
            "[{}] phase=done scenario={} events={} frames={} | total_events={} total_frames={} eps={:.0}",
            chrono::Local::now().format("%H:%M:%S"),
            scenario.name,
            phase_events,
            phase_frames,
            total_events,
            total_frames,
            eps
        );

        idx = (idx + 1) % scenarios.len();
    }
}

/// Load all .wfg files from a directory.
fn load_scenarios(
    dir: &Path,
    global_schemas: &[WindowSchema],
    global_rules: &[wf_lang::plan::RulePlan],
) -> WfgenResult<Vec<LoadedScenario>> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .source_err(
            WfgenReason::Io,
            format!("reading scenario dir {}", dir.display()),
        )?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "wfg"))
        .collect();
    entries.sort();

    let mut result = Vec::new();
    for path in entries {
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());

        let content = std::fs::read_to_string(&path)
            .source_err(WfgenReason::Io, format!("reading {}", path.display()))?;
        let wfg = parse_wfg(&content).source_err(
            WfgenReason::Io,
            format!("parse {}: {}", path.display(), "parse error"),
        )?;

        // Load schemas referenced by the scenario's `use` declarations
        let (mut scenario_schemas, _) =
            load_from_uses(&wfg, &path, &std::collections::HashMap::new(), false)?;
        // Merge with global schemas (avoid duplicates by name)
        for s in global_schemas {
            if !scenario_schemas.iter().any(|x| x.name == s.name) {
                scenario_schemas.push(s.clone());
            }
        }

        // Compile any WFL files referenced by the scenario
        let mut rule_plans = Vec::new();
        for plan in global_rules {
            rule_plans.push(plan.clone());
        }

        result.push(LoadedScenario {
            name,
            wfg,
            rule_plans,
        });
    }

    Ok(result)
}
