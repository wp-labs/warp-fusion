#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use wf_engine::match_engine::{
    CepStateMachine, CloseOutput, CloseReason, EngineHashMap, Event, FieldSource, JoinRow,
    RuleExecutor, StepResult, Value, WindowLookup,
};
use wf_lang::WindowSchema;
use wf_lang::plan::{ConvPlan, RulePlan};

use crate::datagen::stream_gen::GenEvent;
use crate::error::WfgenResult;

/// An oracle alert produced by the reference evaluator.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OracleAlert {
    pub rule_name: String,
    pub score: f64,
    pub entity_type: String,
    pub entity_id: String,
    pub origin: String,
    /// ISO 8601 — logical time (triggering event's timestamp).
    pub emit_time: String,
}

/// Result of oracle evaluation.
pub struct OracleResult {
    pub alerts: Vec<OracleAlert>,
}

/// Run the reference evaluator on generated events.
///
/// Creates a `CepStateMachine` + `RuleExecutor` per rule, feeds events in
/// timestamp order, and collects oracle alerts. Uses event-time nanoseconds
/// for deterministic window expiry.
///
/// SC7: when `injected_rules` is `Some`, only the rules whose names appear
/// in the set are evaluated. Rules without `inject` coverage are skipped so
/// the oracle doesn't generate spurious expected hits from baseline traffic.
pub fn run_oracle(
    events: &[GenEvent],
    rule_plans: &[RulePlan],
    scenario_start: &DateTime<Utc>,
    scenario_duration: &Duration,
    injected_rules: Option<&std::collections::HashSet<String>>,
) -> WfgenResult<OracleResult> {
    run_oracle_events(
        events.iter().cloned(),
        rule_plans,
        scenario_start,
        scenario_duration,
        injected_rules,
    )
}

/// 流式版 `run_oracle`：事件以迭代器逐条送入，不要求全量物化
/// （verify-nexmark 大流量场景——30M/100M 事件无法驻留内存）。
/// 事件顺序必须与 `run_oracle` 的要求一致（时间序，见其文档）。
pub fn run_oracle_events<I>(
    events: I,
    rule_plans: &[RulePlan],
    scenario_start: &DateTime<Utc>,
    scenario_duration: &Duration,
    injected_rules: Option<&std::collections::HashSet<String>>,
) -> WfgenResult<OracleResult>
where
    I: Clone + IntoIterator<Item = GenEvent>,
{
    run_oracle_events_opts(
        events,
        rule_plans,
        scenario_start,
        scenario_duration,
        injected_rules,
        true,
    )
}

/// `run_oracle_events` + EOS 收口开关：`close_at_eos = true`（默认，cmd_gen 有限
/// 场景 batch 语义）时流结束后 `close_all` 剩余实例；`false`（verify-nexmark 与
/// 引擎 replay 对拍）时不推进 EOS——引擎在流结束不再推进 watermark，尾部未收口
/// 窗口不会 close（实证：q9/q16 fixed+close 规则，close_all 会多出尾部窗口 EMIT，
/// q9 差 1.07M、q16 差 2.6k，均 oracle 偏多）。
pub fn run_oracle_events_opts<I>(
    events: I,
    rule_plans: &[RulePlan],
    scenario_start: &DateTime<Utc>,
    scenario_duration: &Duration,
    injected_rules: Option<&std::collections::HashSet<String>>,
    close_at_eos: bool,
) -> WfgenResult<OracleResult>
where
    I: Clone + IntoIterator<Item = GenEvent>,
{
    // 无 schemas → 无窗口状态 → join 一律不评估（历史行为）。
    run_oracle_events_full(
        events,
        rule_plans,
        &[],
        scenario_start,
        scenario_duration,
        injected_rules,
        close_at_eos,
    )
}

/// `run_oracle_events_opts` + 窗口 schemas（P2, join-then-key）：为 join 目标
/// 窗口维护行状态（`over` 滑动过期），使 join 键规则（如新 Q4 `match<category>`）
/// 与 match 时 join 富化/anti join 在 oracle 中真实评估。`schemas` 提供各窗口
/// 的 `over` 保留时长；空数组 = 无窗口状态（历史行为）。
pub fn run_oracle_events_full<I>(
    events: I,
    rule_plans: &[RulePlan],
    schemas: &[WindowSchema],
    scenario_start: &DateTime<Utc>,
    scenario_duration: &Duration,
    injected_rules: Option<&std::collections::HashSet<String>>,
    close_at_eos: bool,
) -> WfgenResult<OracleResult>
where
    I: Clone + IntoIterator<Item = GenEvent>,
{
    if rule_plans.is_empty() {
        return Ok(OracleResult { alerts: vec![] });
    }

    // Build per-rule engines, filtering to injected rules only (SC7)
    let mut engines: Vec<RuleEngine> = rule_plans
        .iter()
        .filter(|plan| {
            injected_rules
                .map(|set| set.contains(&plan.name))
                .unwrap_or(true)
        })
        .map(|plan| {
            let alias_map = build_window_alias_map(plan);
            RuleEngine {
                sm: CepStateMachine::new(plan.name.clone(), plan.match_plan.clone(), None),
                executor: RuleExecutor::new(plan.clone()),
                conv_plan: plan.conv_plan.clone(),
                // `on each`（无状态）规则走事件级评估，不用状态机
                each: plan.each_plan.is_some(),
                has_joins: !plan.joins.is_empty(),
                // fixed 窗口只在桶边界收口——桶内事件不可能触发收口，
                // 跨桶边界事件才 scan（避免每事件全扫实例：q4 10m 162s → 秒级）
                fixed_bucket_nanos: match plan.match_plan.window_spec {
                    wf_lang::plan::WindowSpec::Fixed(dur) => Some(dur.as_nanos() as i64),
                    _ => None,
                },
                last_scan_bucket: i64::MIN,
                alias_map,
                lookup: OracleLookup::build(plan, schemas),
            }
        })
        .collect();

    // P2 (Path A): 预加载 join 目标窗口的全部行。引擎 replay 的窗口 actor
    // append 超前于规则任务消费（pull/push 解耦），join_lookup 因此能看到
    // "已 append 但尚未被本事件消费"的行——oracle 同步流式看不到（bid 随机
    // 引用"未来"auction 时 200k 仅 57% 命中 vs 引擎 100%）。预加载把 join 窗口
    // 的完整历史放入 lookup，镜像引擎的 append 超前；行保留仍按事件时间
    // `over` 过期（与引擎驱逐的语义目标一致；sweep 时机差异为已知差异）。
    //
    // 预加载遍通过 `events.clone()` 流式消费（不收集 Vec——verify 输入是
    // 惰性桶迭代器，30M/100M 事件驻留内存会 OOM）；仅喂 join 目标窗口行。
    // 无 join 规则的引擎组跳过预加载遍（避免全流双遍历）。
    if engines.iter().any(|e| e.lookup.is_some()) {
        for ev in events.clone() {
            let core = gen_event_to_core(&ev);
            let ns = ev.timestamp.timestamp_nanos_opt().unwrap_or(0);
            for engine in &mut engines {
                if let Some(lookup) = &mut engine.lookup {
                    lookup.feed_window(&ev.window_name, &core, ns);
                }
            }
        }
    }

    let mut alerts = Vec::new();

    // Process events in order (caller should have sorted by timestamp)
    for event in events {
        let event_nanos = event.timestamp.timestamp_nanos_opt().unwrap_or(0);

        let core_event = gen_event_to_core(&event);

        for engine in &mut engines {
            // P2 (Path A): maintain join-window rows ahead of this event so
            // join-then-key and match-time joins see the same live rows the
            // engine's window registry does (same stream order → same
            // visibility; rows expire at `ts + over <= watermark`).
            if let Some(lookup) = &mut engine.lookup {
                lookup.feed_window(&event.window_name, &core_event, event_nanos);
            }

            // Scan for expired instances first (with conv). Fixed-window rules
            // only ever close at bucket boundaries, so a bucket-internal event
            // cannot expire anything — skip the scan until the event time
            // crosses into a new bucket (keeps `and close` rules linear).
            let should_scan = match engine.fixed_bucket_nanos {
                Some(dur) => {
                    let bucket = event_nanos.div_euclid(dur);
                    if bucket == engine.last_scan_bucket {
                        false
                    } else {
                        engine.last_scan_bucket = bucket;
                        true
                    }
                }
                None => true,
            };
            if should_scan {
                let expired = engine
                    .sm
                    .scan_expired_at_with_conv(event_nanos, engine.conv_plan.as_ref());
                collect_close_alerts(
                    &engine.executor,
                    expired,
                    engine.has_joins,
                    engine.lookup.as_ref().map(|l| l as &dyn WindowLookup),
                    &mut alerts,
                );
            }

            // Find bind aliases for this event's window
            let bind_aliases = match engine.alias_map.get(&event.window_name) {
                Some(aliases) => aliases,
                None => continue, // this rule doesn't use this window
            };

            // `on each`（无状态）：事件级评估一次（each 规则单 bind，
            // 任一 alias 命中即整体评估，避免逐 alias 重复计数）。
            if engine.each {
                if let Ok(Some(record)) = engine.executor.execute_each(&core_event, event_nanos) {
                    push_alert(record, &mut alerts);
                }
                continue;
            }

            // Advance the state machine for each alias bound to this window
            for bind_alias in bind_aliases {
                if !engine
                    .executor
                    .event_matches_alias(bind_alias, &core_event, None)
                {
                    continue;
                }
                let windows = engine.lookup.as_ref().map(|l| l as &dyn WindowLookup);
                let result = engine.sm.advance_at_with_masks(
                    bind_alias,
                    &core_event,
                    event_nanos,
                    windows,
                    0,
                    None,
                );

                if let StepResult::Matched(ctx) = result {
                    let alert_record = if engine.has_joins {
                        // Match-time joins: enrichment (snapshot), anti-join
                        // drop, asof — evaluated against the window state.
                        // No state (no schemas) → every join misses (legacy
                        // oracle behavior: anti keeps, snapshot enriches nothing).
                        let lookup: &dyn WindowLookup = match windows {
                            Some(l) => l,
                            None => &EmptyLookup,
                        };
                        engine.executor.execute_match_with_joins(&ctx, lookup)
                    } else {
                        engine.executor.execute_match(&ctx).map(Some)
                    };
                    if let Ok(Some(alert_record)) = alert_record {
                        push_alert(alert_record, &mut alerts);
                    }
                }
            }
        }
    }

    let eos_time =
        *scenario_start + chrono::Duration::from_std(*scenario_duration).unwrap_or_default();
    let eos_nanos = eos_time.timestamp_nanos_opt().unwrap_or(i64::MAX);

    /// 引擎 replay 语义（close_at_eos = false）：流结束即止，不扫 EOS 过期、
    /// 不 close_all——与引擎在 replay 收尾不再推进 watermark 的行为一致。
    if close_at_eos {
        for engine in &mut engines {
            let expired = engine
                .sm
                .scan_expired_at_with_conv(eos_nanos, engine.conv_plan.as_ref());
            collect_close_alerts(
                &engine.executor,
                expired,
                engine.has_joins,
                engine.lookup.as_ref().map(|l| l as &dyn WindowLookup),
                &mut alerts,
            );

            // End-of-scenario in datagen is also a finite replay boundary. After
            // the timeout sweep, close remaining active instances so `and close`
            // rules match batch/EOF execution semantics.
            let closed = engine
                .sm
                .close_all_with_conv(CloseReason::Eos, engine.conv_plan.as_ref());
            collect_close_alerts(
                &engine.executor,
                closed,
                engine.has_joins,
                engine.lookup.as_ref().map(|l| l as &dyn WindowLookup),
                &mut alerts,
            );
        }
    }

    Ok(OracleResult { alerts })
}

// ---------------------------------------------------------------------------
// Internal types and helpers
// ---------------------------------------------------------------------------

struct RuleEngine {
    sm: CepStateMachine,
    executor: RuleExecutor,
    conv_plan: Option<ConvPlan>,
    /// `on each`（无状态）规则：事件级评估，不走状态机
    each: bool,
    /// 规则是否有 join（决定 match 时走 execute_match_with_joins）
    has_joins: bool,
    /// fixed 窗口的桶长（oracle 桶边界扫描优化）；None = sliding/session/each
    fixed_bucket_nanos: Option<i64>,
    /// 上一次 scan 的桶号（fixed 窗口）
    last_scan_bucket: i64,
    /// window_name → Vec<bind_alias> for routing events to all matching aliases
    alias_map: HashMap<String, Vec<String>>,
    /// P2: join 目标窗口状态（join-then-key / match 时 join 评估）；无 join 规则为 None
    lookup: Option<OracleLookup>,
}

/// Join lookup with no state — every join misses. Used when a rule has joins
/// but no schemas were supplied (legacy oracle behavior).
struct EmptyLookup;

impl WindowLookup for EmptyLookup {
    fn snapshot_field_values(
        &self,
        _window: &str,
        _field: &str,
    ) -> Option<std::collections::HashSet<String>> {
        None
    }

    fn snapshot(&self, _window: &str) -> Option<Vec<JoinRow>> {
        None
    }

    fn join_lookup(&self, _window: &str, _key_field: &str, _key: &Value) -> Option<Vec<JoinRow>> {
        None
    }
}

/// P2 (Path A): per-rule join window state for the oracle reference evaluator.
///
/// Mirrors the engine window registry's retention: rows are indexed by the
/// rule's join-condition right-side key field and expire when
/// `ts + over <= watermark` (sliding `over`). Only windows targeted by the
/// rule's joins are tracked. A row is keyed by its join-key value; duplicate
/// keys keep the latest row (auction/person ids are unique in NEXMark).
struct OracleLookup {
    windows: HashMap<String, OracleWindow>,
}

struct OracleWindow {
    /// Index key field (join condition's right side, e.g. `id`).
    key_field: String,
    /// key repr → (event timestamp nanos, event).
    rows: HashMap<String, (i64, Event)>,
    /// ts → keys of rows inserted at that ts. Monotonic watermark expiry pops
    /// from the front in amortized O(1) per event — a full `retain` scan per
    /// event would be O(window rows) × O(events) = quadratic (the 18m hang).
    by_ts: BTreeMap<i64, Vec<String>>,
    /// Sliding retention (`over`) of the target window.
    over_nanos: i64,
}

impl OracleLookup {
    /// Build the lookup for a rule's joins. `None` when the rule has no joins
    /// or no tracked window schema is found.
    fn build(plan: &RulePlan, schemas: &[WindowSchema]) -> Option<Self> {
        if plan.joins.is_empty() {
            return None;
        }
        let mut windows = HashMap::new();
        for join in &plan.joins {
            let Some(schema) = schemas.iter().find(|s| s.name == join.right_window) else {
                continue;
            };
            let Some(cond) = join.conds.first() else {
                continue;
            };
            // checker rejects nested join-condition paths — skip rather than
            // dropping ALL window state via `?` (a single odd join must not
            // disable join state for the other joins).
            let Some(key_field) = wf_lang::plan::JoinCondPlan::right_field_name(cond) else {
                continue;
            };
            windows.insert(
                join.right_window.clone(),
                OracleWindow {
                    key_field: key_field.to_string(),
                    rows: HashMap::new(),
                    by_ts: BTreeMap::new(),
                    over_nanos: schema.over.as_nanos() as i64,
                },
            );
        }
        if windows.is_empty() {
            None
        } else {
            Some(OracleLookup { windows })
        }
    }

    /// Record an incoming row of a tracked window, then expire rows older than
    /// `over` at THIS window's own watermark.
    ///
    /// Watermark semantics (mirrors the engine): each window's retention is
    /// driven by its OWN event stream (the auction window expires auction rows
    /// by auction events, not by the driver's bid timestamps). An event for an
    /// untracked window advances nothing — a late bid must not evict auction
    /// rows (the 2026-08 review finding; NEXMark's over≈auction lifetime masks
    /// it, but the semantics must not depend on that).
    fn feed_window(&mut self, window: &str, event: &Event, ts_nanos: i64) {
        let Some(win) = self.windows.get_mut(window) else {
            return;
        };
        win.expire_before(ts_nanos);
        if let Some(key) = event
            .field_value(&win.key_field)
            .and_then(|v| value_key_repr(&v))
        {
            win.rows.insert(key.clone(), (ts_nanos, event.clone()));
            win.by_ts.entry(ts_nanos).or_default().push(key);
        }
    }

    fn snapshot(&self, window: &str) -> Option<Vec<JoinRow>> {
        let win = self.windows.get(window)?;
        Some(
            win.rows
                .values()
                .map(|(_, ev)| JoinRow::Event(Arc::new(ev.clone())))
                .collect(),
        )
    }

    fn snapshot_with_timestamps(&self, window: &str) -> Option<Vec<(i64, JoinRow)>> {
        let win = self.windows.get(window)?;
        Some(
            win.rows
                .values()
                .map(|(ts, ev)| (*ts, JoinRow::Event(Arc::new(ev.clone()))))
                .collect(),
        )
    }

    fn join_lookup(&self, window: &str, key_field: &str, key: &Value) -> Option<Vec<JoinRow>> {
        let win = self.windows.get(window)?;
        if win.key_field != key_field {
            return None;
        }
        let key_repr = value_key_repr(key)?;
        win.rows
            .get(&key_repr)
            .map(|(_, ev)| vec![JoinRow::Event(Arc::new(ev.clone()))])
    }
}

impl OracleWindow {
    /// Drop rows with `ts + over <= watermark` (i.e. `ts <= watermark - over`).
    /// Watermark is monotonic, so this pops the front of the ts index.
    fn expire_before(&mut self, watermark: i64) {
        if self.over_nanos <= 0 {
            return;
        }
        let cutoff = watermark - self.over_nanos;
        let mut expired: Vec<String> = Vec::new();
        while let Some((&ts, _)) = self.by_ts.iter().next() {
            if ts > cutoff {
                break;
            }
            if let Some(mut keys) = self.by_ts.remove(&ts) {
                expired.append(&mut keys);
            }
        }
        for key in expired {
            self.rows.remove(&key);
        }
    }
}

impl WindowLookup for OracleLookup {
    fn snapshot_field_values(
        &self,
        _window: &str,
        _field: &str,
    ) -> Option<std::collections::HashSet<String>> {
        None
    }

    fn snapshot(&self, window: &str) -> Option<Vec<JoinRow>> {
        self.snapshot(window)
    }

    fn snapshot_with_timestamps(&self, window: &str) -> Option<Vec<(i64, JoinRow)>> {
        self.snapshot_with_timestamps(window)
    }

    fn join_lookup(&self, window: &str, key_field: &str, key: &Value) -> Option<Vec<JoinRow>> {
        self.join_lookup(window, key_field, key)
    }
}

/// Stable string repr of a scalar join key value (insert and lookup sides go
/// through the same conversion, so they always agree). Float keys are excluded
/// by the checker for join index keys, so bit-exact f64 is fine here.
fn value_key_repr(v: &Value) -> Option<String> {
    match v {
        Value::Number(n) => Some(format!("n:{}", n.to_bits())),
        Value::Str(s) => Some(format!("s:{}", s)),
        Value::Bool(b) => Some(format!("b:{}", b)),
        _ => None,
    }
}

fn push_alert(alert_record: wf_engine::alert::OutputRecord, alerts: &mut Vec<OracleAlert>) {
    alerts.push(OracleAlert {
        rule_name: alert_record.rule_name.to_string(),
        score: alert_record.score,
        entity_type: alert_record.entity_type.to_string(),
        entity_id: alert_record.entity_id,
        origin: alert_record.origin.as_str().to_string(),
        emit_time: alert_record.fired_at.clone(),
    });
}

fn collect_close_alerts(
    executor: &RuleExecutor,
    close_outputs: Vec<CloseOutput>,
    has_joins: bool,
    windows: Option<&dyn WindowLookup>,
    alerts: &mut Vec<OracleAlert>,
) {
    for close_out in close_outputs {
        // Close-path join enrichment must mirror the engine's
        // `execute_close_with_joins` (rule_task.rs:1090) — otherwise a close
        // rule whose yield/entity/score reads extra join fields would mismatch
        // on field values. No state (no schemas) → EmptyLookup (join misses).
        let result = if has_joins {
            let lookup: &dyn WindowLookup = match windows {
                Some(l) => l,
                None => &EmptyLookup,
            };
            executor.execute_close_with_joins(&close_out, lookup)
        } else {
            executor.execute_close(&close_out)
        };
        if let Ok(Some(alert_record)) = result {
            alerts.push(OracleAlert {
                rule_name: alert_record.rule_name.to_string(),
                score: alert_record.score,
                entity_type: alert_record.entity_type.to_string(),
                entity_id: alert_record.entity_id,
                origin: alert_record.origin.as_str().to_string(),
                emit_time: alert_record.fired_at.clone(),
            });
        }
    }
}

/// Build a mapping from window name to ALL bind aliases for a rule.
fn build_window_alias_map(plan: &RulePlan) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for bind in &plan.binds {
        map.entry(bind.window.clone())
            .or_default()
            .push(bind.alias.clone());
    }
    map
}

/// Convert a GenEvent to a wf_core Event.
fn gen_event_to_core(event: &GenEvent) -> Event {
    let mut fields: EngineHashMap<_, Value> = EngineHashMap::default();
    for (k, v) in &event.fields {
        if let Some(core_v) = json_to_core_value(v) {
            fields.insert(k.clone().into(), core_v);
        }
    }
    Event { fields }
}

fn json_to_core_value(v: &serde_json::Value) -> Option<Value> {
    match v {
        serde_json::Value::String(s) => Some(Value::Str(s.clone().into())),
        serde_json::Value::Number(n) => n.as_f64().map(Value::Number),
        serde_json::Value::Bool(b) => Some(Value::Bool(*b)),
        _ => None,
    }
}

/// Tolerance settings extracted from the oracle block params.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OracleTolerances {
    /// Time tolerance for verify matching (default 1s).
    pub time_tolerance_secs: f64,
    /// Score tolerance for verify matching (default 0.01).
    pub score_tolerance: f64,
}

impl Default for OracleTolerances {
    fn default() -> Self {
        Self {
            time_tolerance_secs: 1.0,
            score_tolerance: 0.01,
        }
    }
}

/// Extract tolerance parameters from the parsed oracle block.
pub fn extract_oracle_tolerances(oracle: &crate::wfg_ast::OracleBlock) -> OracleTolerances {
    let mut tolerances = OracleTolerances::default();
    for param in &oracle.params {
        match param.name.as_str() {
            "time_tolerance" => {
                if let crate::wfg_ast::ParamValue::Duration(d) = &param.value {
                    tolerances.time_tolerance_secs = d.as_secs_f64();
                }
            }
            "score_tolerance" => {
                if let crate::wfg_ast::ParamValue::Number(n) = &param.value {
                    tolerances.score_tolerance = *n;
                }
            }
            _ => {}
        }
    }
    tolerances
}
