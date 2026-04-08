use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Context;

use wfgen::datagen::generate;
use wfgen::loader::load_from_uses;
use wfgen::wfg_parser::parse_wfg;

use crate::cmd_helpers::{load_wfl_files, load_ws_files};
use crate::tcp_send::{connect_sender, send_events, send_events_with_stream};

pub(crate) fn run(
    scenario: PathBuf,
    ws: Vec<PathBuf>,
    wfl: Vec<PathBuf>,
    bench_duration: Option<String>,
    send: bool,
    addr: String,
) -> anyhow::Result<()> {
    let wfg_content = std::fs::read_to_string(&scenario).context("reading .wfg file")?;
    let wfg = parse_wfg(&wfg_content).context("parsing .wfg file")?;

    let (mut schemas, mut wfl_files) = load_from_uses(&wfg, &scenario, &HashMap::new())?;
    schemas.extend(load_ws_files(&ws)?);
    wfl_files.extend(load_wfl_files(&wfl)?);

    // Compile WFL rules
    let mut rule_plans = Vec::new();
    for wfl_file in &wfl_files {
        match wf_lang::compile_wfl(wfl_file, &schemas) {
            Ok(plans) => rule_plans.extend(plans),
            Err(e) => {
                eprintln!("Warning: WFL compilation failed: {}", e);
            }
        }
    }

    let sustained = bench_duration
        .map(|s| parse_bench_duration(&s))
        .transpose()?;

    match sustained {
        Some(target_dur) => {
            // Sustained bench: repeatedly generate for the target duration
            let wall_start = std::time::Instant::now();
            let mut iterations: u64 = 0;
            let mut total_events: u64 = 0;
            let mut total_frames: u64 = 0;
            let mut stream = if send {
                Some(connect_sender(&addr)?)
            } else {
                None
            };

            while wall_start.elapsed() < target_dur {
                let result = generate(&wfg, &schemas, &rule_plans)?;
                if let Some(stream) = stream.as_mut() {
                    let sent = send_events_with_stream(&result.events, &schemas, stream)
                        .with_context(|| format!("sending bench iteration {}", iterations + 1))?;
                    total_frames += sent as u64;
                }
                total_events += result.events.len() as u64;
                iterations += 1;
            }

            let elapsed = wall_start.elapsed();
            let secs = elapsed.as_secs_f64();
            let eps = if secs > 0.0 {
                total_events as f64 / secs
            } else {
                f64::INFINITY
            };

            println!("Iterations: {}", iterations);
            println!("Events:     {}", total_events);
            if send {
                println!("Frames:     {}", total_frames);
            }
            println!("Duration:   {:.3}s", secs);
            println!("Throughput: {:.0} events/sec", eps);
        }
        None => {
            // Single-shot bench (original behavior)
            let start = std::time::Instant::now();
            let result = generate(&wfg, &schemas, &rule_plans)?;
            let sent_frames = if send {
                Some(send_events(&result.events, &schemas, &addr)?)
            } else {
                None
            };
            let elapsed = start.elapsed();

            let events = result.events.len();
            let secs = elapsed.as_secs_f64();
            let eps = if secs > 0.0 {
                events as f64 / secs
            } else {
                f64::INFINITY
            };

            println!("Events:     {}", events);
            if let Some(frames) = sent_frames {
                println!("Frames:     {}", frames);
            }
            println!("Duration:   {:.3}s", secs);
            println!("Throughput: {:.0} events/sec", eps);
        }
    }

    Ok(())
}

/// Parse a human-friendly duration string (e.g. "30s", "2m", "1h") into `std::time::Duration`.
fn parse_bench_duration(s: &str) -> anyhow::Result<std::time::Duration> {
    let s = s.trim();
    if s.is_empty() {
        anyhow::bail!("empty duration string");
    }

    let (num_str, suffix) = if let Some(stripped) = s.strip_suffix('s') {
        (stripped, "s")
    } else if let Some(stripped) = s.strip_suffix('m') {
        (stripped, "m")
    } else if let Some(stripped) = s.strip_suffix('h') {
        (stripped, "h")
    } else {
        // Assume seconds if no suffix
        (s, "s")
    };

    let value: f64 = num_str
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid duration number: '{}'", num_str))?;

    let secs = match suffix {
        "s" => value,
        "m" => value * 60.0,
        "h" => value * 3600.0,
        _ => unreachable!(),
    };

    if secs <= 0.0 {
        anyhow::bail!("bench duration must be positive, got '{}'", s);
    }

    Ok(std::time::Duration::from_secs_f64(secs))
}
