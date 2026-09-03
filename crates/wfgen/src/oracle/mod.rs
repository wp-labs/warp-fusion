#[cfg(test)]
mod tests;

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
    /// Evaluated `yield (...)` field values (name → formatted string), in
    /// yield-definition order. 2026-08-30: added for field-level detail diff
    /// (verify-nexmark --detail-diff vs engine benchmark.ndjson). Empty for
    /// oracle paths that do not evaluate yield fields yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<(String, String)>,
    /// 中间管道输出（yield target 被下游 bind，如 q4a/q13a）——不写引擎
    /// benchmark.ndjson，字段级明细对拍必须排除（2026-08-30）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub intermediate: bool,
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
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use chrono::{DateTime, Utc};
use wf_engine::alert::OutputRecord;
use wf_engine::match_engine::{
    CepStateMachine, CloseOutput, CloseReason, DeferredLeft, DeferredPending, EngineHashMap, Event,
    RuleExecutor, StatsExecutor, StepResult, Value, WindowLookup,
};
use wf_lang::WindowSchema;
use wf_lang::plan::{ConvPlan, RulePlan, WindowSpec};

use crate::datagen::stream_gen::GenEvent;
use crate::error::WfgenResult;

mod window;

use window::*;

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
    // Stats（`stats<...>`）规则 oracle 用 StatsExecutor 逐事件驱动（2026-08-27 接入）
    // ——fixed 窗口 bucket 对齐推进（同 StatsTask::advance_window）, 跨边界 close
    // 计数, 流末 close 全部尾部（对齐引擎 shutdown flush 的确定性收口）。
    // session/sliding 窗口（stats P2/P3 范围）不接入, 跳过计数。
    let mut skipped_stats = 0usize;
    let mut engines: Vec<RuleEngine> = Vec::new();
    let mut stats_engines: Vec<StatsOracleEngine> = Vec::new();
    for plan in rule_plans.iter().filter(|plan| {
        injected_rules
            .map(|set| set.contains(&plan.name))
            .unwrap_or(true)
    }) {
        if let Some(stats_plan) = &plan.stats_plan {
            // 仅 fixed 窗口可 oracle（bucket 对齐推进）; session/sliding 跳过。
            if matches!(stats_plan.window_spec, WindowSpec::Fixed(_)) {
                stats_engines.push(StatsOracleEngine::new(plan));
            } else {
                skipped_stats += 1;
            }
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
        // 经 progress::note 先清进度条当前帧再打印，避免粘在 "... ETA 0s" 帧尾
        crate::progress::note(&format!(
            "oracle: 跳过 {} 个 stats 规则（StatsExecutor 暂未接入 oracle）",
            skipped_stats
        ));
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

        // stats 规则（fixed 窗口）: 逐事件驱动窗口推进 + 归并。行式路径——
        // oracle 逐事件无批可列式化; 计数口径 = 引擎（close 桶 × n_records）。
        // **按绑定窗口喂行**（2026-08-27 review）: 引擎 StatsTask 只消费规则的
        // `window_sources`（= binds），oracle 若喂全部原始流——空键 + 无 where 度量
        // 会被非绑定流行虚增（q15 `total` 10M 计成 1000 万而非 920 万）; 键式规则
        // 靠键缺失跳过掩幸, 空键规则值全错且 verify 只对拍计数测不出。
        if stats_engines
            .iter()
            .any(|se| se.accepts(&event.window_name))
        {
            let row = gen_event_to_row(&event);
            for se in &mut stats_engines {
                if se.accepts(&event.window_name) {
                    alerts.extend(se.feed(event_nanos, &row));
                }
            }
        }

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
                // 2026-08-30 修复（q7）：**有 conv 的 fixed 规则也用无界预算**——
                // 引擎的 conv 在 conv_sink 任务层跨批聚合（同桶全部 close 一次
                // top_ties），oracle 无 conv_sink，若用 1024 预算拆批 → 每批
                // 单独 top_ties(1) → 同桶多输出（q7 桶[0,10) 输出 5 条 vs 引擎
                // 1 条，oracle 把非最高价 auction 也输出了）。
                let expired = if engine.hop_unbounded || engine.conv_plan.is_some() {
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
                        if let Some(p) = engine.executor.deferred_pending_for(
                            join_idx,
                            &DeferredLeft::Event(core_event.clone()),
                            event_nanos,
                        ) {
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
                            // 2026-08-27 review: 此前直接 push_alert——deferred 的中间
                            // 输出（q4a→auction_finals）从不入 feed_queue, 下游规则
                            // （q4b stats 绑定 auction_finals）收不到; 当时无 CEP 规则
                            // 消费该中间窗口故未暴露。改为 record_output（= push_alert
                            // + 中间窗口入队）, 与 each/match 中间输出同路径。
                            record_output(
                                record,
                                &mut alerts,
                                &intermediate_windows,
                                &mut feed_queue,
                            );
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
            // 中间事件也须喂给绑定该中间窗口的 stats 引擎（2026-08-27 review）:
            // q4a→auction_finals→q4b 双规则链——否则 q4b oracle 只看到原始流
            // auction_events（auction 行有 category 建同名桶, 桶数巧合一致但
            // avg(f.final) 缺 final 字段贡献为 0, 值全错）。
            if stats_engines.iter().any(|se| se.accepts(&window)) {
                let row = core_event_to_row(&ev);
                for se in &mut stats_engines {
                    if se.accepts(&window) {
                        alerts.extend(se.feed(ts, &row));
                    }
                }
            }
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
        let expired = if engine.hop_unbounded || engine.conv_plan.is_some() {
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
        if close_at_eos && let Some(deferred) = &mut engine.deferred {
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

    // stats 规则流末收口: 引擎 replay 的 shutdown flush 确定性 close 尾部窗口
    // （emitted_total 含尾部桶）——oracle 无条件 close 剩余（无论 close_at_eos,
    // 与 CEP 的 replay 不推进 EOS 语义区分——stats 引擎 flush 必然收口）。
    for se in &mut stats_engines {
        alerts.extend(se.close_tail());
    }

    Ok(OracleResult { alerts })
}

/// oracle 的 stats 规则引擎（2026-08-27 接入）: [`StatsExecutor`] 事件驱动——
/// fixed 窗口 bucket 对齐推进（与 `StatsTask::advance_window` 同口径: 事件时间
/// 越过 window_end 即 close 并开下一窗）。**按窗口批量归并**: 行缓冲到窗口边界
/// 才一次 [`StatsExecutor::process_rows`]（`process_rows` 批末做
/// `refresh_estimated_bytes` O(桶数) 遍历——逐事件调用会 O(事件×桶) 爆炸）。
/// close 计数 = 引擎口径（每桶 × n_records, top 度量多条目）。流末
/// [`StatsOracleEngine::close_tail`] 收口剩余窗口（引擎 shutdown flush 确定性
/// 收口, emitted_total 含尾部桶）。
///
/// session/sliding 窗口（stats P2/P3）不接入（oracle 仅 fixed 推进）。
///
/// 窗口内行缓冲容量阈值: 达到即分批归并进桶状态（`StatsExecutor::process_rows`
/// 批末 `refresh_estimated_bytes` 是 O(桶数) 遍历——每 10 万行一次摊薄可接受;
/// 若不设阈值, 1d 窗口会把整窗行（10m 数据 ~920 万行 HashMap ≈ 1-2GB）全压在
/// pending 里 → OOM（2026-08-27 实测 Killed: 9）。
const FLUSH_PENDING_ROWS: usize = 100_000;

struct StatsOracleEngine {
    name: String,
    stats: StatsExecutor,
    /// 绑定源窗口（= plan.binds; 引擎 StatsTask 的 window_sources 同源）——
    /// oracle 只喂这些窗口的行（含中间窗口事件）, 对齐引擎不喂非绑定流。
    bound_windows: std::collections::HashSet<String>,
    /// 当前窗口上界（None = 尚未见事件）; bucket 对齐 `(t / dur) * dur + dur`。
    window_end: Option<i64>,
    entity_type: String,
    score: f64,
    /// 当前窗口累积行缓冲（窗口边界批量归并, 免逐事件 process_rows 的
    /// refresh_estimated_bytes O(桶数) 开销）。
    pending: Vec<HashMap<String, Value>>,
}

impl StatsOracleEngine {
    fn new(plan: &RulePlan) -> Self {
        let stats_plan = plan.stats_plan.as_ref().expect("stats 规则").clone();
        let score = match &plan.score_plan.expr {
            wf_lang::ast::Expr::Number(n) => *n,
            _ => 10.0, // 非数字 score（罕见）: 计数不受影响
        };
        let bound_windows = plan
            .binds
            .iter()
            .map(|b| b.window.clone())
            .collect::<std::collections::HashSet<_>>();
        Self {
            name: plan.name.clone(),
            stats: StatsExecutor::with_row_fields(stats_plan, None),
            bound_windows,
            window_end: None,
            entity_type: plan.entity_plan.entity_type.clone(),
            score,
            pending: Vec::new(),
        }
    }

    /// 本引擎是否消费该窗口（绑定窗口匹配; 未绑定 = 不喂, 对齐引擎 window_sources）。
    fn accepts(&self, window_name: &str) -> bool {
        self.bound_windows.contains(window_name)
    }

    /// 喂一个事件: 窗口推进（跨边界先批量归并缓冲 + close）+ 缓冲本事件。
    /// 返回本事件触发的 close alerts。
    fn feed(&mut self, event_nanos: i64, row: &HashMap<String, Value>) -> Vec<OracleAlert> {
        let mut alerts = Vec::new();
        let dur = match self.stats.plan.window_spec {
            WindowSpec::Fixed(d) => d.as_nanos() as i64,
            _ => return alerts, // 非 fixed 不应构造（调用方已过滤）
        };
        if self.window_end.is_none() {
            self.window_end = Some(((event_nanos / dur) * dur) + dur);
        }
        while let Some(end) = self.window_end {
            if event_nanos < end {
                break;
            }
            // 本事件已越过边界: 先归并缓冲（旧窗口行）+ close, 再开下一窗。
            self.flush_pending();
            alerts.extend(self.close_window(end));
            self.window_end = Some(((event_nanos / dur) * dur) + dur);
        }
        self.pending.push(row.clone());
        // 容量阈值分批归并（2026-08-27 OOM 修复）: 1d 窗口下 pending 会累积整窗
        // 行（10m 数据 ~920 万行 HashMap ≈ 1-2GB）——按阈值提前归并进桶状态
        // （同窗口行分批归并安全, 桶状态累积; close 时才收口输出）。
        // process_rows 批末 refresh O(桶数)——每 10 万行一次, 摊薄可接受。
        if self.pending.len() >= FLUSH_PENDING_ROWS {
            self.flush_pending();
        }
        alerts
    }

    /// 批量归并当前窗口缓冲（一次 process_rows——批末 refresh O(桶数) 摊到整窗）。
    fn flush_pending(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let rows = std::mem::take(&mut self.pending);
        self.stats
            .process_rows(&rows, |r, name| r.get(name).cloned());
    }

    /// close 当前窗口: 先归并缓冲, 再输出桶（标量每桶 1 alert; top 每桶 n_records）。
    fn close_window(&mut self, window_end: i64) -> Vec<OracleAlert> {
        self.flush_pending();
        let buckets = self.stats.close_window_by_bucket_rows();
        let mut alerts = Vec::with_capacity(buckets.len());
        let emit_time = chrono::DateTime::from_timestamp_nanos(window_end).to_rfc3339();
        for b in &buckets {
            let n_records = b.measures.iter().map(Vec::len).max().unwrap_or(1);
            for _ in 0..n_records {
                alerts.push(OracleAlert {
                    rule_name: self.name.clone(),
                    score: self.score,
                    entity_type: self.entity_type.clone(),
                    entity_id: format!("{:?}", b.key),
                    origin: "close".to_string(),
                    emit_time: emit_time.clone(),
                    // 2026-08-30: stats 路径的 yield 字段求值待接入
                    // （AlertColumnBuilder 复用 execute_stats_close_batch_columnar）——
                    // 暂空 → 明细对拍跳过 stats 规则（计数对拍 + CHECKS 覆盖）。
                    fields: Vec::new(),
                    intermediate: false,
                });
            }
        }
        alerts
    }

    /// 流末收口剩余窗口（引擎 shutdown flush 同语义）。
    fn close_tail(&mut self) -> Vec<OracleAlert> {
        match self.window_end {
            Some(end) => self.close_window(end),
            None => Vec::new(), // 无事件: 无窗口
        }
    }
}

/// GenEvent → stats 行（HashMap<String, Value>）; 字段经 json_to_core_value
/// 转引擎 Value（数字 f64 / 字符串 / 布尔; 复合类型丢弃——stats 度量不读）。
fn gen_event_to_row(event: &GenEvent) -> HashMap<String, Value> {
    let mut row = HashMap::new();
    for (k, v) in &event.fields {
        if let Some(core_v) = json_to_core_value(v) {
            row.insert(k.clone(), core_v);
        }
    }
    row
}

/// 中间管道事件（yield 字段已是引擎 Value）→ stats 行。
fn core_event_to_row(event: &Event) -> HashMap<String, Value> {
    event
        .fields
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

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

fn push_alert(alert_record: wf_engine::alert::OutputRecord, alerts: &mut Vec<OracleAlert>) {
    alerts.push(OracleAlert {
        rule_name: alert_record.rule_name.to_string(),
        score: alert_record.score,
        entity_type: alert_record.entity_type.to_string(),
        entity_id: alert_record.entity_id,
        origin: alert_record.origin.as_str().to_string(),
        emit_time: alert_record.fired_at.clone(),
        fields: yield_fields(&alert_record.yield_fields),
        intermediate: false,
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
    let is_inter = intermediate.contains(&*record.yield_target);
    if is_inter {
        let ts = record.event_time_nanos;
        feed.push((
            record.yield_target.to_string(),
            record_to_event(&record),
            ts,
        ));
    }
    alerts.push(OracleAlert {
        rule_name: record.rule_name.to_string(),
        score: record.score,
        entity_type: record.entity_type.to_string(),
        entity_id: record.entity_id,
        origin: record.origin.as_str().to_string(),
        emit_time: record.fired_at.clone(),
        fields: yield_fields(&record.yield_fields),
        intermediate: is_inter,
    });
}

/// OutputRecord → Event（中间管道 feed）：yield 字段作为事件字段。
fn record_to_event(record: &OutputRecord) -> Event {
    let mut fields = EngineHashMap::default();
    for (name, value) in &record.yield_fields {
        fields.insert(name.to_string().into(), value.clone());
    }
    Event { fields }
}

/// OutputRecord.yield_fields → (名, 格式化值) 列表（字段级明细对拍用；顺序 =
/// yield 定义序）。Value → 字符串与引擎 file_json_sink 的模型值输出同构：
/// Number 原样（f64 → 尽量整数打印）、Str 原样、Bool true/false。
fn yield_fields(
    fields: &[(std::sync::Arc<str>, wf_engine::match_engine::Value)],
) -> Vec<(String, String)> {
    fields
        .iter()
        .map(|(name, value)| (name.to_string(), format_yield_value(value)))
        .collect()
}

/// Value → 对拍用字符串（引擎 JSON 输出的模型值同构）。
pub fn format_yield_value(v: &wf_engine::match_engine::Value) -> String {
    match v {
        wf_engine::match_engine::Value::Number(n) => format_f64(*n),
        wf_engine::match_engine::Value::Str(s) => s.to_string(),
        wf_engine::match_engine::Value::Bool(b) => b.to_string(),
        wf_engine::match_engine::Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(format_yield_value).collect();
            format!("[{}]", parts.join(","))
        }
        wf_engine::match_engine::Value::Object(m) => {
            let mut parts: Vec<String> = m
                .iter()
                .map(|(k, v)| format!("{k}={}", format_yield_value(v)))
                .collect();
            parts.sort();
            format!("{{{}}}", parts.join(","))
        }
    }
}

/// f64 → 对拍字符串：整数精度时打印整数（对齐 JSON 序列化的 Number 输出）。
pub fn format_f64(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
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
                fields: yield_fields(&alert_record.yield_fields),
                intermediate: false,
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
