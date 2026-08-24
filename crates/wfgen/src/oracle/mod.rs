#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use wf_engine::alert::OutputRecord;
use wf_engine::match_engine::{
    CepStateMachine, CloseOutput, CloseReason, DeferredPending, EngineHashMap, Event, FieldSource,
    JoinRow, RuleExecutor, StepResult, Value, WindowLookup,
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

    // Build per-rule engines, filtering to injected rules only (SC7).
    // Stats（`stats<...>`）规则 oracle 尚不支持（StatsExecutor 是列式批执行器，
    // oracle 逐事件无等价路径）——跳过并计数，调用方（verify）据此把 stats 规则
    // 标记为「oracle 未覆盖」而不是 panic（Q19 stats top 曾除零崩溃）。
    let mut skipped_stats = 0usize;
    let mut engines: Vec<RuleEngine> = Vec::new();
    for plan in rule_plans.iter().filter(|plan| {
        injected_rules
            .map(|set| set.contains(&plan.name))
            .unwrap_or(true)
    }) {
        if plan.stats_plan.is_some() {
            skipped_stats += 1;
            continue;
        }
        let alias_map = build_window_alias_map(plan);
        engines.push(RuleEngine {
            sm: CepStateMachine::new(plan.name.clone(), plan.match_plan.clone(), None),
            executor: RuleExecutor::new(plan.clone()),
            conv_plan: plan.conv_plan.clone(),
            // `on each`（无状态）规则走事件级评估，不用状态机
            each: plan.each_plan.is_some(),
            has_joins: !plan.joins.is_empty(),
            // fixed 窗口只在桶边界收口——桶内事件不可能触发收口，
            // 跨桶边界事件才 scan（避免每事件全扫实例：q4 10m 162s → 秒级）。
            // Hop 窗口在 slide 对齐时刻收口（expire = w_start + size 亦为 slide
            // 边界），跨 slide 边界才 scan 同样安全。
            fixed_bucket_nanos: match plan.match_plan.window_spec {
                wf_lang::plan::WindowSpec::Fixed(dur) => Some(dur.as_nanos() as i64),
                wf_lang::plan::WindowSpec::Hop { slide, .. } => Some(slide.as_nanos() as i64),
                _ => None,
            },
            // Hop 扫描用无界预算（每 slide 边界恰一个窗口到期，收口原子）。
            hop_unbounded: matches!(
                plan.match_plan.window_spec,
                wf_lang::plan::WindowSpec::Hop { .. }
            ),
            last_scan_bucket: i64::MIN,
            alias_map,
            lookup: OracleLookup::build(plan, schemas),
            deferred: plan
                .joins
                .iter()
                .position(|j| j.emit_at.is_some())
                .map(|join_idx| DeferredState {
                    join_idx,
                    watermark: i64::MIN,
                    pending: BTreeMap::new(),
                }),
        });
    }
    if skipped_stats > 0 {
        eprintln!(
            "oracle: 跳过 {} 个 stats 规则（StatsExecutor 暂未接入 oracle）",
            skipped_stats
        );
    }

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

    // 中间管道（2026-08-23 q13 双规则链）：被 bind 的 yield target 是中间窗口——
    // 上游规则输出到它时，既计数（与引擎 emitted_total 含中间一致）又作为事件
    // feed 给 bind 该窗口的下游规则（on-each 无状态路径；match 下游的 expiry
    // scan 由驱动事件主遍负责，中间 feed 只做状态推进，q13 场景为 each 规则）。
    let consumed_windows: std::collections::HashSet<&str> = rule_plans
        .iter()
        .filter(|plan| {
            injected_rules
                .map(|set| set.contains(&plan.name))
                .unwrap_or(true)
        })
        .flat_map(|plan| plan.binds.iter().map(|b| b.window.as_str()))
        .collect();
    let intermediate_windows: std::collections::HashSet<String> = rule_plans
        .iter()
        .filter(|plan| {
            injected_rules
                .map(|set| set.contains(&plan.name))
                .unwrap_or(true)
        })
        .map(|plan| plan.yield_plan.target.as_str())
        .filter(|target| consumed_windows.contains(target))
        .map(String::from)
        .collect();

    // Process events in order (caller should have sorted by timestamp)
    for event in events {
        let event_nanos = event.timestamp.timestamp_nanos_opt().unwrap_or(0);

        let core_event = gen_event_to_core(&event);

        // 本事件的中间输出：既 push_alert（计数）又入队 feed 下游。
        let mut feed_queue: Vec<(String, Event, i64)> = Vec::new();

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
                // Hop 窗口：每 slide 边界恰一个窗口到期，用无界预算一次性收口
                // （1024 预算会把同一窗口关闭拆多批，inline conv 逐批 top-1 重复
                // EMIT）；fixed/sliding/session 保持预算扫描（对齐引擎行为）。
                let expired = if engine.hop_unbounded {
                    engine
                        .sm
                        .scan_expired_at_with_conv_skip_non_alerting_unbounded(
                            event_nanos,
                            engine.conv_plan.as_ref(),
                        )
                } else {
                    engine
                        .sm
                        .scan_expired_at_with_conv(event_nanos, engine.conv_plan.as_ref())
                };
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
                // 先过 bind filter（events 块 filter，与 match 规则同路径
                // event_matches_alias）——修复：此前直接 execute_each 只检查
                // `on each` 的 where 子句，漏掉 events 块 filter，q2/q10 的
                // MOD/子集过滤在 oracle 里被忽略，EMIT 虚高（对拍假一致）。
                let any_bind_matches = bind_aliases
                    .iter()
                    .any(|a| engine.executor.event_matches_alias(a, &core_event, None));
                if !any_bind_matches {
                    continue;
                }
                // P3：deferred join（`emit at`）——驱动事件挂起（expiry = emit at），
                // 不即时输出；到期评估在水位推进后 pop_due（镜像引擎批次尾 scan）。
                if let Some(deferred) = &mut engine.deferred {
                    let (due, join_idx, watermark) = {
                        deferred.watermark = deferred.watermark.max(event_nanos);
                        let join_idx = deferred.join_idx;
                        if let Some(p) =
                            engine
                                .executor
                                .deferred_pending_for(join_idx, &core_event, event_nanos)
                        {
                            deferred.pending.entry(p.expiry_nanos).or_default().push(p);
                        }
                        (
                            deferred.pop_due(deferred.watermark),
                            join_idx,
                            deferred.watermark,
                        )
                    };
                    let windows: &dyn WindowLookup = engine
                        .lookup
                        .as_ref()
                        .map(|l| l as &dyn WindowLookup)
                        .unwrap_or(&EmptyLookup);
                    for p in due {
                        if let Ok(Some(record)) = engine
                            .executor
                            .execute_deferred_join(join_idx, &p, windows, watermark)
                        {
                            push_alert(record, &mut alerts);
                        }
                    }
                    continue;
                }
                // 有 join 的 on each 规则（如 q20：bid ⋈ auction + where
                // category==10）必须走 with_joins 路径，否则 join 富化缺失、
                // `where` 引用的 join 字段求值为 None → 全抑制或全放行（假对拍）。
                if engine.has_joins {
                    let lookup: &dyn WindowLookup = engine
                        .lookup
                        .as_ref()
                        .map(|l| l as &dyn WindowLookup)
                        .unwrap_or(&EmptyLookup);
                    if let Ok(Some(record)) = engine.executor.execute_each_with_joins(
                        &core_event,
                        event_nanos,
                        lookup,
                        &[],
                        event_nanos,
                    ) {
                        record_output(record, &mut alerts, &intermediate_windows, &mut feed_queue);
                    }
                } else if let Ok(Some(record)) =
                    engine.executor.execute_each(&core_event, event_nanos)
                {
                    record_output(record, &mut alerts, &intermediate_windows, &mut feed_queue);
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
                        record_output(
                            alert_record,
                            &mut alerts,
                            &intermediate_windows,
                            &mut feed_queue,
                        );
                    }
                }
            }
        }

        // 中间管道 feed：本事件所有引擎处理完的中间输出 → 作为事件喂给 bind
        // 该窗口的下游规则（同一事件序；输出若又是中间则继续入队，支持多级）。
        let mut feed_index = 0usize;
        while feed_index < feed_queue.len() {
            let (window, ev, ts) = feed_queue[feed_index].clone();
            feed_index += 1;
            for engine in &mut engines {
                if let Some(record) = process_engine_event(engine, &window, &ev, ts) {
                    record_output(record, &mut alerts, &intermediate_windows, &mut feed_queue);
                }
            }
        }
    }

    let eos_time =
        *scenario_start + chrono::Duration::from_std(*scenario_duration).unwrap_or_default();
    let eos_nanos = eos_time.timestamp_nanos_opt().unwrap_or(i64::MAX);

    // 引擎 replay 语义（close_at_eos = false）：不 close_all 剩余实例；但引擎
    // replay 的 slice 水位会推进到数据末尾的 slice 边界（fixed 桶在数据末尾
    // 恰好到期时会收口——q5 实证：30m 数据 + 10m 桶引擎收 3 桶、oracle 只收
    // 2 桶），故 oracle 也扫一次 eos 水位（不含 close_all）模拟该行为。
    // close_at_eos = true（cmd_gen batch 语义）时另 close_all 剩余实例。
    // P3：deferred join 同源——引擎水位到 slice 边界会使尾部 expiry 恰好位于
    // [数据末尾, slice 边界] 的挂起实例到期（Q8 实证 10M 差 185 条尾部桶），
    // oracle 也按 eos 水位 pop_due；close_at_eos 时才 flush 全部剩余（i64::MAX）。
    for engine in &mut engines {
        let expired = if engine.hop_unbounded {
            engine
                .sm
                .scan_expired_at_with_conv_skip_non_alerting_unbounded(
                    eos_nanos,
                    engine.conv_plan.as_ref(),
                )
        } else {
            engine
                .sm
                .scan_expired_at_with_conv(eos_nanos, engine.conv_plan.as_ref())
        };
        collect_close_alerts(
            &engine.executor,
            expired,
            engine.has_joins,
            engine.lookup.as_ref().map(|l| l as &dyn WindowLookup),
            &mut alerts,
        );

        // P3：deferred join —— EOS 水位扫（两种模式都做：镜像引擎 slice 边界
        // 水位，只到期不 flush）。
        // ⚠ 2026-08-22 修正：deferred 的 watermark 语义是**最后驱动事件时间**
        // （rule_task `deferred.watermark` 只随驱动事件更新），**不是** fixed
        // 窗口的 slice 边界水位——按 eos_nanos（= slice 边界）sweep 会把尾部
        // 未到期的实例错误输出（Q8 实证：引擎 82446 vs 按 eos 扫 83274，多
        // 828 条尾部 10s 桶）；主遍逐事件 pop_due 已覆盖全部到期实例，EOS
        // 无需再扫。close_at_eos=true（cmd_gen batch 语义）才 flush 全部剩余。
        if close_at_eos
            && let Some(deferred) = &mut engine.deferred
        {
            // P3：deferred join —— EOS 关闭触发全部剩余挂起实例（引擎 flush 语义）。
            let (due, join_idx) = (deferred.pop_due(i64::MAX), deferred.join_idx);
            let windows: &dyn WindowLookup = engine
                .lookup
                .as_ref()
                .map(|l| l as &dyn WindowLookup)
                .unwrap_or(&EmptyLookup);
            for p in due {
                if let Ok(Some(record)) = engine
                    .executor
                    .execute_deferred_join(join_idx, &p, windows, eos_nanos)
                {
                    push_alert(record, &mut alerts);
                }
            }
        }

        if close_at_eos {
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
            // P3：deferred join —— EOS 关闭触发全部剩余挂起实例（引擎 flush 语义）。
            if let Some(deferred) = &mut engine.deferred {
                let (due, join_idx) = (deferred.pop_due(i64::MAX), deferred.join_idx);
                let windows: &dyn WindowLookup = engine
                    .lookup
                    .as_ref()
                    .map(|l| l as &dyn WindowLookup)
                    .unwrap_or(&EmptyLookup);
                for p in due {
                    if let Ok(Some(record)) = engine
                        .executor
                        .execute_deferred_join(join_idx, &p, windows, eos_nanos)
                    {
                        push_alert(record, &mut alerts);
                    }
                }
            }
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
    /// Hop 规则：扫描用无界预算（每 slide 边界恰一个窗口到期，原子收口）
    hop_unbounded: bool,
    /// 上一次 scan 的桶号（fixed 窗口）
    last_scan_bucket: i64,
    /// window_name → Vec<bind_alias> for routing events to all matching aliases
    alias_map: HashMap<String, Vec<String>>,
    /// P2: join 目标窗口状态（join-then-key / match 时 join 评估）；无 join 规则为 None
    lookup: Option<OracleLookup>,
    /// P3: deferred join（`emit at`）挂起队列；无 emit_at 规则为 None
    deferred: Option<DeferredState>,
}

/// P3: deferred join 运行时状态——挂起队列（按 expiry 排序）+ 事件时间 watermark。
///
/// 镜像引擎 rule_task 的 DeferredRuntime（join-family-design §5.2）：驱动事件到达时
/// `deferred_pending_for` 挂起（expiry = `emit at` 求值）；watermark 推进后
/// `pop_due` 取出所有 `expiry <= watermark` 的实例并评估。右窗行由
/// [`OracleLookup`] 预加载（append 超前镜像），到期评估时候选已全量可见。
struct DeferredState {
    /// 规则内第一个带 `emit at` 的 join（v1 单 deferred join）。
    join_idx: usize,
    /// 事件时间 watermark（驱动事件 max 事件时间——与引擎一致，右窗事件不推进）。
    watermark: i64,
    /// expiry_nanos → 挂起实例（BTreeMap 按到期时间排序，pop 摊还 O(log n)）。
    pending: BTreeMap<i64, Vec<DeferredPending>>,
}

impl DeferredState {
    /// 取出所有 `expiry <= wm` 的实例（顺序按 expiry 升序）。
    fn pop_due(&mut self, wm: i64) -> Vec<DeferredPending> {
        let keys: Vec<i64> = self.pending.range(..=wm).map(|(k, _)| *k).collect();
        let mut due = Vec::new();
        for k in keys {
            if let Some(mut bucket) = self.pending.remove(&k) {
                due.append(&mut bucket);
            }
        }
        due
    }
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
/// rule's joins are tracked.
///
/// **每键多行**：join 目标可能是 bid（key=auction 不唯一，Q9 生命周期内同 auction
/// 多 bid 都要参与 reduce）——`rows` 按 key 存 `Vec<(ts, Event)>`（插入序），
/// 而不是单行覆盖。唯一键窗口（auction/person 表）每键恰 1 行，行为不变。
struct OracleLookup {
    windows: HashMap<String, OracleWindow>,
}

struct OracleWindow {
    /// Index key field (join condition's right side, e.g. `id`).
    key_field: String,
    /// key repr → 该键的全部行（插入序）。
    rows: HashMap<String, Vec<(i64, Event)>>,
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
            win.rows
                .entry(key.clone())
                .or_default()
                .push((ts_nanos, event.clone()));
            win.by_ts.entry(ts_nanos).or_default().push(key);
        }
    }

    fn snapshot(&self, window: &str) -> Option<Vec<JoinRow>> {
        let win = self.windows.get(window)?;
        Some(
            win.rows
                .values()
                .flat_map(|rows| {
                    rows.iter()
                        .map(|(_, ev)| JoinRow::Event(Arc::new(ev.clone())))
                })
                .collect(),
        )
    }

    fn snapshot_with_timestamps(&self, window: &str) -> Option<Vec<(i64, JoinRow)>> {
        let win = self.windows.get(window)?;
        Some(
            win.rows
                .values()
                .flat_map(|rows| {
                    rows.iter()
                        .map(|(ts, ev)| (*ts, JoinRow::Event(Arc::new(ev.clone()))))
                })
                .collect(),
        )
    }

    fn join_lookup(&self, window: &str, key_field: &str, key: &Value) -> Option<Vec<JoinRow>> {
        let win = self.windows.get(window)?;
        if win.key_field != key_field {
            return None;
        }
        let key_repr = value_key_repr(key)?;
        win.rows.get(&key_repr).map(|rows| {
            rows.iter()
                .map(|(_, ev)| JoinRow::Event(Arc::new(ev.clone())))
                .collect()
        })
    }

    /// P3：deferred join 到期候选——该 key 的全部 (ts, row)。显式实现以
    /// 避免 trait 默认的全窗 `snapshot_with_timestamps` + 线性过滤（每次到期
    /// 评估 O(右窗全行)：Q8/Q9 对拍会退化到 O(评估次数 × 全窗) 卡死）。
    fn asof_candidates(
        &self,
        window: &str,
        key_field: &str,
        key: &Value,
    ) -> Option<Vec<(i64, JoinRow)>> {
        let win = self.windows.get(window)?;
        if win.key_field != key_field {
            return None;
        }
        let key_repr = value_key_repr(key)?;
        win.rows.get(&key_repr).map(|rows| {
            rows.iter()
                .map(|(ts, ev)| (*ts, JoinRow::Event(Arc::new(ev.clone()))))
                .collect()
        })
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
            if let Some(rows) = self.rows.get_mut(&key) {
                rows.retain(|(ts, _)| *ts > cutoff);
                if rows.is_empty() {
                    self.rows.remove(&key);
                }
            }
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

    fn asof_candidates(
        &self,
        window: &str,
        key_field: &str,
        key: &Value,
    ) -> Option<Vec<(i64, JoinRow)>> {
        self.asof_candidates(window, key_field, key)
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

/// 输出分派（2026-08-23 q13 双规则链中间管道）：sink 输出只计数；中间输出
/// （yield target 被下游 bind）计数（对齐引擎 emitted_total 含中间，q4a 修复
/// 同口径）**并**入队 feed 队列（转 Event 喂给下游规则）。
fn record_output(
    record: OutputRecord,
    alerts: &mut Vec<OracleAlert>,
    intermediate: &std::collections::HashSet<String>,
    feed: &mut Vec<(String, Event, i64)>,
) {
    if intermediate.contains(&*record.yield_target) {
        let ts = record.event_time_nanos;
        feed.push((
            record.yield_target.to_string(),
            record_to_event(&record),
            ts,
        ));
    }
    push_alert(record, alerts);
}

/// OutputRecord → Event（中间管道 feed）：yield 字段作为事件字段。
fn record_to_event(record: &OutputRecord) -> Event {
    let mut fields = EngineHashMap::default();
    for (name, value) in &record.yield_fields {
        fields.insert(name.to_string().into(), value.clone());
    }
    Event { fields }
}

/// 处理一个事件对单个引擎（each 或 match），返回输出记录（可能为 None）。
/// 复用主循环的 each/match 求值路径——中间管道 feed 与主循环同一条语义。
fn process_engine_event(
    engine: &mut RuleEngine,
    window: &str,
    ev: &Event,
    ts_nanos: i64,
) -> Option<OutputRecord> {
    let bind_aliases = engine.alias_map.get(window)?;
    if engine.each {
        let any_bind_matches = bind_aliases
            .iter()
            .any(|a| engine.executor.event_matches_alias(a, ev, None));
        if !any_bind_matches {
            return None;
        }
        if engine.has_joins {
            let lookup: &dyn WindowLookup = engine
                .lookup
                .as_ref()
                .map(|l| l as &dyn WindowLookup)
                .unwrap_or(&EmptyLookup);
            engine
                .executor
                .execute_each_with_joins(ev, ts_nanos, lookup, &[], ts_nanos)
                .ok()
                .flatten()
        } else {
            engine.executor.execute_each(ev, ts_nanos).ok().flatten()
        }
    } else {
        for bind_alias in bind_aliases {
            if !engine.executor.event_matches_alias(bind_alias, ev, None) {
                continue;
            }
            let windows = engine.lookup.as_ref().map(|l| l as &dyn WindowLookup);
            let result = engine
                .sm
                .advance_at_with_masks(bind_alias, ev, ts_nanos, windows, 0, None);
            if let StepResult::Matched(ctx) = result {
                let record = if engine.has_joins {
                    let lookup: &dyn WindowLookup = windows.unwrap_or(&EmptyLookup);
                    engine.executor.execute_match_with_joins(&ctx, lookup)
                } else {
                    engine.executor.execute_match(&ctx).map(Some)
                };
                if let Ok(Some(rec)) = record {
                    return Some(rec);
                }
            }
        }
        None
    }
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
