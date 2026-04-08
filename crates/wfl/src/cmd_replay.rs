use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, IsTerminal};
use std::path::PathBuf;

use anyhow::Result;
use chrono::DateTime;

use wf_core::alert::OutputRecord;
use wf_core::rule::{
    CepStateMachine, CloseReason, Event, RuleExecutor, StepResult, Value, WindowLookup,
};
use wf_lang::WindowSchema;
use wf_lang::plan::RulePlan;
use wf_vars::ConfigVarContext;

const GREEN: &str = "\x1b[1;32m";
const RED: &str = "\x1b[1;31m";
const YELLOW: &str = "\x1b[1;38;5;208m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";
const PIPE_WINDOW_PREFIX: &str = "__wf_pipe_";
const PIPE_EVENT_TIME_FIELD: &str = "__wf_pipe_ts";

/// Result of replaying events through compiled rules.
pub struct ReplayResult {
    pub alerts: Vec<OutputRecord>,
    pub event_count: u64,
    pub match_count: u64,
    pub error_count: u64,
}

/// CLI entry point: load files → replay → print output.
pub fn run(file: PathBuf, schemas: Vec<String>, input: PathBuf, vars: Vec<String>) -> Result<()> {
    use wf_config::project::{load_schemas, load_wfl_with_context, parse_vars};

    let cwd = std::env::current_dir()?;
    let mut var_map = parse_vars(&vars)?;
    var_map
        .entry("WORK_DIR".to_string())
        .or_insert_with(|| cwd.to_string_lossy().to_string());
    let ctx = ConfigVarContext::from_explicit_vars(var_map);
    let color = std::io::stderr().is_terminal();

    let all_schemas = load_schemas(&schemas, &cwd)?;
    let source = load_wfl_with_context(&file, &ctx, Some(&cwd))?;

    let reader = BufReader::new(
        std::fs::File::open(&input)
            .map_err(|e| anyhow::anyhow!("failed to open {}: {}", input.display(), e))?,
    );

    let result = replay_events(&source, &all_schemas, reader, color)?;

    for alert in &result.alerts {
        match serde_json::to_string(alert) {
            Ok(s) => println!("{}", s),
            Err(e) => eprintln!("ERROR: failed to serialize alert: {}", e),
        }
    }

    // Summary
    eprintln!("---");
    if color {
        let ev = result.event_count;
        let mc = result.match_count;
        let ec = result.error_count;
        eprint!("{BOLD}Replay complete:{RESET} {ev} events processed, ");
        if mc > 0 {
            eprint!("{GREEN}{mc} matches{RESET}");
        } else {
            eprint!("{DIM}0 matches{RESET}");
        }
        eprint!(", ");
        if ec > 0 {
            eprintln!("{RED}{ec} errors{RESET}");
        } else {
            eprintln!("{DIM}0 errors{RESET}");
        }
    } else {
        eprintln!(
            "Replay complete: {} events processed, {} matches, {} errors",
            result.event_count, result.match_count, result.error_count
        );
    }

    Ok(())
}

/// Pure-logic replay: parse WFL source, compile, and replay events from reader.
///
/// Returns all alerts plus statistics. This function is testable without
/// filesystem access.
///
/// Events are automatically routed based on their `_stream` field and the
/// rule's bind definitions. No manual event alias is required.
pub fn replay_events<R: BufRead>(
    wfl_source: &str,
    schemas: &[WindowSchema],
    reader: R,
    color: bool,
) -> Result<ReplayResult> {
    replay_events_impl(
        wfl_source,
        schemas,
        reader,
        color,
        ReplayExecOptions {
            scan_expired_each_event: false,
            eof_action: ReplayEofAction::CloseAllEos,
        },
    )
}

/// Replay mode used by `wfl replay-verify`.
///
/// This mode aligns with oracle semantics:
/// - scans timeout expirations before each input event
/// - does NOT force `close:eos` at EOF
pub fn replay_events_for_verify<R: BufRead>(
    wfl_source: &str,
    schemas: &[WindowSchema],
    reader: R,
    color: bool,
) -> Result<ReplayResult> {
    replay_events_impl(
        wfl_source,
        schemas,
        reader,
        color,
        ReplayExecOptions {
            scan_expired_each_event: true,
            eof_action: ReplayEofAction::SweepTimeoutAtLastWatermark,
        },
    )
}

fn replay_events_impl<R: BufRead>(
    wfl_source: &str,
    schemas: &[WindowSchema],
    reader: R,
    color: bool,
    options: ReplayExecOptions,
) -> Result<ReplayResult> {
    let wfl_file =
        wf_lang::parse_wfl(wfl_source).map_err(|e| anyhow::anyhow!("parse error: {e}"))?;
    let plans = wf_lang::compile_wfl(&wfl_file, schemas)?;

    if plans.is_empty() {
        return Ok(ReplayResult {
            alerts: vec![],
            event_count: 0,
            match_count: 0,
            error_count: 0,
        });
    }

    replay_with_plans(&plans, schemas, reader, color, options)
}

/// Stub [`WindowLookup`] for replay mode.
///
/// Replay operates without a live window store, so join lookups and
/// `window.has()` guards always return `None` (no data available).
struct NullWindowLookup;

impl WindowLookup for NullWindowLookup {
    fn snapshot_field_values(
        &self,
        _window: &str,
        _field: &str,
    ) -> Option<std::collections::HashSet<String>> {
        None
    }

    fn snapshot(&self, _window: &str) -> Option<Vec<std::collections::HashMap<String, Value>>> {
        None
    }
}

struct ReplayEngine {
    machine: CepStateMachine,
    executor: RuleExecutor,
    conv_plan: Option<wf_lang::plan::ConvPlan>,
}

#[derive(Clone, Copy)]
enum ReplayEofAction {
    CloseAllEos,
    SweepTimeoutAtLastWatermark,
}

#[derive(Clone, Copy)]
struct ReplayExecOptions {
    scan_expired_each_event: bool,
    eof_action: ReplayEofAction,
}

#[derive(Clone)]
struct ConsumerRoute {
    engine_idx: usize,
    bind_alias: String,
}

/// Replay events against pre-compiled rule plans.
///
/// Events are automatically routed based on `_stream` field and rule definitions.
/// The routing logic:
/// 1. Read `_stream` from JSONL (e.g., "syslog")
/// 2. Find which window subscribes to this stream (from schema)
/// 3. Find which binds reference that window (from rule)
/// 4. Apply each bind's filter to the event
/// 5. Route matching events to the appropriate engine
fn replay_with_plans<R: BufRead>(
    plans: &[RulePlan],
    schemas: &[WindowSchema],
    reader: R,
    color: bool,
    options: ReplayExecOptions,
) -> Result<ReplayResult> {
    // Build stream -> window mapping from schemas
    let stream_to_windows: HashMap<String, Vec<String>> = build_stream_to_windows_map(schemas);

    // Build window -> binds mapping from rules
    let window_to_binds: HashMap<String, Vec<(usize, String)>> = build_window_to_binds_map(plans);

    let mut engines: Vec<ReplayEngine> = plans
        .iter()
        .map(|plan| {
            let time_field = resolve_replay_time_field_auto(plan, schemas);

            let limits = plan.limits_plan.clone();
            let sm = CepStateMachine::with_limits(
                plan.name.clone(),
                plan.match_plan.clone(),
                time_field,
                limits,
            );
            let executor = RuleExecutor::new(plan.clone());
            ReplayEngine {
                machine: sm,
                executor,
                conv_plan: plan.conv_plan.clone(),
            }
        })
        .collect();

    // Build routes for all binds (external and internal)
    let all_routes = build_all_routes(plans);

    let lookup = NullWindowLookup;
    let mut alerts = Vec::new();
    let mut event_count: u64 = 0;
    let mut match_count: u64 = 0;
    let mut error_count: u64 = 0;
    let known_time_fields = collect_known_time_fields(schemas);
    let mut has_watermark = false;
    let mut last_watermark_nanos: i64 = 0;

    // -- Event loop --
    for line_result in reader.lines() {
        let line = line_result?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let json: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                if color {
                    eprintln!(
                        "{YELLOW}WARN{RESET}: skipping invalid JSON on line {}: {}",
                        event_count + 1,
                        e
                    );
                } else {
                    eprintln!(
                        "WARN: skipping invalid JSON on line {}: {}",
                        event_count + 1,
                        e
                    );
                }
                error_count += 1;
                continue;
            }
        };

        if let Some(watermark_nanos) = infer_event_watermark_nanos(&json, &known_time_fields) {
            if !has_watermark || watermark_nanos > last_watermark_nanos {
                has_watermark = true;
                last_watermark_nanos = watermark_nanos;
            }

            if options.scan_expired_each_event {
                for i in 0..engines.len() {
                    run_timeout_scan_for_engine(
                        i,
                        watermark_nanos,
                        &all_routes,
                        &mut engines,
                        &lookup,
                        &mut alerts,
                        &mut match_count,
                        &mut error_count,
                        color,
                    );
                }
            }
        }

        let event = json_to_event_with_time_fields(&json, &known_time_fields);
        event_count += 1;

        // Auto-route: find binds that should receive this event
        let route_keys = resolve_event_routes(&json, &stream_to_windows, &window_to_binds);

        let mut queue = VecDeque::new();
        for route_key in route_keys {
            queue.push_back((route_key, event.clone()));
        }

        while let Some((route_key, route_event)) = queue.pop_front() {
            route_event_once(
                &all_routes,
                &mut engines,
                &lookup,
                &route_key,
                &route_event,
                &mut queue,
                &mut alerts,
                &mut match_count,
                &mut error_count,
                color,
            );
        }
    }

    // -- EOF --
    match options.eof_action {
        ReplayEofAction::CloseAllEos => {
            for i in 0..engines.len() {
                let close_outputs = {
                    let engine = &mut engines[i];
                    engine
                        .machine
                        .close_all_with_conv(CloseReason::Eos, engine.conv_plan.as_ref())
                };
                handle_close_outputs_for_engine(
                    i,
                    &close_outputs,
                    &all_routes,
                    &mut engines,
                    &lookup,
                    &mut alerts,
                    &mut match_count,
                    &mut error_count,
                    color,
                );
            }
        }
        ReplayEofAction::SweepTimeoutAtLastWatermark => {
            if has_watermark {
                for i in 0..engines.len() {
                    run_timeout_scan_for_engine(
                        i,
                        last_watermark_nanos,
                        &all_routes,
                        &mut engines,
                        &lookup,
                        &mut alerts,
                        &mut match_count,
                        &mut error_count,
                        color,
                    );
                }
            }
        }
    }

    Ok(ReplayResult {
        alerts,
        event_count,
        match_count,
        error_count,
    })
}

/// Build mapping from stream name to window names that subscribe to it.
fn build_stream_to_windows_map(schemas: &[WindowSchema]) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for schema in schemas {
        for stream in &schema.streams {
            map.entry(stream.clone())
                .or_default()
                .push(schema.name.clone());
        }
    }
    map
}

/// Build mapping from window name to (engine_idx, bind_alias) pairs.
fn build_window_to_binds_map(plans: &[RulePlan]) -> HashMap<String, Vec<(usize, String)>> {
    let mut map: HashMap<String, Vec<(usize, String)>> = HashMap::new();
    for (engine_idx, plan) in plans.iter().enumerate() {
        for bind in &plan.binds {
            if !is_internal_window_name(&bind.window) {
                map.entry(bind.window.clone())
                    .or_default()
                    .push((engine_idx, bind.alias.clone()));
            }
        }
    }
    map
}

/// Resolve which route keys an event should be sent to based on its `_stream` field.
fn resolve_event_routes(
    json: &serde_json::Value,
    stream_to_windows: &HashMap<String, Vec<String>>,
    window_to_binds: &HashMap<String, Vec<(usize, String)>>,
) -> Vec<String> {
    let mut routes = Vec::new();

    // Get _stream from JSON
    let stream_name = json.get("_stream").and_then(|v| v.as_str()).unwrap_or("");

    if stream_name.is_empty() {
        return routes;
    }

    // Find windows that subscribe to this stream
    let windows = stream_to_windows
        .get(stream_name)
        .cloned()
        .unwrap_or_default();

    // Find binds for each window
    for window in windows {
        let binds = window_to_binds.get(&window).cloned().unwrap_or_default();
        for (_engine_idx, bind_alias) in binds {
            routes.push(external_route_key(&bind_alias));
        }
    }

    routes
}

fn resolve_replay_time_field_auto(plan: &RulePlan, schemas: &[WindowSchema]) -> Option<String> {
    // For multi-source rules, prefer the bind that's used in the first event step
    // This matches the most common case where events come from the primary source
    if let Some(first_step) = plan.match_plan.event_steps.first()
        && let Some(first_branch) = first_step.branches.first()
    {
        let source_alias = &first_branch.source;
        if let Some(bind) = plan.binds.iter().find(|b| b.alias == *source_alias) {
            if is_internal_window_name(&bind.window) {
                return Some(PIPE_EVENT_TIME_FIELD.to_string());
            }
            if let Some(tf) = schemas
                .iter()
                .find(|s| s.name == bind.window)
                .and_then(|s| s.time_field.clone())
            {
                return Some(tf);
            }
        }
    }

    // Fallback: find the first bind with a non-internal window
    for bind in &plan.binds {
        if is_internal_window_name(&bind.window) {
            return Some(PIPE_EVENT_TIME_FIELD.to_string());
        }
        if let Some(tf) = schemas
            .iter()
            .find(|s| s.name == bind.window)
            .and_then(|s| s.time_field.clone())
        {
            return Some(tf);
        }
    }

    // Final fallback: check for internal window
    plan.binds
        .iter()
        .find(|b| is_internal_window_name(&b.window))
        .map(|_| PIPE_EVENT_TIME_FIELD.to_string())
}

fn build_all_routes(plans: &[RulePlan]) -> HashMap<String, Vec<ConsumerRoute>> {
    let mut routes: HashMap<String, Vec<ConsumerRoute>> = HashMap::new();
    for (engine_idx, plan) in plans.iter().enumerate() {
        for bind in &plan.binds {
            let route_key = if is_internal_window_name(&bind.window) {
                bind.window.clone()
            } else {
                external_route_key(&bind.alias)
            };
            routes.entry(route_key).or_default().push(ConsumerRoute {
                engine_idx,
                bind_alias: bind.alias.clone(),
            });
        }
    }
    routes
}

fn external_route_key(alias: &str) -> String {
    format!("__ext__{alias}")
}

fn is_internal_window_name(name: &str) -> bool {
    name.starts_with(PIPE_WINDOW_PREFIX)
}

#[allow(clippy::too_many_arguments)]
fn route_event_once(
    routes: &HashMap<String, Vec<ConsumerRoute>>,
    engines: &mut [ReplayEngine],
    lookup: &NullWindowLookup,
    route_key: &str,
    event: &Event,
    queue: &mut VecDeque<(String, Event)>,
    alerts: &mut Vec<OutputRecord>,
    match_count: &mut u64,
    error_count: &mut u64,
    color: bool,
) {
    let consumers = routes.get(route_key).cloned().unwrap_or_default();
    for consumer in consumers {
        let step = engines[consumer.engine_idx].machine.advance_with(
            &consumer.bind_alias,
            event,
            Some(lookup),
        );
        if let StepResult::Matched(ctx) = step {
            match engines[consumer.engine_idx]
                .executor
                .execute_match_with_joins(&ctx, lookup)
            {
                Ok(record) => {
                    handle_output_record(record, queue, alerts, match_count);
                }
                Err(e) => {
                    if color {
                        eprintln!("{RED}ERROR{RESET}: execute_match failed: {}", e);
                    } else {
                        eprintln!("ERROR: execute_match failed: {}", e);
                    }
                    *error_count += 1;
                }
            }
        }
    }
}

fn handle_output_record(
    record: OutputRecord,
    queue: &mut VecDeque<(String, Event)>,
    alerts: &mut Vec<OutputRecord>,
    match_count: &mut u64,
) {
    if is_internal_window_name(&record.yield_target) {
        queue.push_back((record.yield_target.clone(), output_record_to_event(&record)));
    } else {
        alerts.push(record);
        *match_count += 1;
    }
}

fn output_record_to_event(record: &OutputRecord) -> Event {
    let mut fields = HashMap::new();
    fields.insert(
        PIPE_EVENT_TIME_FIELD.to_string(),
        Value::Number(record.event_time_nanos as f64),
    );
    for (name, value) in &record.yield_fields {
        fields.insert(name.clone(), value.clone());
    }
    Event { fields }
}

/// Convert a serde_json::Value (object) into our Event type.
pub fn json_to_event(json: &serde_json::Value) -> Event {
    json_to_event_with_time_fields(json, &HashSet::new())
}

fn json_to_event_with_time_fields(
    json: &serde_json::Value,
    time_fields: &HashSet<String>,
) -> Event {
    let mut fields = HashMap::new();
    if let serde_json::Value::Object(map) = json {
        for (key, val) in map {
            if time_fields.contains(key)
                && let Some(nanos) = parse_json_timestamp_nanos(val)
            {
                fields.insert(key.clone(), Value::Number(nanos as f64));
                continue;
            }

            let v = match val {
                serde_json::Value::Number(n) => {
                    if let Some(f) = n.as_f64() {
                        Value::Number(f)
                    } else {
                        continue;
                    }
                }
                serde_json::Value::String(s) => Value::Str(s.clone()),
                serde_json::Value::Bool(b) => Value::Bool(*b),
                _ => continue, // skip arrays, objects, nulls
            };
            fields.insert(key.clone(), v);
        }
    }
    Event { fields }
}

fn collect_known_time_fields(schemas: &[WindowSchema]) -> HashSet<String> {
    schemas
        .iter()
        .filter_map(|s| s.time_field.clone())
        .collect()
}

fn parse_json_timestamp_nanos(v: &serde_json::Value) -> Option<i64> {
    match v {
        serde_json::Value::Number(n) => n.as_f64().map(|f| f as i64),
        serde_json::Value::String(s) => {
            if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
                return dt.timestamp_nanos_opt();
            }
            s.parse::<i64>().ok()
        }
        _ => None,
    }
}

fn infer_event_watermark_nanos(
    json: &serde_json::Value,
    known_time_fields: &HashSet<String>,
) -> Option<i64> {
    if let Some(ts) = json.get("_timestamp").and_then(parse_json_timestamp_nanos) {
        return Some(ts);
    }

    for field in known_time_fields {
        if let Some(v) = json.get(field)
            && let Some(ts) = parse_json_timestamp_nanos(v)
        {
            return Some(ts);
        }
    }

    None
}

#[allow(clippy::too_many_arguments)]
fn handle_close_outputs_for_engine(
    engine_idx: usize,
    close_outputs: &[wf_core::rule::CloseOutput],
    routes: &HashMap<String, Vec<ConsumerRoute>>,
    engines: &mut [ReplayEngine],
    lookup: &NullWindowLookup,
    alerts: &mut Vec<OutputRecord>,
    match_count: &mut u64,
    error_count: &mut u64,
    color: bool,
) {
    let mut queue = VecDeque::new();
    for close in close_outputs {
        let result = {
            let engine = &mut engines[engine_idx];
            engine.executor.execute_close_with_joins(close, lookup)
        };
        match result {
            Ok(Some(record)) => {
                handle_output_record(record, &mut queue, alerts, match_count);
            }
            Ok(None) => {}
            Err(e) => {
                if color {
                    eprintln!("{RED}ERROR{RESET}: execute_close failed: {}", e);
                } else {
                    eprintln!("ERROR: execute_close failed: {}", e);
                }
                *error_count += 1;
            }
        }
    }
    while let Some((route_key, route_event)) = queue.pop_front() {
        route_event_once(
            routes,
            engines,
            lookup,
            &route_key,
            &route_event,
            &mut queue,
            alerts,
            match_count,
            error_count,
            color,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn run_timeout_scan_for_engine(
    engine_idx: usize,
    watermark_nanos: i64,
    routes: &HashMap<String, Vec<ConsumerRoute>>,
    engines: &mut [ReplayEngine],
    lookup: &NullWindowLookup,
    alerts: &mut Vec<OutputRecord>,
    match_count: &mut u64,
    error_count: &mut u64,
    color: bool,
) {
    let close_outputs = {
        let engine = &mut engines[engine_idx];
        engine
            .machine
            .scan_expired_at_with_conv(watermark_nanos, engine.conv_plan.as_ref())
    };
    handle_close_outputs_for_engine(
        engine_idx,
        &close_outputs,
        routes,
        engines,
        lookup,
        alerts,
        match_count,
        error_count,
        color,
    );
}
