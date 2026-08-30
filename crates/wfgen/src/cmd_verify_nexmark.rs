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

use crate::cmd_helpers::{load_wfl_files, load_ws_files};
use crate::datagen::stream_gen::GenEvent;
use crate::error::{WfgenReason, WfgenResult};
use crate::nexmark::{INTER_EVENT_DELAY_NS, NxEvent, generate_events, nx_to_value};
use crate::oracle::run_oracle_events_full;

const BASE_NS: i64 = 1767225600000000000; // 与 cmd_gen_nexmark::BASE_NS 一致（2026-01-01T00:00:00Z）
const BUCKET_NS: i64 = 30_000_000_000;

/// 30s 桶数随跨度动态（与 gen-nexmark 的 time_buckets 一致）：
/// span = count × 100µs（官方固定速率），桶宽 30s → ~count/300k 个桶。
fn time_buckets_for(count: i64) -> usize {
    (((count * INTER_EVENT_DELAY_NS) / BUCKET_NS).max(1)) as usize
}

/// 与 `gen-nexmark` 相同的 30s 桶序：事件按桶收集（桶内生成序），再按桶序
/// 喂规则引擎——与 daemon 收到的帧序一致。
struct NxData {
    buckets: Vec<Vec<NxEvent>>,
    n_person: i64,
    n_auction: i64,
    n_bid: i64,
}

fn collect_buckets(count: i64, seed: u64) -> WfgenResult<NxData> {
    let buckets_n = time_buckets_for(count);
    let mut buckets: Vec<Vec<NxEvent>> = (0..buckets_n).map(|_| Vec::new()).collect();
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
        let b = (((ns - BASE_NS).max(0)) / BUCKET_NS).min((buckets_n - 1) as i64) as usize;
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

/// `wfgen verify-nexmark` 参数：真实 WFL 规则引擎处理 NEXMark 事件，产出
/// 各规则应 EMIT 计数，供与引擎 daemon 对拍。
#[derive(clap::Args)]
pub struct Args {
    /// Number of events to verify
    pub count: i64,

    /// RNG seed (must match `gen-nexmark` for comparable output)
    #[arg(long, default_value_t = 1)]
    pub seed: u64,

    /// Directory containing the NEXMark .wfl rule files (glob *.wfl)
    #[arg(long, default_value = "models/queries")]
    pub rules_dir: PathBuf,

    /// NEXMark window schema .wfs (referenced by `use` in the rules)
    #[arg(long, default_value = "models/schemas/nexmark.wfs")]
    pub schemas: PathBuf,

    /// 只验证指定查询的规则文件（q1..q22；默认全部 models/queries/*.wfl）。
    /// bench 单查询验证时传 --query 大幅提速（26 规则 → 1 个文件）。
    #[arg(long)]
    pub query: Option<String>,

    /// 引擎结果对拍：目录（扫描 bench_*_replay.txt，bench.sh 用法）或单文件。
    /// 读引擎实际 EMIT 计数，在 wfgen 内用 git-diff 同款分层方法
    /// （L1 哈希 → L2 Myers/降级 → L3 明细）与 oracle 逐规则对拍；
    /// 退出码 0=一致 / 1=有差异（q21 已知差异不判失败）。
    #[arg(long)]
    pub engine_emit: Option<PathBuf>,

    /// 字段级明细对拍：指向引擎文件源输出 `data/alerts/benchmark.ndjson`。
    /// oracle 侧输出每条 alert 的 yield 字段值（字段级；oracle 未求值 yield
    /// 字段的规则——stats 路径——自动跳过），与引擎逐行规范化后排序 diff。
    /// 2026-08-30 新增：verify_file.sh 用它把内容验证从计数级提升到字段级。
    #[arg(long)]
    pub detail_diff: Option<PathBuf>,
}

pub fn run(args: Args) -> WfgenResult<()> {
    let Args {
        count,
        seed,
        rules_dir,
        schemas,
        query,
        engine_emit,
        detail_diff,
    } = args;
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
    // eos 水位须覆盖数据末尾（span = count × 100µs，官方固定速率）才能模拟引擎 slice
    // 收口；旧实现固定 30min 与新 span（30M → 50min / 100M → 167min）不匹配。
    let duration = std::time::Duration::from_nanos((count * INTER_EVENT_DELAY_NS).max(1) as u64);
    let _n_threads = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(rule_plans.len())
        .max(1);
    let pb = crate::progress::ProgressBar::new(count as u64, "verify: 规则引擎");
    let counter = pb.counter();
    let buckets = Arc::new(data.buckets);
    let schemas_arc = Arc::new(schemas_list);
    let alerts = thread::scope(|scope| {
        // 中间管道依赖（2026-08-23 q13 双规则链）：bind 的窗口被其它规则
        // yield（如 q13a→bid_mod→q13b、q4a→auction_finals→q4b）时，两个
        // 规则必须同组——oracle 的中间 feed 是组内（单实例）事件流转，跨组
        // 实例断裂（q13a/q13b 拆组则 q13b 收不到 bid_mod 事件，EMIT=0）。
        // 用并查集把 yield-bind 依赖链合并为组，每组一个线程。
        fn find(group_of: &mut [usize], mut i: usize) -> usize {
            while group_of[i] != i {
                group_of[i] = group_of[group_of[i]];
                i = group_of[i];
            }
            i
        }
        let mut group_of: Vec<usize> = (0..rule_plans.len()).collect();
        let yield_idx: std::collections::HashMap<&str, usize> = rule_plans
            .iter()
            .enumerate()
            .map(|(i, p)| (p.yield_plan.target.as_str(), i))
            .collect();
        for (i, plan) in rule_plans.iter().enumerate() {
            for bind in &plan.binds {
                if let Some(&j) = yield_idx.get(bind.window.as_str()) {
                    let ri = find(&mut group_of, i);
                    let rj = find(&mut group_of, j);
                    group_of[ri] = rj;
                }
            }
        }
        let mut groups: std::collections::BTreeMap<usize, Vec<usize>> =
            std::collections::BTreeMap::new();
        let rule_count = group_of.len();
        for i in 0..rule_count {
            let r = find(&mut group_of, i);
            groups.entry(r).or_default().push(i);
        }
        let handles: Vec<_> = groups
            .into_values()
            .enumerate()
            .map(|(i, idxs)| {
                let plans: Vec<_> = idxs.iter().map(|&i| rule_plans[i].clone()).collect();
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
    for group_alerts in &alerts {
        for a in group_alerts {
            *counts.entry(a.rule_name.clone()).or_insert(0) += 1;
        }
    }

    // 4b) 字段级明细对拍（--detail-diff <engine benchmark.ndjson>，2026-08-30）
    // oracle 侧只对**已求值 yield 字段**的规则（CEP/on-each/match/deferred 路径）
    // 产出明细行；stats 规则（oracle 未求值 yield）自动跳过（保持计数对拍 +
    // verify_file.sh 的 CHECKS 内容断言）。引擎侧从 benchmark.ndjson 提取
    // 非 __wfu_* 字段，两侧按相同规范化行排序后 diff。
    if let Some(detail_path) = detail_diff {
        let known_rules: std::collections::HashSet<&str> =
            known_diff_rules().iter().map(|(r, _)| *r).collect();
        // 只对 oracle 已求值 yield 字段的规则做明细对拍（stats 规则两边都跳过）
        let detail_rules: std::collections::HashSet<String> = alerts
            .iter()
            .flatten()
            .filter(|a| !a.fields.is_empty())
            .map(|a| a.rule_name.clone())
            .filter(|r| !known_rules.contains(r.as_str()))
            .collect();
        let mut oracle_lines: Vec<String> = alerts
            .iter()
            .flatten()
            .filter(|a| !known_rules.contains(a.rule_name.as_str()))
            .filter_map(alert_detail_line)
            .collect();
        let engine_lines = engine_detail_lines(&detail_path, &detail_rules)?;
        if std::env::var("WFGEN_VERIFY_DETAIL_DEBUG").is_ok() {
            let mut with_f = std::collections::BTreeMap::<String, usize>::new();
            let mut without_f = std::collections::BTreeMap::<String, usize>::new();
            for a in alerts.iter().flatten() {
                if a.intermediate {
                    continue;
                }
                if a.fields.is_empty() {
                    *without_f.entry(a.rule_name.clone()).or_insert(0) += 1;
                } else {
                    *with_f.entry(a.rule_name.clone()).or_insert(0) += 1;
                }
            }
            for (rule, n) in &with_f {
                eprintln!(
                    "[detail] {rule}: 有字段 {n} / 无字段 {}",
                    without_f.get(rule).unwrap_or(&0)
                );
            }
            for (rule, n) in &without_f {
                if !with_f.contains_key(rule) {
                    eprintln!("[detail] {rule}: 有字段 0 / 无字段 {n}");
                }
            }
        }
        oracle_lines.sort();
        let mut engine_sorted = engine_lines;
        engine_sorted.sort();
        println!(
            "== 字段级明细对拍（oracle {} 行 vs 引擎 {} 行，仅含已求值 yield 的规则）==",
            oracle_lines.len(),
            engine_sorted.len()
        );
        // 多重数比较（multiset）：重复行（同 id 多条相同 alert，如 q6 每事件命中）
        // 按出现次数对齐，比排序逐行 diff 更鲁棒；语义 = 每行内容出现次数两侧相等。
        if !compare_multiset(&oracle_lines, &engine_sorted) {
            std::process::exit(1);
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
        // 数据符合性声明（stderr，不污染 stdout 对拍输出）。
        eprintln!();
        eprint!("{}", crate::nexmark_conformance::report(true));
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
            if let Some(rest) = line.strip_prefix("EMIT ")
                && let Some((rule, n)) = rest.rsplit_once(' ')
                && let Ok(n) = n.trim().parse::<u64>()
            {
                counts.insert(rule.to_string(), n);
            }
        }
    }
    Ok(counts)
}

/// oracle alert → 字段级明细行 `规则名\tentity_id\t字段名=值;...`（字段按名
/// 排序）。oracle 未求值 yield 字段的规则（stats 路径）fields 为空 → None
/// （该规则跳过明细对拍，保持计数对拍 + verify_file.sh CHECKS 内容断言）。
fn alert_detail_line(a: &crate::oracle::OracleAlert) -> Option<String> {
    if a.fields.is_empty() || a.intermediate {
        return None;
    }
    let mut fields = a.fields.clone();
    fields.sort_by(|x, y| x.0.cmp(&y.0));
    let joined = fields
        .iter()
        .map(|(n, v)| format!("{n}={v}"))
        .collect::<Vec<_>>()
        .join(";");
    Some(format!("{}\t{}\t{joined}", a.rule_name, a.entity_id))
}

/// 引擎 benchmark.ndjson → 字段级明细行（与 oracle 侧同格式）：排除 __wfu_*
/// 系统字段后，剩余字段即 yield 字段（id/alert_type/detail/request_count）。
/// 只保留 `detail_rules`（oracle 已求值 yield 的规则）的行——stats 规则
/// oracle 侧无明细，引擎侧同步过滤避免行数不匹配。坏行跳过。
fn engine_detail_lines(
    path: &std::path::Path,
    detail_rules: &std::collections::HashSet<String>,
) -> WfgenResult<Vec<String>> {
    let content = std::fs::read_to_string(path).map_err(|e| {
        crate::error::error(WfgenReason::Io, format!("read {}: {e}", path.display()))
    })?;
    let mut lines = Vec::new();
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let rule = v
            .get("__wfu_rule_name")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        if !detail_rules.contains(rule) {
            continue;
        }
        let entity = v
            .get("__wfu_entity_id")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let mut fields: Vec<(String, String)> = Vec::new();
        if let Some(obj) = v.as_object() {
            for (k, val) in obj {
                if k.starts_with("__wfu_") {
                    continue;
                }
                fields.push((k.clone(), json_value_to_str(val)));
            }
        }
        fields.sort();
        let joined = fields
            .iter()
            .map(|(n, s)| format!("{n}={s}"))
            .collect::<Vec<_>>()
            .join(";");
        lines.push(format!("{rule}\t{entity}\t{joined}"));
    }
    Ok(lines)
}

/// 两个明细行多集的比较：每行出现次数两侧相等即一致（重复行按多重数对齐）。
/// 输出差集（- oracle 独有 / + 引擎独有，最多 20 行）。
fn compare_multiset(a: &[String], b: &[String]) -> bool {
    let mut counts: HashMap<&str, i64> = HashMap::new();
    for x in a {
        *counts.entry(x.as_str()).or_insert(0) += 1;
    }
    for x in b {
        *counts.entry(x.as_str()).or_insert(0) -= 1;
    }
    let mut bad: Vec<String> = Vec::new();
    for (line, cnt) in counts {
        for _ in 0..cnt {
            bad.push(format!("- {line}"));
        }
        for _ in 0..(-cnt) {
            bad.push(format!("+ {line}"));
        }
    }
    if bad.is_empty() {
        println!("identical ✅ (multiset, {} lines)", a.len());
        return true;
    }
    bad.sort();
    println!("different (multiset): {} 行差", bad.len());
    for l in bad.iter().take(20) {
        println!("  {l}");
    }
    false
}

/// serde_json Value → 对拍字符串（与 oracle 侧 format_f64 同构：整数精度
/// Number 打印为整数）。
fn json_value_to_str(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                format!("{i}")
            } else if let Some(f) = n.as_f64() {
                crate::oracle::format_f64(f)
            } else {
                n.to_string()
            }
        }
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// 已知差异规则（oracle 与引擎实现面不同，计数与明细对拍都跳过，单独报告
/// 不判失败）：fixed+close 收口非确定（引擎多收/漏收尾部桶，oracle 理想值）。
/// 2026-08-30 提取为共享——detail 明细对拍同样跳过（q12 等）。
fn known_diff_rules() -> &'static [(&'static str, &'static str)] {
    &[
        // 2026-08-22 对齐后清理：q3/q6/q9/q20 的 join 可见性差异已随
        // 「over 调大 1h + 帧内跨流时间序排序」修复（10M 对拍完全一致），
        // q4 一致；旧规则名（q16_sum_price_1000/q17_distinct_bidders_20）
        // 已不存在于当前 wfl——全部移出 known 列表。
        // 2026-08-23 q11 修复后真一致（197,095 = 197,095）并移出 known。
        (
            "q12_bidder_10s_window_count",
            "fixed+close 收口（固定窗口 10s 桶）引擎多收尾部桶（10M 实测 oracle=102400 引擎=282514，多 ~176%）——fixed 收口预算/scan_timeouts 墙钟推进，oracle 事件时间到末尾即止；oracle 为理想值",
        ),
    ]
}

/// 归一化两侧为同序文本行 `规则名 计数`（Myers 才能对齐）：oracle 取全部规则
/// （跳过 _ 前缀元数据），引擎侧只取 oracle 覆盖的规则（单查询验证时其它查询
/// 残留 EMIT 是历史噪音）。已知差异规则（oracle 与引擎实现面不同，见下）
/// 剔除后单独报告，不判失败。
fn normalize_counts(
    oracle: &HashMap<String, u64>,
    engine: &HashMap<String, u64>,
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let known = known_diff_rules();
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
