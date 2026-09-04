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
    /// 峰值时刻平均每实例字节（peak_memory / peak_instances；实例峰为 0 时无意义）。
    pub avg_bytes_per_instance: f64,
    /// 建议 max_memory = peak_memory × headroom（四舍五入到字节）。
    pub suggested_max_memory_bytes: u64,
    /// 建议 max_instances = peak_instances × headroom。
    pub suggested_max_instances: u64,
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
}

/// 从 metrics.ndjson 文本统计每规则的峰值（纯函数，可单测）。
///
/// 逐行取 `stage=rule` 且 name ∈ {instances, memory_bytes} 的 gauge 采样，
/// 每规则取各自最大值。采样是周期快照（非累计），max 即运行期峰值。
fn peak_per_rule(lines: &str) -> Vec<(String, u64, u64)> {
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
            .and_then(|x| x.as_str().and_then(|s| s.parse().ok()).or_else(|| x.as_u64()))
            .unwrap_or(0);
        match name {
            "instances" => upsert_max(&mut inst, label, val),
            "memory_bytes" => upsert_max(&mut mem, label, val),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for (rule, i) in inst {
        let m = mem
            .iter()
            .find(|(r, _)| r == &rule)
            .map(|(_, m)| *m)
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

/// 计算建议（纯函数）：cap = 峰值 × headroom。
fn estimate(rule: &str, peak_instances: u64, peak_memory: u64, headroom: f64) -> RuleLimitEst {
    let avg = if peak_instances > 0 {
        peak_memory as f64 / peak_instances as f64
    } else {
        0.0
    };
    RuleLimitEst {
        rule: rule.to_string(),
        peak_instances,
        peak_memory_bytes: peak_memory,
        avg_bytes_per_instance: avg,
        suggested_max_memory_bytes: round_bytes(peak_memory as f64 * headroom),
        suggested_max_instances: (peak_instances as f64 * headroom).round() as u64,
    }
}

/// 建议值向上取整到整洁粒度：≥1MiB 取整 MiB、≥1KiB 取整 KiB、否则字节——
/// 给可写进 `limits { max_memory = "19MB" }` 的值（向上取保守侧）。
fn round_bytes(v: f64) -> u64 {
    if v >= 1048576.0 {
        (v / 1048576.0).ceil() as u64 * 1048576
    } else if v >= 1024.0 {
        (v / 1024.0).ceil() as u64 * 1024
    } else {
        v.ceil() as u64
    }
}

fn fmt_bytes(v: u64) -> String {
    if v >= 1048576 {
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
                "{} 无 rule.instances 采样——用 bench.sh/diag.sh/verify_daemon.sh 跑一次后\
                 再评估（metrics.ndjson 缺规则 gauge）",
                metrics.display()
            ),
        );
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));

    let mut rules: Vec<RuleLimitEst> = Vec::new();
    let mut noted = false;
    for (rule, inst, mem) in rows {
        if let Some(f) = &rule_filter
            && rule != *f
        {
            continue;
        }
        if mem == 0 && !noted {
            // 一次提示即可：peak 0 的成因（未配 max_memory / stats 族规则）写进 human。
            noted = true;
            if !json {
                eprintln!(
                    "  提示: rule.memory_bytes 全 0 → 规则未配 max_memory（引擎不记账）或属 \
                     stats 族（见 docs/useage/memory-limits.md §3 边界）——先配宽上限重跑再收紧"
                );
            }
        }
        rules.push(estimate(&rule, inst, mem, headroom));
    }
    if rules.is_empty() {
        return error::fail(
            WflReason::Validation,
            format!("metrics 中没有规则 `{}` 的采样", rule_filter.unwrap_or_default()),
        );
    }

    if json {
        let report = LimitsEstReport {
            schema: "wfl-limits-est/v1",
            metrics_file: metrics.display().to_string(),
            headroom,
            rules,
        };
        let out = serde_json::to_string_pretty(&report)
            .source_err(WflReason::Serialization, "serializing limits-est report")?;
        println!("{out}");
    } else {
        println!(
            "{:<28} {:>9} {:>10} {:>10}  建议 max_memory = 峰值×{headroom:.1}   建议 max_instances",
            "rule", "inst峰值", "mem峰值", "B/实例"
        );
        for r in &rules {
            println!(
                "{:<28} {:>9} {:>10} {:>10}  {:<9} (上限 {:.1}×)   {:>9} (上限 {:.1}×)",
                r.rule,
                r.peak_instances,
                fmt_bytes(r.peak_memory_bytes),
                format!("{:.0}", r.avg_bytes_per_instance),
                fmt_bytes(r.suggested_max_memory_bytes),
                headroom,
                r.suggested_max_instances,
                headroom,
            );
        }
        println!(
            "\n建议 = 实测峰值 × {headroom:.1} 余量（保险丝，非工作带）；先配宽上限跑最坏负载、\
             收紧后跑 oracle 对拍防静默丢（memory-limits.md §4–5）"
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
            // 第二规则：有实例无内存（stats 族/未配 max_memory → 恒 0）。
            r#"{"stage":"rule","name":"instances","label":"r_stats","value":"88"}"#,
            r#"{"stage":"rule","name":"memory_bytes","label":"r_stats","value":"0"}"#,
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
    fn round_bytes_picks_readable_granularity() {
        assert_eq!(round_bytes(19_000_000.0), 19 * 1048576); // → 19 MiB
        assert_eq!(round_bytes(60_000.0), 59 * 1024); // 58.6 KiB → 59 KiB
        assert_eq!(round_bytes(500.0), 500); // B
    }

    #[test]
    fn empty_metrics_yields_no_rows() {
        assert!(peak_per_rule("").is_empty());
        assert!(peak_per_rule("{\"stage\":\"window\"}\nnot-json\n").is_empty());
    }
}
