//! Continuous data generation — daemon mode.
//!
//! Loads multiple `.wfg` scenarios, cycles through them indefinitely,
//! and sends events via persistent TCP connection.
//!
//! Usage:
//!   wpgen stream --scenario-dir scenarios/ --ws schemas/*.wfs --addr 127.0.0.1:9800

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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

/// A loaded scenario ready for continuous generation.
struct LoadedScenario {
    name: String,
    wfg: WfgFile,
    rule_plans: Vec<wf_lang::plan::RulePlan>,
}

pub fn run(
    scenario_dir: PathBuf,
    ws: Vec<PathBuf>,
    wfl: Vec<PathBuf>,
    addr: String,
    interval_secs: u64,
    rate_sleep_ms: u64,
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
                eprintln!("Warning: WFL compilation failed: {:?}", wfl_file);
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
        "Rate: sleep {}ms between batches | Scenario interval: {}s",
        rate_sleep_ms, interval_secs
    );
    eprintln!("Target: {}", addr);

    // 4. Connect to wparse TCP (persistent)
    let mut stream = connect_sender(&addr)?;
    eprintln!("Connected to {}", addr);

    // 5. Cycle through scenarios forever
    let sleep_dur = Duration::from_millis(rate_sleep_ms);
    let scenario_dur = Duration::from_secs(interval_secs);
    let mut idx = 0usize;
    let mut total_events: u64 = 0;
    let mut total_frames: u64 = 0;
    let wall_start = Instant::now();

    loop {
        let scenario = &scenarios[idx];
        let phase_start = Instant::now();
        let mut phase_events = 0u64;
        let mut phase_frames = 0u64;

        eprintln!(
            "[{}] phase={} scenario={} (idx {}/{})",
            chrono::Local::now().format("%H:%M:%S"),
            "start",
            scenario.name,
            idx,
            scenarios.len()
        );

        while phase_start.elapsed() < scenario_dur {
            let result = generate(&scenario.wfg, &schemas, &scenario.rule_plans)?;
            let event_count = result.events.len();

            let sent =
                crate::tcp_send::send_events_with_stream(&result.events, &schemas, &mut stream)?;

            total_events += event_count as u64;
            total_frames += sent as u64;
            phase_events += event_count as u64;
            phase_frames += sent as u64;

            if sleep_dur > Duration::ZERO {
                std::thread::sleep(sleep_dur);
            }
        }

        let elapsed = wall_start.elapsed().as_secs_f64();
        let eps = if elapsed > 0.0 {
            total_events as f64 / elapsed
        } else {
            0.0
        };
        eprintln!(
            "[{}] phase={} scenario={} events={} frames={} | total_events={} total_frames={} eps={:.0}",
            chrono::Local::now().format("%H:%M:%S"),
            "done",
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
            load_from_uses(&wfg, &path, &std::collections::HashMap::new())?;
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

/// Try to find .wfl files in the same directory as scenarios.
fn load_wfl_from_scenario_dir(dir: &Path) -> WfgenResult<Vec<PathBuf>> {
    let parent = dir.parent().unwrap_or(Path::new("."));
    let rules_dir = parent.join("rules");
    if rules_dir.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&rules_dir)
            .source_err(
                WfgenReason::Io,
                format!("reading rules dir {}", rules_dir.display()),
            )?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "wfl"))
            .collect();
        entries.sort();
        Ok(entries)
    } else {
        Ok(Vec::new())
    }
}
