use std::collections::HashSet;
use std::path::PathBuf;

use orion_error::conversion::SourceErr;
use serde::Serialize;

use crate::error::{self, WflReason, WflResult};

/// 单规则内存/实例实测峰值与建议（memory-limits.md 校准流程第 3–4 步自动版）。
#[derive(Debug, Clone, Serialize)]
pub struct RuleLimitEst {
    /// 规则名（metrics label）。
    pub rule: String,
    /// 存活实例数峰值（`rule.instances`）。
    pub peak_instances: u64,
    /// 实例状态估算内存峰值（`rule.memory_bytes`；引擎 2026-09-04 起导出）。
    pub peak_memory_bytes: u64,
    /// 平均每实例字节 = peak_memory / peak_instances。
    ///
    /// 量级参考：两个峰可能不在同一采样点（instances 峰值时刻 ≠ memory 峰值
    /// 时刻），不要当精确值用。
    pub avg_bytes_per_instance: f64,
    /// 建议 max_memory = peak_memory × headroom（向上取整到整 MiB/KiB/B）。
    pub suggested_max_memory_bytes: u64,
    /// 建议 max_instances = peak_instances × headroom。
    pub suggested_max_instances: u64,
    /// 建议 max_memory 的向上取整档位（GiB/MiB/KiB/B）——写回 `limits { max_memory = "17MB" }`
    /// 时整数单位的来源；配合 peak_memory_bytes × headroom 可完整复核建议值。
    pub memory_granularity: &'static str,
}

/// `wfl limits-est --format json` 报告（schema v1）。
#[derive(Debug, Clone, Serialize)]
pub struct LimitsEstReport {
    pub schema: &'static str,
    /// 输入 metrics 文件。
    pub metrics_file: String,
    /// 余量倍数（文档推荐 1.5–3×；默认 2）。
    pub headroom: f64,
    pub rules: Vec<RuleLimitEst>,
    /// 全 0 成因提示（未配 max_memory / stats 族规则）；非零时缺省。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

const ZERO_NOTE: &str = "rule.memory_bytes 全 0：规则未配 max_memory（引擎不记账）或属 stats 族 \
    规则（见 docs/useage/memory-limits.md §3 边界）——先配宽上限重跑真实负载再收紧";

/// 全规则均 stats 族（metrics 含 stats_over_limit_total 采样）时的提示——stats 执行器
/// 不在 rule.memory_bytes/instances 记账内（恒 0），重跑也出不来数，需换口径。
const STATS_NOTE: &str = "全部规则属 stats 族（metrics 含 stats_over_limit_total 采样）：\
    rule.memory_bytes / instances 不覆盖该执行器（恒 0）——本指令只校准 match/CEP 规则；\
    stats 族规则容量看 stats_over_limit_total 拒收计数与全局内存预算（memory-limits.md §3 边界）";

/// 从 metrics.ndjson 收集有 `stats_over_limit_total` 采样的规则（stats 执行器标记）。
///
/// stats 族规则（如 nexmark q15–q18/q22 形态）走自己 guard/计数，`rule.memory_bytes` /
/// `instances` 恒 0——靠这个标记把"stats 族（重跑无效）"与"未配 max_memory（重跑有效）"分开。
fn collect_stats_labels(lines: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    for line in lines.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("stage").and_then(|x| x.as_str()) != Some("rule") {
            continue;
        }
        if v.get("name").and_then(|x| x.as_str()) != Some("stats_over_limit_total") {
            continue;
        }
        if let Some(lb) = v.get("label").and_then(|x| x.as_str()) {
            out.insert(lb.to_string());
        }
    }
    out
}

/// 从 metrics.ndjson 文本统计每规则峰值（纯函数，可单测）。
///
/// 逐行取 `stage=rule` 且 name ∈ {instances, memory_bytes} 的 gauge 采样，每规则
/// 各取最大值。采样是周期快照（非累计），max 即运行期峰值。规则集 = 两指标
/// label 的**并集**（只出现一侧的规则也保留，缺侧补 0）。
fn peak_per_rule(lines: &str) -> Vec<(String, u64, u64)> {
    // label 首次出现顺序（inst/mem 两表都算），后查表补缺侧 0。
    let mut order: Vec<String> = Vec::new();
    let mut inst: Vec<(String, u64)> = Vec::new();
    let mut mem: Vec<(String, u64)> = Vec::new();
    for line in lines.lines() {
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("stage").and_then(|x| x.as_str()) != Some("rule") {
            continue;
        }
        let Some(name) = v.get("name").and_then(|x| x.as_str()) else {
            continue;
        };
        if name != "instances" && name != "memory_bytes" {
            continue;
        }
        let Some(label) = v.get("label").and_then(|x| x.as_str()) else {
            continue;
        };
        // metrics 行 value 是字符串形态（`"value":"2841"`）；也兼容纯数字。
        let val: u64 = v
            .get("value")
            .and_then(|x| {
                x.as_str()
                    .and_then(|s| s.parse().ok())
                    .or_else(|| x.as_u64())
            })
            .unwrap_or(0);
        if !order.iter().any(|r| r == label) {
            order.push(label.to_string());
        }
        match name {
            "instances" => upsert_max(&mut inst, label, val),
            "memory_bytes" => upsert_max(&mut mem, label, val),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for rule in order {
        let i = inst
            .iter()
            .find(|(r, _)| r == &rule)
            .map(|(_, v)| *v)
            .unwrap_or(0);
        let m = mem
            .iter()
            .find(|(r, _)| r == &rule)
            .map(|(_, v)| *v)
            .unwrap_or(0);
        out.push((rule, i, m));
    }
    out
}

fn upsert_max(acc: &mut Vec<(String, u64)>, label: &str, val: u64) {
    match acc.iter_mut().find(|(r, _)| r == label) {
        Some((_, v)) => *v = (*v).max(val),
        None => acc.push((label.to_string(), val)),
    }
}

/// 计算建议（纯函数）：cap = 峰值 × headroom；内存再向上取整到整洁档位。
fn estimate(rule: &str, peak_instances: u64, peak_memory: u64, headroom: f64) -> RuleLimitEst {
    let avg = if peak_instances > 0 {
        peak_memory as f64 / peak_instances as f64
    } else {
        0.0
    };
    let (rounded_mem, gran) = round_bytes_unit(peak_memory as f64 * headroom);
    RuleLimitEst {
        rule: rule.to_string(),
        peak_instances,
        peak_memory_bytes: peak_memory,
        avg_bytes_per_instance: avg,
        suggested_max_memory_bytes: rounded_mem,
        suggested_max_instances: (peak_instances as f64 * headroom).round() as u64,
        memory_granularity: gran,
    }
}

/// 逐规则推导行（纯函数，可单测）——把建议值还原成可复核的算术。
/// `stats` = 有 stats_over_limit_total 采样的规则集（stats 族标记）。
fn derivation_lines(est: &RuleLimitEst, headroom: f64, stats: &HashSet<String>) -> Vec<String> {
    let mut lines = Vec::new();
    let rule = &est.rule;
    if est.peak_instances == 0 && est.peak_memory_bytes == 0 {
        if stats.contains(rule) {
            lines.push(format!(
                "  {rule}: stats 族规则（metrics 含 stats_over_limit_total）——不在 \
                 rule.memory_bytes/instances 记账内（恒 0），本指令无法校准；看 \
                 stats_over_limit_total 拒收计数与全局内存预算（memory-limits.md §3）"
            ));
        } else {
            lines.push(format!(
                "  {rule}: 两指标记账值均 0（无有效采样）——若已配 max_memory 仍全 0 = 引擎不记账/\
                 stats 族；否则先配宽上限跑真实负载再收紧"
            ));
        }
        return lines;
    }
    if est.peak_instances == 0 {
        lines.push(format!(
            "  {rule}: 无 instances 采样（memory_bytes 有值）——只能估内存，max_instances 无实测基准"
        ));
    } else {
        lines.push(format!(
            "  {rule}: max_instances = round({} × {headroom:.1}) = {}",
            est.peak_instances, est.suggested_max_instances
        ));
    }
    if est.peak_memory_bytes == 0 {
        if stats.contains(rule) {
            lines.push(format!(
                "          max_memory 恒 0：stats 族规则不走 memory_bytes 记账（见 memory-limits.md §3）——\
                 看 stats_over_limit_total 与全局预算，本指令给不出建议"
            ));
        } else {
            lines.push(format!(
                "          max_memory 无实测：memory_bytes 全 0（未配 max_memory 引擎不记账 / stats 族规则）——先设宽上限重跑再收紧"
            ));
        }
    } else {
        let scaled = (est.peak_memory_bytes as f64 * headroom).round() as u64;
        lines.push(format!(
            "          max_memory = {}B × {headroom:.1} = {}B → 向上取整到 {} → {}B（{}）",
            fmt_thousands(est.peak_memory_bytes),
            fmt_thousands(scaled),
            est.memory_granularity,
            fmt_thousands(est.suggested_max_memory_bytes),
            fmt_bytes(est.suggested_max_memory_bytes),
        ));
    }
    lines
}

/// 过滤 + 估算 + 全零提示（纯函数，可单测）。`filter=None` = 全部规则。
/// `stats` = stats 族标记集，用于区分全零成因（重跑有效 vs 无效）。
fn assess(
    rows: Vec<(String, u64, u64)>,
    filter: Option<&str>,
    headroom: f64,
    stats: &HashSet<String>,
) -> (Vec<RuleLimitEst>, Option<String>) {
    let mut rules = Vec::new();
    for (rule, inst, mem) in rows {
        if let Some(f) = filter
            && rule != f
        {
            continue;
        }
        rules.push(estimate(&rule, inst, mem, headroom));
    }
    let note = if !rules.is_empty() && rules.iter().all(|e| e.peak_memory_bytes == 0) {
        if rules.iter().all(|e| stats.contains(&e.rule)) {
            Some(STATS_NOTE.to_string())
        } else {
            Some(ZERO_NOTE.to_string())
        }
    } else {
        None
    };
    (rules, note)
}

/// 建议值向上取整到整洁粒度（≥1GiB 取 GiB、≥1MiB 取 MiB、≥1KiB 取 KiB，否则原字节）
/// 并返回档位——档位供推导输出/JSON 复核（取整单位与取值同源，避免只给值推不出单位）。
fn round_bytes_unit(v: f64) -> (u64, &'static str) {
    const GIB: f64 = 1073741824.0;
    const MIB: f64 = 1048576.0;
    const KIB: f64 = 1024.0;
    if v >= GIB {
        ((v / GIB).ceil() as u64 * GIB as u64, "GiB")
    } else if v >= MIB {
        ((v / MIB).ceil() as u64 * MIB as u64, "MiB")
    } else if v >= KIB {
        ((v / KIB).ceil() as u64 * KIB as u64, "KiB")
    } else {
        (v.ceil() as u64, "B")
    }
}

/// 千分位（8,848,081）——推导输出里原字节数可复核。
fn fmt_thousands(v: u64) -> String {
    let s = v.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn fmt_bytes(v: u64) -> String {
    if v >= 1073741824 {
        format!("{:.1}GB", v as f64 / 1073741824.0)
    } else if v >= 1048576 {
        format!("{:.1}MB", v as f64 / 1048576.0)
    } else if v >= 1024 {
        format!("{:.0}KB", v as f64 / 1024.0)
    } else {
        format!("{v}B")
    }
}

pub fn run(
    metrics: PathBuf,
    rule_filter: Option<String>,
    headroom: f64,
    format: String,
) -> WflResult<()> {
    if !(1.0..=10.0).contains(&headroom) {
        return error::fail(
            WflReason::Validation,
            format!("--headroom {headroom} 超出 [1,10]（文档推荐 1.5–3）"),
        );
    }
    let json = matches!(format.as_str(), "json" | "jsonl");
    let content = std::fs::read_to_string(&metrics)
        .source_err(WflReason::Io, format!("reading {}", metrics.display()))?;

    let mut rows = peak_per_rule(&content);
    if rows.is_empty() {
        return error::fail(
            WflReason::Validation,
            format!(
                "{} 无规则采样（stage=rule name=instances/memory_bytes）——\
                 用 bench.sh/diag.sh/verify_daemon.sh 跑一次再评估",
                metrics.display()
            ),
        );
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    // 可用规则名先收集（rows 随后被 assess 消费）。
    let avail: Vec<String> = rows.iter().map(|(r, _, _)| r.clone()).collect();
    let stats = collect_stats_labels(&content);
    let (rules, note) = assess(rows, rule_filter.as_deref(), headroom, &stats);
    if rules.is_empty() {
        return error::fail(
            WflReason::Validation,
            format!(
                "metrics 中没有规则 `{}` 的采样；可用规则: {}",
                rule_filter.unwrap_or_default(),
                avail.join(", ")
            ),
        );
    }

    if json {
        let report = LimitsEstReport {
            schema: "wfl-limits-est/v1",
            metrics_file: metrics.display().to_string(),
            headroom,
            rules,
            note,
        };
        let out = serde_json::to_string_pretty(&report)
            .source_err(WflReason::Serialization, "serializing limits-est report")?;
        println!("{out}");
    } else {
        if let Some(n) = &note {
            eprintln!("  提示: {n}");
        }
        // ASCII 列名 + 定宽右对齐（全角列名会被 ascii padding 按单字符计 → 错位）。
        println!(
            "{:<28} {:>10} {:>12} {:>9}  {:>10} {:>10}",
            "rule", "inst_peak", "mem_peak", "B/inst", "suggest_mem", "suggest_inst"
        );
        for r in &rules {
            println!(
                "{:<28} {:>10} {:>12} {:>9}  {:>10} {:>10}",
                r.rule,
                r.peak_instances,
                fmt_bytes(r.peak_memory_bytes),
                format!("{:.0}", r.avg_bytes_per_instance),
                fmt_bytes(r.suggested_max_memory_bytes),
                r.suggested_max_instances,
            );
        }
        println!("\n推导（headroom={headroom:.1}）:");
        for r in &rules {
            for line in derivation_lines(r, headroom, &stats) {
                println!("{line}");
            }
        }
        println!(
            "\n口径：建议 = 实测峰值 × headroom（内存再向上取整到整洁档位）；上限是保险丝非工作带——\
             先设宽跑最坏负载、收紧后 oracle 对拍防静默丢（memory-limits.md §4–5）"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_metrics() -> String {
        // 两规则 × 两指标 × 三段采样（首/峰值/收口归零）——峰值取 max。
        let mut s = String::new();
        for line in [
            r#"{"stage":"rule","name":"instances","label":"r_heavy","value":"10"}"#,
            r#"{"stage":"rule","name":"memory_bytes","label":"r_heavy","value":"1000"}"#,
            r#"{"stage":"rule","name":"instances","label":"r_heavy","value":"2841"}"#,
            r#"{"stage":"rule","name":"memory_bytes","label":"r_heavy","value":"9500000"}"#,
            r#"{"stage":"rule","name":"instances","label":"r_heavy","value":"0"}"#,
            r#"{"stage":"rule","name":"memory_bytes","label":"r_heavy","value":"0"}"#,
            // 第二规则：有实例无内存——stats 族标记（stats_over_limit_total）应被识别。
            r#"{"stage":"rule","name":"instances","label":"r_stats","value":"88"}"#,
            r#"{"stage":"rule","name":"memory_bytes","label":"r_stats","value":"0"}"#,
            r#"{"stage":"rule","name":"stats_over_limit_total","label":"r_stats","value":"3"}"#,
            // 噪声行（非 rule / 其它指标 / 坏 JSON）应跳过。
            r#"{"stage":"window","name":"rows","label":"w1","value":"99"}"#,
            r#"{"stage":"rule","name":"events_total","label":"r_heavy","value":"42"}"#,
            "not-json",
        ] {
            s.push_str(line);
            s.push('\n');
        }
        s
    }

    #[test]
    fn peak_scan_takes_max_per_rule_and_skips_noise() {
        let rows = peak_per_rule(&sample_metrics());
        assert_eq!(rows.len(), 2);
        let heavy = rows.iter().find(|(r, _, _)| r == "r_heavy").unwrap();
        assert_eq!(heavy.1, 2841); // instances 峰值
        assert_eq!(heavy.2, 9_500_000); // memory 峰值
        let stats = rows.iter().find(|(r, _, _)| r == "r_stats").unwrap();
        assert_eq!(stats.1, 88);
        assert_eq!(stats.2, 0);
    }

    #[test]
    fn one_sided_rules_are_not_dropped() {
        // 只出现在一侧指标的规则也保留（并集语义），缺侧补 0。
        let txt = concat!(
            r#"{"stage":"rule","name":"memory_bytes","label":"mem_only","value":"7000"}"#,
            "\n",
            r#"{"stage":"rule","name":"instances","label":"inst_only","value":"5"}"#,
            "\n",
        );
        let rows = peak_per_rule(txt);
        assert_eq!(rows.len(), 2);
        let mem_only = rows.iter().find(|(r, _, _)| r == "mem_only").unwrap();
        assert_eq!(mem_only.1, 0); // 无实例采样 → 0
        assert_eq!(mem_only.2, 7000);
        let inst_only = rows.iter().find(|(r, _, _)| r == "inst_only").unwrap();
        assert_eq!(inst_only.1, 5);
        assert_eq!(inst_only.2, 0);
    }

    #[test]
    fn estimate_scales_by_headroom_and_avg() {
        let est = estimate("r_heavy", 2841, 9_500_000, 2.0);
        assert_eq!(est.suggested_max_instances, 5682);
        // 19_000_000 B = 18.12 MiB → 向上取整 19 MiB。
        assert_eq!(est.suggested_max_memory_bytes, 19 * 1048576);
        assert!((est.avg_bytes_per_instance - 9_500_000.0 / 2841.0).abs() < 1.0);
        // 实例峰为 0 → avg 无意义 0。
        let zero = estimate("x", 0, 500, 2.0);
        assert_eq!(zero.avg_bytes_per_instance, 0.0);
    }

    #[test]
    fn assess_filters_and_note_only_when_all_zero() {
        let stats = collect_stats_labels(&sample_metrics());
        // 全 0 → note；带过滤（r_stats 是 stats 族 → STATS_NOTE 而非通用 ZERO_NOTE）。
        let (rules, note) = assess(
            peak_per_rule(&sample_metrics()),
            Some("r_stats"),
            2.0,
            &stats,
        );
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].rule, "r_stats");
        let n = note.expect("all-zero 应有 note");
        assert!(n.contains("stats_over_limit_total"), "{n}");
        // 非全 0（heavy 参与）→ 无 note。
        let (rules, note) = assess(peak_per_rule(&sample_metrics()), None, 2.0, &stats);
        assert_eq!(rules.len(), 2);
        assert!(note.is_none());
        // 过滤未命中 → 空 + 无 note。
        let (rules, note) = assess(peak_per_rule(&sample_metrics()), Some("nope"), 2.0, &stats);
        assert!(rules.is_empty());
        assert!(note.is_none());
        // 未配 max_memory 的全零规则（metrics 无 stats 标记）→ 通用 ZERO_NOTE（重跑有效）。
        let txt = concat!(
            r#"{"stage":"rule","name":"instances","label":"r_unset","value":"0"}"#,
            "\n",
            r#"{"stage":"rule","name":"memory_bytes","label":"r_unset","value":"0"}"#,
            "\n",
        );
        let (rules, note) = assess(peak_per_rule(txt), None, 2.0, &collect_stats_labels(txt));
        assert_eq!(rules.len(), 1);
        let n = note.expect("全零未标记应有 ZERO_NOTE");
        assert!(
            n.contains("max_memory") && !n.contains("stats_over_limit_total"),
            "{n}"
        );
    }

    #[test]
    fn collect_stats_labels_finds_stats_family_rules() {
        let stats = collect_stats_labels(&sample_metrics());
        assert!(stats.contains("r_stats"));
        assert!(!stats.contains("r_heavy")); // 非 stats 族不带标记
        // 无 stats 计数 → 空集。
        assert!(collect_stats_labels("{\"stage\":\"window\"}\n").is_empty());
    }

    #[test]
    fn report_note_is_serialized_only_when_present() {
        let ests = vec![estimate("r_heavy", 2841, 9_500_000, 2.0)];
        let no_note = LimitsEstReport {
            schema: "wfl-limits-est/v1",
            metrics_file: "m".into(),
            headroom: 2.0,
            rules: ests.clone(),
            note: None,
        };
        let v = serde_json::to_value(&no_note).unwrap();
        assert!(v.get("note").is_none());
        let with_note = LimitsEstReport {
            schema: "wfl-limits-est/v1",
            metrics_file: "m".into(),
            headroom: 2.0,
            rules: ests,
            note: Some(ZERO_NOTE.to_string()),
        };
        let v = serde_json::to_value(&with_note).unwrap();
        assert!(v["note"].as_str().unwrap().contains("max_memory"));
    }

    #[test]
    fn round_bytes_picks_readable_granularity() {
        assert_eq!(round_bytes_unit(19_000_000.0), (19 * 1048576, "MiB")); // → 19 MiB
        assert_eq!(round_bytes_unit(60_000.0), (59 * 1024, "KiB")); // 58.6 KiB → 59 KiB
        assert_eq!(round_bytes_unit(500.0), (500, "B")); // B
        // GB 档：15 GiB 级规则（q18 画像）向上取整到 GiB。
        assert_eq!(
            round_bytes_unit(15.0 * 1073741824.0 + 1.0),
            (16 * 1073741824, "GiB")
        );
    }

    #[test]
    fn estimate_records_memory_granularity_tier() {
        // 档位随缩放后量级走：MiB / KiB / B / GiB。
        let heavy = estimate("h", 10, 9_500_000, 2.0); // 19MiB 级
        assert_eq!(heavy.memory_granularity, "MiB");
        assert_eq!(heavy.suggested_max_memory_bytes, 19 * 1048576);
        let small = estimate("s", 1, 500, 2.0); // 1000B → B 档
        assert_eq!(small.memory_granularity, "B");
        assert_eq!(small.suggested_max_memory_bytes, 1000);
        let big = estimate("g", 1, 8 * 1073741824, 2.0); // 16GiB 级
        assert_eq!(big.memory_granularity, "GiB");
        assert_eq!(big.suggested_max_memory_bytes, 16 * 1073741824);
    }

    #[test]
    fn derivation_lines_replay_arithmetic_per_shape() {
        let no_stats = HashSet::new();
        // 正常形态：实例与内存推导行都能手算复核。
        let est = estimate("r_heavy", 2841, 9_500_000, 2.0);
        let lines = derivation_lines(&est, 2.0, &no_stats);
        assert_eq!(lines.len(), 2);
        assert!(
            lines[0].contains("max_instances = round(2841 × 2.0) = 5682"),
            "{}",
            lines[0]
        );
        assert!(
            lines[1].contains("max_memory = 9,500,000B × 2.0 = 19,000,000B")
                && lines[1].contains("向上取整到 MiB")
                && lines[1].contains("19,922,944B（19.0MB）"),
            "{}",
            lines[1]
        );
        // stats 族（有 stats_over_limit_total 标记）：明示执行器边界，不误导重跑。
        let mut stats = HashSet::new();
        stats.insert("r_stats".to_string());
        let est = estimate("r_stats", 88, 0, 2.0);
        let lines = derivation_lines(&est, 2.0, &stats);
        assert!(lines[1].contains("stats 族"), "{}", lines[1]);
        assert!(!lines[1].contains("先设宽上限重跑"), "{}", lines[1]);
        // 两指标均 0 且非 stats 标记（未配 max_memory 但引擎确实没记账）。
        let est = estimate("r_none", 0, 0, 2.0);
        let lines = derivation_lines(&est, 2.0, &no_stats);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("两指标记账值均 0"), "{}", lines[0]);
    }

    #[test]
    fn derivation_lines_flag_both_zero_stats_rule() {
        // 用户实跑的 q18 形态：stats 族 + instances/memory 双 0 → 直接指向 §3 边界。
        let mut stats = HashSet::new();
        stats.insert("q18_last_bid_stats".to_string());
        let est = estimate("q18_last_bid_stats", 0, 0, 2.0);
        let lines = derivation_lines(&est, 2.0, &stats);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("stats 族规则"), "{}", lines[0]);
        assert!(lines[0].contains("memory-limits.md §3"), "{}", lines[0]);
    }

    #[test]
    fn derivation_shows_which_suggestion_lacks_basis() {
        let no_stats = HashSet::new();
        // mem-only（instances 缺采样）：内存可估、实例数无基准——必须明示，防抄错。
        let est = estimate("mem_only", 0, 7_000, 2.0);
        let lines = derivation_lines(&est, 2.0, &no_stats);
        assert!(lines[0].contains("无 instances 采样"), "{}", lines[0]);
        assert!(lines[1].contains("max_memory = 7,000B"), "{}", lines[1]);
        assert_eq!(est.suggested_max_instances, 0);
    }

    #[test]
    fn fmt_thousands_groups_digits() {
        assert_eq!(fmt_thousands(8_848_081), "8,848,081");
        assert_eq!(fmt_thousands(19_922_944), "19,922,944");
        assert_eq!(fmt_thousands(500), "500");
        assert_eq!(fmt_thousands(1_000), "1,000");
        assert_eq!(fmt_thousands(0), "0");
    }

    #[test]
    fn fmt_bytes_tiers_b_kb_mb_gb() {
        assert_eq!(fmt_bytes(500), "500B");
        assert_eq!(fmt_bytes(60 * 1024), "60KB");
        assert_eq!(fmt_bytes(8_848_081), "8.4MB");
        assert_eq!(fmt_bytes(15 * 1073741824), "15.0GB");
    }

    #[test]
    fn empty_metrics_yields_no_rows() {
        assert!(peak_per_rule("").is_empty());
        assert!(peak_per_rule("{\"stage\":\"window\"}\nnot-json\n").is_empty());
    }

    #[test]
    fn headroom_out_of_range_fails_before_file_read() {
        // 校验在文件读取前：文件不存在也应报 headroom 错而非 Io 错。
        let err = run(
            PathBuf::from("/nonexistent/metrics.ndjson"),
            None,
            0.5,
            "human".into(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("headroom"), "{err}");
        let err = run(
            PathBuf::from("/nonexistent/metrics.ndjson"),
            None,
            42.0,
            "human".into(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("headroom"));
    }
}
