//! NEXMark 引擎结果验证：用**真实 WFL 规则引擎**（wf_engine，经 oracle 管线）
//! 处理 wfgen 生成的事件，产出各规则应 EMIT 计数（JSON，按 .wfl 规则名），
//! 供与引擎 daemon 实际 EMIT 对拍（bench.sh --verify）。
//!
//! 替代旧手写 Sim（Q2-Q21 模拟器）：Sim 是独立实现、语义与引擎漂移
//! （理想/朴素值 → q16/q21 已知边界、±0.5% 容差带、q9→q3 映射 hack
//! 均源于此）。规则引擎与 daemon 跑同一套 .wfl 规则、同一份数据，EMIT
//! 数应完全一致——差异即引擎实现缺陷，也顺带覆盖 q1/q11/q12/q14/q22
//! （Sim 未建模的规则）。
//!
//! 事件流：`generate_events(count, seed)` 确定性产出，与 `gen-nexmark`
//! 输出逐字节一致（同 rng 序列）；按与 gen-nexmark 相同的 30s 桶序喂
//! 规则引擎——与 daemon 收到的帧序一致，窗口过期语义对拍才成立。
//!
//! 用法：`wfgen verify-nexmark <count> [--seed N] [--rules-dir models/queries]
//! [--schemas models/schemas/nexmark.wfs]`（默认指向 nexmark_pk 布局）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;

use chrono::{DateTime, Utc};
use serde_json::json;

use crate::cmd_gen_nexmark::{NxEvent, generate_events, nx_to_value};
use crate::cmd_helpers::{load_wfl_files, load_ws_files};
use crate::datagen::stream_gen::GenEvent;
use crate::error::{WfgenReason, WfgenResult};
use crate::oracle::run_oracle_events_full;

const BASE_NS: i64 = 1767225600000000000; // 与 cmd_gen_nexmark::BASE_NS 一致（2026-01-01T00:00:00Z）
const BUCKET_NS: i64 = 30_000_000_000;
const BUCKETS: usize = 60;

/// 与 `gen-nexmark` 相同的 30s 桶序：事件按桶收集（桶内生成序），再按桶序
/// 喂规则引擎——与 daemon 收到的帧序一致。
struct NxData {
    buckets: Vec<Vec<NxEvent>>,
    n_person: i64,
    n_auction: i64,
    n_bid: i64,
}

fn collect_buckets(count: i64, seed: u64) -> WfgenResult<NxData> {
    let mut buckets: Vec<Vec<NxEvent>> = (0..BUCKETS).map(|_| Vec::new()).collect();
    let mut n_person = 0i64;
    let mut n_auction = 0i64;
    let mut n_bid = 0i64;
    let pb = crate::progress::ProgressBar::new(count as u64, "verify: 生成事件");
    generate_events(count, seed, |ev| {
        pb.tick();
        let ns = ev.ns();
        match ev {
            NxEvent::Person { .. } => n_person += 1,
            NxEvent::Auction { .. } => n_auction += 1,
            NxEvent::Bid { .. } => n_bid += 1,
        }
        let b = (((ns - BASE_NS).max(0)) / BUCKET_NS).min((BUCKETS - 1) as i64) as usize;
        buckets[b].push(ev);
        Ok(())
    })?;
    pb.finish();
    Ok(NxData {
        buckets,
        n_person,
        n_auction,
        n_bid,
    })
}

/// NxEvent → oracle 的 GenEvent（字段即 gen-nexmark 输出的 JSON 字段，
/// 时间戳保留 ns 精度，与 daemon 的窗口时间字段一致）。
fn nx_to_gen_event(ev: &NxEvent) -> GenEvent {
    let ns = ev.ns();
    let ts = DateTime::<Utc>::from_timestamp(
        ns.div_euclid(1_000_000_000),
        ns.rem_euclid(1_000_000_000) as u32,
    )
    .unwrap_or_default();
    GenEvent {
        stream_name: ev.stream().to_string(),
        window_name: ev.stream().to_string(),
        timestamp: ts,
        fields: nx_to_value(ev).as_object().cloned().unwrap_or_default(),
    }
}

pub fn run(
    count: i64,
    seed: u64,
    rules_dir: PathBuf,
    schemas: PathBuf,
    query: Option<String>,
    engine_emit: Option<PathBuf>,
) -> WfgenResult<()> {
    // 1) 生成并分桶（确定性事件流，与 gen-nexmark 一致；桶序 = daemon 输入序）
    let data = collect_buckets(count, seed)?;

    // 2) 加载并编译 NEXMark 规则（models/queries/*.wfl + nexmark.wfs）
    let schemas_list = load_ws_files(&[schemas])?;
    let mut wfl_paths: Vec<PathBuf> = std::fs::read_dir(&rules_dir)
        .map_err(|e| {
            crate::error::error(
                WfgenReason::Io,
                format!("read rules dir {}: {e}", rules_dir.display()),
            )
        })?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "wfl").unwrap_or(false))
        .collect();
    wfl_paths.sort();
    // --query qN：只验证该查询的规则文件（bench 单查询验证提速：26 规则 → 1 个文件）
    if let Some(q) = &query {
        wfl_paths.retain(|p| p.file_stem().map(|s| s == q.as_str()).unwrap_or(false));
    }
    if wfl_paths.is_empty() {
        return crate::error::fail(
            WfgenReason::Validation,
            format!(
                "no .wfl rules found in {} {}",
                rules_dir.display(),
                query
                    .as_deref()
                    .map(|q| format!("for query '{q}'"))
                    .unwrap_or_default()
            ),
        );
    }
    let wfl_files = load_wfl_files(&wfl_paths)?;
    let mut rule_plans = Vec::new();
    for f in &wfl_files {
        match wf_lang::compile_wfl(f, &schemas_list) {
            Ok(plans) => rule_plans.extend(plans),
            Err(e) => {
                return crate::error::fail(
                    WfgenReason::Validation,
                    format!("WFL compilation failed: {}", e.report().render()),
                );
            }
        }
    }
    eprintln!(
        "verify: {} 规则已编译（{} 个 .wfl 文件）",
        rule_plans.len(),
        wfl_paths.len()
    );

    // 3) 并行：规则分组，各自流式跑真实规则引擎（共享分桶；同一桶序）
    let start =
        DateTime::<Utc>::from_timestamp(BASE_NS.div_euclid(1_000_000_000), 0).unwrap_or_default();
    let duration = std::time::Duration::from_secs(30 * 60);
    let n_threads = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(rule_plans.len())
        .max(1);
    let pb = crate::progress::ProgressBar::new(count as u64, "verify: 规则引擎");
    let counter = pb.counter();
    let buckets = Arc::new(data.buckets);
    let schemas_arc = Arc::new(schemas_list);
    let alerts = thread::scope(|scope| {
        let chunk = (rule_plans.len() + n_threads - 1) / n_threads;
        let handles: Vec<_> = rule_plans
            .chunks(chunk)
            .enumerate()
            .map(|(i, group)| {
                let plans: Vec<_> = group.to_vec();
                let buckets = Arc::clone(&buckets);
                let counter = Arc::clone(&counter);
                let schemas = Arc::clone(&schemas_arc);
                scope.spawn(move || {
                    // 进度条只由第一个规则组线程驱动（各组消费同一桶流，进度同步）
                    let tick = i == 0;
                    let events = buckets.iter().flat_map(|b| {
                        b.iter().map(nx_to_gen_event).inspect(|_| {
                            if tick {
                                counter.fetch_add(1, Ordering::Relaxed);
                            }
                        })
                    });
                    // 传 schemas：join 目标窗口的 over 保留 → oracle 维护窗口状态
                    // （join-then-key / match 时 join 对拍的前提）。
                    run_oracle_events_full(events, &plans, &schemas, &start, &duration, None, false)
                        .map(|r| r.alerts)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("verify rule thread panicked"))
            .collect::<WfgenResult<Vec<_>>>()
    })?;
    pb.finish();

    // 4) 合并：按规则名统计应 EMIT 数（规则分组不相交，直接累加）
    let mut counts: HashMap<String, u64> = HashMap::new();
    for group_alerts in alerts {
        for a in group_alerts {
            *counts.entry(a.rule_name).or_insert(0) += 1;
        }
    }

    let mut rules: Vec<_> = counts.iter().collect();
    rules.sort_by(|a, b| a.0.cmp(b.0));

    // 5) 输出：--engine-emit 时打印人类可读 oracle 摘要 + git-diff 式对拍报告
    //    （原始 JSON 只在纯 oracle 模式输出，机器可读）；退出码 0=一致 / 1=有差异
    //    （q21 已知差异不判失败）。
    if let Some(emit_path) = engine_emit {
        let engine_counts = read_engine_emits(&emit_path)?;
        println!(
            "== oracle（真实规则引擎 · {} 事件 · seed {seed}）==",
            crate::progress::fmt_num(count as u64)
        );
        println!(
            "  数据  person {} / auction {} / bid {}",
            crate::progress::fmt_num(data.n_person as u64),
            crate::progress::fmt_num(data.n_auction as u64),
            crate::progress::fmt_num(data.n_bid as u64),
        );
        for (rule, n) in &rules {
            println!("  规则  {rule:<24} {:>15}", crate::progress::fmt_num(**n));
        }
        let (gt_lines, engine_lines, known_report) = normalize_counts(&counts, &engine_counts);
        for line in known_report {
            println!("{line}");
        }
        let gt_refs: Vec<&str> = gt_lines.iter().map(String::as_str).collect();
        let engine_refs: Vec<&str> = engine_lines.iter().map(String::as_str).collect();
        if !crate::cmd_diff::compare_lines(&gt_refs, &engine_refs, true) {
            std::process::exit(1);
        }
    } else {
        // 纯 oracle 模式：原始 JSON（机器可读，供外部消费）
        let mut out = serde_json::Map::new();
        for (rule, n) in &rules {
            out.insert((*rule).clone(), json!(n));
        }
        out.insert(
            "_counts".into(),
            json!({"persons": data.n_person, "auctions": data.n_auction, "bids": data.n_bid}),
        );
        println!("{}", serde_json::Value::Object(out));
    }
    Ok(())
}

/// 读引擎侧 EMIT 计数：`--engine-emit` 指向目录时扫描 `bench_*_replay.txt`
/// （bench.sh 结果文件；warmup/stream 文件不在其列），指向单文件时直接读。
/// 每行格式 `EMIT <规则名> <计数>`，后到覆盖（多轮跑批同规则取最后值）。
fn read_engine_emits(path: &std::path::Path) -> WfgenResult<HashMap<String, u64>> {
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    if path.is_dir() {
        for entry in std::fs::read_dir(path).map_err(|e| {
            crate::error::error(WfgenReason::Io, format!("read dir {}: {e}", path.display()))
        })? {
            let p = entry
                .map_err(|e| crate::error::error(WfgenReason::Io, format!("read dir entry: {e}")))?
                .path();
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned());
            if name
                .as_deref()
                .is_some_and(|n| n.starts_with("bench_") && n.ends_with("_replay.txt"))
            {
                files.push(p);
            }
        }
    } else {
        files.push(path.to_path_buf());
    }
    files.sort();

    let mut counts: HashMap<String, u64> = HashMap::new();
    for f in files {
        let text = std::fs::read_to_string(&f).map_err(|e| {
            crate::error::error(WfgenReason::Io, format!("read {}: {e}", f.display()))
        })?;
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("EMIT ") {
                if let Some((rule, n)) = rest.rsplit_once(' ') {
                    if let Ok(n) = n.trim().parse::<u64>() {
                        counts.insert(rule.to_string(), n);
                    }
                }
            }
        }
    }
    Ok(counts)
}

/// 归一化两侧为同序文本行 `规则名 计数`（Myers 才能对齐）：oracle 取全部规则
/// （跳过 _ 前缀元数据），引擎侧只取 oracle 覆盖的规则（单查询验证时其它查询
/// 残留 EMIT 是历史噪音）。已知差异规则（oracle 与引擎实现面不同，见下）
/// 剔除后单独报告，不判失败。
///
/// 已知差异两类：
/// - JOIN_VISIBILITY_DIFF（q6）：join 键规则，引擎 replay 的 join 可见性受
///   append 超前 + evictor sweep 时机影响（非确定），oracle（预加载 + 事件
///   时间过期）为语义正确参考值；
/// - CLOSE_BUDGET_DIFF（q4/q9/q16）：fixed+`and close` 规则，引擎热路径收口
///   每批预算 1024 + 尾桶收口依赖墙钟 scan_timeouts（快速 replay 可能不触发
///   → 丢尾部收口，引擎自身非确定；oracle 为“所有窗口最终收口”的理想值）。
/// （q21 anti-join 已于 2026-08-21 随 oracle join 窗口状态实现解决——oracle 与
/// 引擎均全 drop，对拍一致，不再列 known。）
fn normalize_counts(
    oracle: &HashMap<String, u64>,
    engine: &HashMap<String, u64>,
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let known: &[(&str, &str)] = &[
        (
            "q6_avg_price_by_seller",
            "join 可见性：引擎 replay 的 append 超前/evictor sweep 时机非确定（oracle 为语义参考值）",
        ),
        (
            "q9_winning_bid",
            "fixed+close 收口预算/scan_timeouts 时钟相关，引擎可能丢尾部收口（oracle 理想值）",
        ),
        (
            "q4_avg_price_by_category",
            "fixed+close 收口预算/scan_timeouts 时钟相关（另叠加 join 可见性），引擎可能丢尾部收口（oracle 理想值）",
        ),
        (
            "q16_sum_price_1000",
            "fixed+close 收口预算/scan_timeouts 时钟相关，引擎可能丢尾部收口（oracle 理想值）",
        ),
    ];
    let mut rules: Vec<&String> = oracle
        .keys()
        .filter(|r| !r.starts_with('_') && !known.iter().any(|(k, _)| *k == r.as_str()))
        .collect();
    rules.sort();
    let gt_lines: Vec<String> = rules
        .iter()
        .map(|r| format!("{r} {}", oracle[r.as_str()]))
        .collect();
    let engine_lines: Vec<String> = rules
        .iter()
        .filter(|r| engine.contains_key(r.as_str()))
        .map(|r| format!("{r} {}", engine[r.as_str()]))
        .collect();
    let mut known_report: Vec<String> = Vec::new();
    for (rule, reason) in known {
        if let (Some(ov), Some(ev)) = (oracle.get(*rule), engine.get(*rule)) {
            known_report.push(format!(
                "  {rule}: oracle={ov} 引擎={ev}  ⚠ 已知差异（{reason}）"
            ));
        }
    }
    (gt_lines, engine_lines, known_report)
}
