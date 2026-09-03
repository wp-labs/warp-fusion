// ---------------------------------------------------------------------------
// L4 性能门禁（perf gate）：把 perf-diag 墙梯测量机制化为自动门禁
// ---------------------------------------------------------------------------
// 消费 `wfgen perf-diag` 产出的墙表（每档 EPS），按项目门禁配置断言
// 「单规则成本增量」是否可接受：
//
//   [absolute] 绝对兜底（D5a）——机器/负载校准的硬上限：
//     - rules_eps_min   整集 rules 档 EPS 下限（整集慢到离谱 → FAIL）
//     - per_rule_ns_max 摊到单规则的成本上限（ns/事件/规则）：
//         per_rule_ns = (1e9/eps_rules − 1e9/eps_floor) / rule_count
//       rules−floor 增量 = 规则求值成本；除以规则数 = 单规则成本增量。
//       专治"AI 产出一条全窗扫描/高基数 key 的贵规则"——语义对但把整集
//       拖垮的回执必须能拦。
//   [relative] 相对防回归（D5b）——同机同 feed 口径与上次通过跑（基线墙表）比：
//     - baseline + stages + max_regression_pct
//
// 用法（配 perf-diag 同一诊断）：
//   1) 首次校准：wfgen perf-diag --diag conf/perf-diag.toml ... \
//        --record-baseline data/perf_wall.baseline.txt     # 留存通过基线
//   2) 门禁：    wfgen perf-diag ... --gate conf/perf-gate.toml
//       └ 任一断言 FAIL → verdict=FAIL → exit 1（AI/CI 按退出码拦下）
//
// 语义约定：每个档可能有多个 N 行（--n-list）；门禁取**最大 N** 行
// （固定开销被摊薄后的每事件成本最稳）。相对回归只与基线的同 (档, N) 行比，
// 缺档/缺 N → 明确报错（提示同 n-list 重录基线），不静默跳过。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error;
use crate::error::{WfgenReason, WfgenResult};

// ---------------------------------------------------------------------------
// 墙表行（与 perf-diag 输出文件同格式，双向可读）
// ---------------------------------------------------------------------------

/// 一个 (档, N) 测量点：best-of-rounds EPS。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WallRow {
    pub stage: String,
    pub eps: f64,
    pub n: u64,
    pub rounds: usize,
}

/// 渲染墙表行（与 `run_perf_diag` 输出文件逐字节一致）。
pub fn render_wall_row(row: &WallRow) -> String {
    format!(
        "{}  eps={:.0} n={} rounds={}",
        row.stage, row.eps, row.n, row.rounds
    )
}

/// 解析一行 `stage  eps=<f64> n=<u64> rounds=<usize>`（容错：顺序任意、垃圾行跳过）。
fn parse_wall_row(line: &str) -> Option<WallRow> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let mut tokens = line.split_whitespace();
    let stage = tokens.next()?.to_string();
    let mut eps = None;
    let mut n = None;
    let mut rounds = None;
    for tok in tokens {
        if let Some(v) = tok.strip_prefix("eps=") {
            eps = v.parse::<f64>().ok();
        } else if let Some(v) = tok.strip_prefix("n=") {
            n = v.parse::<u64>().ok();
        } else if let Some(v) = tok.strip_prefix("rounds=") {
            rounds = v.parse::<usize>().ok();
        }
    }
    Some(WallRow {
        stage,
        eps: eps?,
        n: n?,
        rounds: rounds.unwrap_or(1),
    })
}

/// 解析墙表文本（perf-diag 输出/基线文件的正文；空行/坏行跳过）。
pub fn parse_wall_table(text: &str) -> Vec<WallRow> {
    text.lines().filter_map(parse_wall_row).collect()
}

/// 读墙表文件；缺失 → 报错（显式 --gate/--record-baseline 即要求文件存在）。
pub fn read_wall_file(path: &Path) -> WfgenResult<Vec<WallRow>> {
    let content = std::fs::read_to_string(path).map_err(|e| {
        error::error(
            WfgenReason::Io,
            format!("reading wall table {}: {e}", path.display()),
        )
    })?;
    let rows = parse_wall_table(&content);
    if rows.is_empty() {
        return Err(error::error(
            WfgenReason::Validation,
            format!("{} 无有效墙表行（预期格式: `stage  eps=.. n=.. rounds=..`）", path.display()),
        ));
    }
    Ok(rows)
}

/// 取 (stage, n) 的 EPS；`n=None` = 取该档最大 N 行（门禁口径，见模块注释）。
fn eps_at(wall: &[WallRow], stage: &str, n: u64) -> Option<f64> {
    wall.iter()
        .find(|r| r.stage == stage && r.n == n)
        .map(|r| r.eps)
}

fn max_n(wall: &[WallRow]) -> u64 {
    wall.iter().map(|r| r.n).max().unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 门禁配置（conf/perf-gate.toml）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AbsoluteCaps {
    /// 整集 rules 档 EPS 下限（缺省 = 不断言；须 > 0）。
    #[serde(default)]
    pub rules_eps_min: Option<f64>,
    /// 单规则成本上限（ns/事件/规则）——需 rule_count>0 且有 floor+rules 档；须 > 0。
    #[serde(default)]
    pub per_rule_ns_max: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RelativeCaps {
    /// 基线墙表文件（上次通过跑 --record-baseline 留存）。
    #[serde(default)]
    pub baseline: Option<PathBuf>,
    /// 做相对回归的档名列表（缺省 = ["rules"]）。
    #[serde(default = "default_rel_stages")]
    pub stages: Vec<String>,
    /// 允许的 EPS 回退百分比（10 = 现 EPS ≥ 基线的 90%；[0,100]）。
    #[serde(default)]
    pub max_regression_pct: Option<f64>,
}

impl Default for RelativeCaps {
    fn default() -> Self {
        Self {
            baseline: None,
            stages: default_rel_stages(),
            max_regression_pct: None,
        }
    }
}

fn default_rel_stages() -> Vec<String> {
    vec!["rules".to_string()]
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GateConfig {
    /// 被测规则集大小（把 rules-floor 增量摊到单规则成本用）。
    #[serde(default)]
    pub rule_count: usize,
    #[serde(default)]
    pub absolute: AbsoluteCaps,
    #[serde(default)]
    pub relative: RelativeCaps,
}

impl GateConfig {
    pub fn load(path: &Path) -> WfgenResult<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            error::error(
                WfgenReason::Io,
                format!("reading perf-gate config {}: {e}", path.display()),
            )
        })?;
        let cfg: GateConfig = toml::from_str(&content).map_err(|e| {
            error::error(
                WfgenReason::Validation,
                format!("parsing perf-gate config {}: {e}", path.display()),
            )
        })?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// 配置自检：至少一条断言；per_rule_ns_max 需要 rule_count；relative 需要
    /// baseline+回退率。尽早报错——门禁开着但"没拦任何东西"是静默失效。
    fn validate(&self) -> WfgenResult<()> {
        let has_abs = self.absolute.rules_eps_min.is_some() || self.absolute.per_rule_ns_max.is_some();
        let rel = &self.relative;
        // 相对断言信号 = 给了基线（真要比）或给了回退率（想比但可能漏了基线）。
        // 不把 stages 算作信号——它有 serde 默认值（[relative] 整段省略也存在）。
        let has_rel = rel.baseline.is_some() || rel.max_regression_pct.is_some();
        if !has_abs && !has_rel {
            return Err(error::error(
                WfgenReason::Validation,
                "perf-gate 未配置任何断言：[absolute]（rules_eps_min/per_rule_ns_max）\
                 或 [relative]（baseline+stages+max_regression_pct）至少一项",
            ));
        }
        if self.absolute.per_rule_ns_max.is_some() && self.rule_count == 0 {
            return Err(error::error(
                WfgenReason::Validation,
                "per_rule_ns_max（单规则成本上限）需 rule_count > 0",
            ));
        }
        if rel.baseline.is_some() {
            if rel.stages.is_empty() {
                return Err(error::error(
                    WfgenReason::Validation,
                    "[relative] stages 不能为空",
                ));
            }
            if rel.max_regression_pct.is_none() {
                return Err(error::error(
                    WfgenReason::Validation,
                    "[relative] 需 max_regression_pct（允许的 EPS 回退百分比）",
                ));
            }
        }
        // max_regression_pct 给了却忘写 baseline → 相对断言无从比，静默失效。
        if rel.baseline.is_none() && rel.max_regression_pct.is_some() {
            return Err(error::error(
                WfgenReason::Validation,
                "[relative] max_regression_pct 需配套 baseline（基线墙表文件）",
            ));
        }
        if let Some(pct) = rel.max_regression_pct
            && !(0.0..=100.0).contains(&pct)
        {
            return Err(error::error(
                WfgenReason::Validation,
                format!("max_regression_pct={pct} 超出 [0,100]"),
            ));
        }
        // 绝对断言的值必须为正——负值/0 会让断言恒过（拼错级静默失效）。
        for (name, val) in [
            ("rules_eps_min", self.absolute.rules_eps_min),
            ("per_rule_ns_max", self.absolute.per_rule_ns_max),
        ] {
            // NaN 与任何比较都不成立，须显式排除后看非正。
            if let Some(v) = val
                && (v.is_nan() || v <= 0.0)
            {
                return Err(error::error(
                    WfgenReason::Validation,
                    format!("{name}={v} 必须为正数"),
                ));
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 门禁评估
// ---------------------------------------------------------------------------

/// 一条断言结果（json/human 共用）。
#[derive(Debug, Clone, Serialize)]
pub struct GateCheck {
    /// 指标族：rules_set_eps / per_rule_ns / eps_regression。
    pub metric: String,
    /// 档/区间（relative 用档名；per_rule 用 "rules-floor"）。
    pub stage: String,
    pub measured: f64,
    pub threshold: f64,
    pub unit: String,
    /// 关系符号（如 ">=" / "<="），threshold 与 measured 同单位。
    pub relation: String,
    pub passed: bool,
    /// 自解释明细（失败原因直接可读，AI/人免翻表）。
    pub detail: String,
}

/// 记录基线墙表（--record-baseline）：同 perf-diag 输出文件格式。
pub fn write_baseline(path: &Path, wall: &[WallRow]) -> WfgenResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            error::error(
                WfgenReason::Io,
                format!("creating {}: {e}", parent.display()),
            )
        })?;
    }
    let body: String = wall.iter().map(render_wall_row).collect::<Vec<_>>().join("\n");
    std::fs::write(path, body + "\n").map_err(|e| {
        error::error(
            WfgenReason::Io,
            format!("writing baseline {}: {e}", path.display()),
        )
    })?;
    Ok(())
}

/// 评估门禁。`current` = 本次墙表（每档取最大 N 行口径）。
pub fn evaluate_gate(cfg: &GateConfig, current: &[WallRow]) -> WfgenResult<Vec<GateCheck>> {
    if current.is_empty() {
        return Err(error::error(
            WfgenReason::Validation,
            "本次墙表为空，无法评估门禁",
        ));
    }
    let n_ref = max_n(current);
    let mut checks: Vec<GateCheck> = Vec::new();

    // --- 绝对兜底 ---
    if let Some(min) = cfg.absolute.rules_eps_min {
        let eps = eps_at(current, "rules", n_ref).ok_or_else(|| {
            missing_measurement("rules", n_ref, current)
        })?;
        checks.push(GateCheck {
            metric: "rules_set_eps".to_string(),
            stage: "rules".to_string(),
            measured: eps,
            threshold: min,
            unit: "eps".to_string(),
            relation: ">=".to_string(),
            passed: eps >= min,
            detail: format!(
                "整集规则求值 EPS={eps:.0}（n={n_ref}），下限 {min:.0}——低于下限说明规则集整体过慢"
            ),
        });
    }

    if let Some(cap) = cfg.absolute.per_rule_ns_max {
        let eps_floor = eps_at(current, "floor", n_ref).ok_or_else(|| {
            missing_measurement("floor", n_ref, current)
        })?;
        let eps_rules = eps_at(current, "rules", n_ref).ok_or_else(|| {
            missing_measurement("rules", n_ref, current)
        })?;
        // rules−floor 增量 = 规则求值成本（ns/事件）；除规则数 = 单规则成本增量。
        let per_rule_ns = (1e9 / eps_rules - 1e9 / eps_floor) / cfg.rule_count as f64;
        let per_rule_ns = per_rule_ns.max(0.0); // 噪声可能让增量为负 → 视为 0
        checks.push(GateCheck {
            metric: "per_rule_ns".to_string(),
            stage: "rules-floor".to_string(),
            measured: per_rule_ns,
            threshold: cap,
            unit: "ns/evt/rule".to_string(),
            relation: "<=".to_string(),
            passed: per_rule_ns <= cap,
            detail: format!(
                "规则求值成本摊到 {} 条规则：每事件每规则 {per_rule_ns:.1} ns，上限 {cap:.0}\
                 （floor eps={eps_floor:.0} → rules eps={eps_rules:.0}）",
                cfg.rule_count
            ),
        });
    }

    // --- 相对防回归 ---
    if let Some(base_path) = &cfg.relative.baseline {
        let base = read_wall_file(base_path)?;
        let pct = cfg.relative.max_regression_pct.unwrap_or(0.0);
        for stage in &cfg.relative.stages {
            let now = eps_at(current, stage, n_ref).ok_or_else(|| {
                missing_measurement(stage, n_ref, current)
            })?;
            let base_eps = eps_at(&base, stage, n_ref).ok_or_else(|| {
                error::error(
                    WfgenReason::Validation,
                    format!(
                        "基线 {} 缺档 `{stage}`（n={n_ref}）——基线须与本次同档同 n-list\
                         （用 --record-baseline 从同诊断重录）",
                        base_path.display()
                    ),
                )
            })?;
            let allowed = base_eps * (1.0 - pct / 100.0);
            let regress_pct = (base_eps - now) / base_eps * 100.0;
            let ok = now >= allowed;
            checks.push(GateCheck {
                metric: "eps_regression".to_string(),
                stage: stage.clone(),
                measured: now,
                threshold: allowed,
                unit: "eps".to_string(),
                relation: ">=".to_string(),
                passed: ok,
                detail: format!(
                    "{} 档 EPS={now:.0} vs 基线 {base_eps:.0}（回退 {regress_pct:.1}%，允许 ≤{pct:.0}%）",
                    stage
                ),
            });
        }
    }

    Ok(checks)
}

fn missing_measurement(stage: &str, n_ref: u64, wall: &[WallRow]) -> crate::error::WfgenError {
    error::error(
        WfgenReason::Validation,
        format!(
            "本次墙表缺档 `{stage}`（n={n_ref}），无法评估该断言——\
             检查 --diag 的 [[stages]] 是否含对应档（可用行：{}）",
            wall.iter()
                .map(render_wall_row)
                .collect::<Vec<_>>()
                .join(" | ")
        ),
    )
}

/// 门禁结果（含是否通过，供调用方决定退出码）。
#[derive(Debug, Clone, Serialize)]
pub struct GateOutcome {
    pub config: String,
    pub checks: Vec<GateCheck>,
    pub passed: bool,
}

/// 汇总门禁判定（纯函数，可单测）。
pub fn summarize_checks(checks: &[GateCheck]) -> bool {
    checks.iter().all(|c| c.passed)
}

/// 门禁 human 汇总（自解释文本，走 stderr 或 stdout 由调用方定）。
pub fn render_gate_human(outcome: &GateOutcome) -> String {
    let mut lines = vec![format!(
        "== perf gate ({}) · {} 条断言 ==",
        outcome.config,
        outcome.checks.len()
    )];
    for c in &outcome.checks {
        let mark = if c.passed { "PASS" } else { "FAIL" };
        lines.push(format!(
            "  [{mark}] {:<16} {} {} {:.1} {}（阈值 {} {:.1}）",
            c.metric, c.stage, c.relation, c.measured, c.unit, c.relation, c.threshold
        ));
        if !c.passed {
            lines.push(format!("        {}", c.detail));
        }
    }
    let verdict = if outcome.passed { "PASS" } else { "FAIL" };
    lines.push(format!("== 门禁判定: {verdict} =="));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(stage: &str, eps: f64, n: u64) -> WallRow {
        WallRow {
            stage: stage.to_string(),
            eps,
            n,
            rounds: 1,
        }
    }

    // ---- 墙表读写 round-trip ----

    #[test]
    fn wall_row_renders_and_parses() {
        // 墙表文件格式 = eps 四舍五入到整数（与 perf-diag 既有输出一致），
        // 因此 round-trip 断言用整数值 EPS。
        let r = row("rules", 168_129.0, 1_000_000);
        let line = render_wall_row(&r);
        assert_eq!(line, "rules  eps=168129 n=1000000 rounds=1");
        let parsed = parse_wall_row(&line).expect("round-trip");
        assert_eq!(parsed, r);
    }

    #[test]
    fn wall_table_skips_junk_lines_and_blanks() {
        let text = "rules  eps=168129 n=1000000 rounds=1\n\nnot-a-wall-line\nfloor  eps=17000000 n=1000000 rounds=3\n";
        let rows = parse_wall_table(text);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].stage, "rules");
        assert_eq!(rows[0].rounds, 1);
        assert_eq!(rows[1].stage, "floor");
        assert_eq!(rows[1].rounds, 3);
    }

    #[test]
    fn read_wall_file_errors_on_missing_and_empty() {
        let dir = std::env::temp_dir();
        let missing = dir.join(format!("wfgen_gate_missing_{}.txt", std::process::id()));
        let err = read_wall_file(&missing).unwrap_err();
        assert!(err.to_string().contains("reading wall table"));
        let empty = dir.join(format!("wfgen_gate_empty_{}.txt", std::process::id()));
        std::fs::write(&empty, "garbage only\n").unwrap();
        let err = read_wall_file(&empty).unwrap_err();
        let _ = std::fs::remove_file(&empty);
        assert!(err.to_string().contains("无有效墙表行"));
    }

    // ---- 配置校验 ----

    #[test]
    fn gate_config_requires_at_least_one_assertion() {
        let cfg = GateConfig::default();
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("未配置任何断言"));
    }

    #[test]
    fn gate_config_per_rule_needs_rule_count() {
        let cfg = GateConfig {
            rule_count: 0,
            absolute: AbsoluteCaps {
                rules_eps_min: None,
                per_rule_ns_max: Some(200.0),
            },
            relative: RelativeCaps::default(),
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("rule_count"));
    }

    #[test]
    fn gate_config_relative_needs_baseline_and_pct() {
        // baseline 给了但没 max_regression_pct → 报错
        let cfg = GateConfig {
            rule_count: 0,
            absolute: AbsoluteCaps::default(),
            relative: RelativeCaps {
                baseline: Some(PathBuf::from("data/base.txt")),
                stages: default_rel_stages(),
                max_regression_pct: None,
            },
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("max_regression_pct"));
        // pct 越界
        let cfg = GateConfig {
            rule_count: 0,
            absolute: AbsoluteCaps::default(),
            relative: RelativeCaps {
                baseline: Some(PathBuf::from("data/base.txt")),
                stages: default_rel_stages(),
                max_regression_pct: Some(150.0),
            },
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("[0,100]"));
    }

    #[test]
    fn gate_config_parses_toml() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("wfgen_gate_cfg_{}.toml", std::process::id()));
        std::fs::write(
            &path,
            r#"
rule_count = 376
[absolute]
rules_eps_min = 150000.0
per_rule_ns_max = 300.0
[relative]
baseline = "data/perf_wall.baseline.txt"
stages = ["rules", "full"]
max_regression_pct = 20.0
"#,
        )
        .unwrap();
        let cfg = GateConfig::load(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(cfg.rule_count, 376);
        assert_eq!(cfg.absolute.rules_eps_min, Some(150_000.0));
        assert_eq!(cfg.absolute.per_rule_ns_max, Some(300.0));
        assert_eq!(cfg.relative.stages, vec!["rules".to_string(), "full".to_string()]);
        assert_eq!(cfg.relative.max_regression_pct, Some(20.0));
    }

    // ---- 门禁评估 ----

    /// 典型墙：floor ~17M eps、rules 168k eps（约 15.7 ns/事件/规则 @376 规则）。
    fn typical_wall() -> Vec<WallRow> {
        vec![
            row("floor", 17_000_000.0, 1_000_000),
            row("rules", 168_000.0, 1_000_000),
            row("full", 150_000.0, 1_000_000),
        ]
    }

    fn base_cfg() -> GateConfig {
        GateConfig {
            rule_count: 376,
            absolute: AbsoluteCaps::default(),
            relative: RelativeCaps::default(),
        }
    }

    #[test]
    fn absolute_checks_pass_within_budget() {
        let mut cfg = base_cfg();
        cfg.absolute.rules_eps_min = Some(150_000.0);
        cfg.absolute.per_rule_ns_max = Some(300.0);
        let checks = evaluate_gate(&cfg, &typical_wall()).unwrap();
        assert_eq!(checks.len(), 2);
        assert!(checks.iter().all(|c| c.passed));
        // 单规则成本近似 (1e9/168k − 1e9/17M)/376 ≈ (5952−58.8)/376 ≈ 15.7 ns
        let per_rule = &checks[1];
        assert!((per_rule.measured - 15.7).abs() < 1.0, "{}", per_rule.measured);
    }

    #[test]
    fn absolute_rules_eps_min_fails_on_slow_set() {
        let mut cfg = base_cfg();
        cfg.absolute.rules_eps_min = Some(200_000.0); // 现 168k < 200k
        let checks = evaluate_gate(&cfg, &typical_wall()).unwrap();
        assert!(!checks[0].passed);
        assert_eq!(checks[0].metric, "rules_set_eps");
        assert!(checks[0].detail.contains("168000"));
    }

    #[test]
    fn absolute_per_rule_cap_catches_expensive_rule() {
        // 单规则成本推到 400 ns/事件/规则（上限 300 → FAIL）：
        // (1e9/eps_r − 1e9/eps_f)/376 = 400 → eps_r ≈ 1e9/(400*376 + 58.8) ≈ 6636
        let mut cfg = base_cfg();
        cfg.absolute.per_rule_ns_max = Some(300.0);
        let wall = vec![row("floor", 17_000_000.0, 1_000_000), row("rules", 6_600.0, 1_000_000)];
        let checks = evaluate_gate(&cfg, &wall).unwrap();
        assert!(!checks[0].passed);
        assert!(checks[0].measured > 300.0, "{}", checks[0].measured);
        assert!(checks[0].detail.contains("每事件每规则"));
    }

    #[test]
    fn per_rule_metric_needs_floor_and_rules_stages() {
        let mut cfg = base_cfg();
        cfg.absolute.per_rule_ns_max = Some(300.0);
        let wall = vec![row("rules", 168_000.0, 1_000_000)]; // 缺 floor
        let err = evaluate_gate(&cfg, &wall).unwrap_err();
        assert!(err.to_string().contains("缺档 `floor`"));
    }

    #[test]
    fn relative_regression_fails_when_below_allowed_floor() {
        // 基线 rules 200k，允许回退 20% → 允许 ≥160k；现 150k → FAIL
        let dir = std::env::temp_dir();
        let base_path = dir.join(format!("wfgen_gate_base_{}.txt", std::process::id()));
        write_baseline(&base_path, &[row("rules", 200_000.0, 1_000_000)]).unwrap();

        let mut cfg = base_cfg();
        cfg.relative.baseline = Some(base_path.clone());
        cfg.relative.max_regression_pct = Some(20.0);
        let wall = vec![row("rules", 150_000.0, 1_000_000)];
        let checks = evaluate_gate(&cfg, &wall).unwrap();
        let _ = std::fs::remove_file(&base_path);
        assert_eq!(checks.len(), 1);
        assert!(!checks[0].passed);
        assert!(checks[0].detail.contains("回退 25.0%"), "{}", checks[0].detail);
    }

    #[test]
    fn relative_regression_passes_within_tolerance() {
        let dir = std::env::temp_dir();
        let base_path = dir.join(format!("wfgen_gate_base2_{}.txt", std::process::id()));
        write_baseline(&base_path, &[row("rules", 200_000.0, 1_000_000)]).unwrap();

        let mut cfg = base_cfg();
        cfg.relative.baseline = Some(base_path.clone());
        cfg.relative.max_regression_pct = Some(20.0);
        let wall = vec![row("rules", 170_000.0, 1_000_000)]; // 回退 15% < 20%
        let checks = evaluate_gate(&cfg, &wall).unwrap();
        let _ = std::fs::remove_file(&base_path);
        assert!(checks[0].passed);
    }

    #[test]
    fn relative_uses_max_n_and_reports_missing_baseline_n() {
        let dir = std::env::temp_dir();
        let base_path = dir.join(format!("wfgen_gate_base3_{}.txt", std::process::id()));
        // 基线只录了小 N（100k）；本次最大 N=1m → 基线缺该 N → 报错提示重录
        write_baseline(&base_path, &[row("rules", 200_000.0, 100_000)]).unwrap();

        let mut cfg = base_cfg();
        cfg.relative.baseline = Some(base_path.clone());
        cfg.relative.max_regression_pct = Some(20.0);
        let wall = vec![row("rules", 168_000.0, 1_000_000), row("rules", 170_000.0, 100_000)];
        let err = evaluate_gate(&cfg, &wall).unwrap_err();
        let _ = std::fs::remove_file(&base_path);
        assert!(err.to_string().contains("同 n-list"), "{err}");
    }

    #[test]
    fn summarize_all_or_nothing() {
        let pass_checks = vec![
            GateCheck {
                metric: "a".into(),
                stage: "rules".into(),
                measured: 1.0,
                threshold: 0.0,
                unit: "u".into(),
                relation: ">=".into(),
                passed: true,
                detail: "".into(),
            },
            GateCheck {
                metric: "b".into(),
                stage: "rules".into(),
                measured: 0.9,
                threshold: 1.0,
                unit: "u".into(),
                relation: "<=".into(),
                passed: true,
                detail: "".into(),
            },
        ];
        assert!(summarize_checks(&pass_checks));
        let mut fail = pass_checks.clone();
        fail[1].passed = false;
        assert!(!summarize_checks(&fail));
    }

    // ---- review 追加：配置值域 / 未知 key / 口径语义 / 输出形态 ----------------

    fn cfg_from_toml(toml_text: &str) -> WfgenResult<GateConfig> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir();
        // 并行测试同 PID：文件名必须唯一（固定名会被并发测试互相覆盖）。
        let path = dir.join(format!(
            "wfgen_gate_extra_{}_{}.toml",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, toml_text).unwrap();
        let res = GateConfig::load(&path);
        let _ = std::fs::remove_file(&path);
        res
    }

    #[test]
    fn gate_config_rejects_non_positive_absolute_caps() {
        // 负值/0 会让断言恒过（拼写级静默失效）——必须显式报错。
        let err = cfg_from_toml("[absolute]\nrules_eps_min = -1.0\n").unwrap_err();
        assert!(err.to_string().contains("必须为正数"), "{err}");
        let err = cfg_from_toml("[absolute]\nrules_eps_min = 0.0\n").unwrap_err();
        assert!(err.to_string().contains("rules_eps_min=0"));
        let err = cfg_from_toml("rule_count = 10\n[absolute]\nper_rule_ns_max = -300.0\n")
            .unwrap_err();
        assert!(err.to_string().contains("per_rule_ns_max=-300"));
        let err = cfg_from_toml("rule_count = 10\n[absolute]\nper_rule_ns_max = 0\n")
            .unwrap_err();
        assert!(err.to_string().contains("必须为正数"));
        // 正值通过。
        assert!(cfg_from_toml("rule_count = 10\n[absolute]\nper_rule_ns_max = 300.0\n").is_ok());
    }

    #[test]
    fn gate_config_rejects_unknown_keys() {
        // 未知 key 静默忽略 = 断言悄悄消失；deny_unknown_fields 让它显式报错。
        let err = cfg_from_toml("rule_cout = 376\n[absolute]\nrules_eps_min = 1.0\n")
            .unwrap_err();
        assert!(err.to_string().contains("unknown field"), "{err}");
        let err = cfg_from_toml("[absolute]\nrule_eps_min = 1.0\n").unwrap_err();
        assert!(err.to_string().contains("unknown field"));
        let err = cfg_from_toml("[relative]\nmax_regresion_pct = 10.0\nbaseline = \"b.txt\"\n")
            .unwrap_err();
        assert!(err.to_string().contains("unknown field"));
        // 合法配置不受影响。
        assert!(cfg_from_toml("[relative]\nbaseline = \"b.txt\"\nmax_regression_pct = 10.0\n")
            .is_ok());
    }

    #[test]
    fn gate_config_relative_pct_without_baseline_is_rejected() {
        // [relative] 只写 max_regression_pct、忘写 baseline → 相对断言无从比
        // （stages 有 serde 默认、pct 无默认，pct 是"想开相对"的唯一可靠信号）。
        let err = cfg_from_toml("[relative]\nmax_regression_pct = 10.0\n").unwrap_err();
        assert!(err.to_string().contains("需配套 baseline"), "{err}");
    }

    #[test]
    fn relative_stages_default_to_rules_when_omitted() {
        // [relative] 未写 stages → serde 默认 ["rules"]，评估产生 1 条相对断言。
        let dir = std::env::temp_dir();
        let base_path = dir.join(format!("wfgen_gate_defstage_{}.txt", std::process::id()));
        write_baseline(&base_path, &[row("rules", 200_000.0, 1_000_000)]).unwrap();
        let cfg = cfg_from_toml(&format!(
            "[relative]\nbaseline = \"{}\"\nmax_regression_pct = 20.0\n",
            base_path.display()
        ))
        .unwrap();
        assert_eq!(cfg.relative.stages, vec!["rules".to_string()]);
        let wall = vec![row("rules", 180_000.0, 1_000_000)];
        let checks = evaluate_gate(&cfg, &wall).unwrap();
        let _ = std::fs::remove_file(&base_path);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].metric, "eps_regression");
        assert_eq!(checks[0].stage, "rules");
    }

    #[test]
    fn absolute_gate_uses_max_n_row() {
        // 门禁口径 = 取该档最大 N 行（固定开销摊薄后的每事件成本最稳）。
        // 小 N 好、大 N 差 → 必须 FAIL（不拿小 N 的好数字糊弄）。
        let mut cfg = base_cfg();
        cfg.absolute.rules_eps_min = Some(100_000.0);
        let wall = vec![
            row("rules", 2_000_000.0, 100_000), // 小 N：固定开销摊薄前虚高
            row("rules", 60_000.0, 1_000_000),  // 大 N：稳态真值 60k < 100k
        ];
        let checks = evaluate_gate(&cfg, &wall).unwrap();
        assert!(!checks[0].passed, "门禁必须用最大 N 行，现取 {:?}", checks[0].measured);
        assert_eq!(checks[0].measured, 60_000.0);

        // 反向：小 N 差、大 N 好 → 通过（测量噪声/启动固定开销不误伤）。
        let wall = vec![
            row("rules", 30_000.0, 100_000),
            row("rules", 200_000.0, 1_000_000),
        ];
        let checks = evaluate_gate(&cfg, &wall).unwrap();
        assert!(checks[0].passed);
        assert_eq!(checks[0].measured, 200_000.0);
    }

    #[test]
    fn evaluate_rejects_empty_current_wall() {
        let mut cfg = base_cfg();
        cfg.absolute.rules_eps_min = Some(100_000.0);
        let err = evaluate_gate(&cfg, &[]).unwrap_err();
        assert!(err.to_string().contains("墙表为空"));
    }

    #[test]
    fn per_rule_ns_clamps_noise_to_zero() {
        // 噪声可能让 rules 档 EPS 反而高于 floor（增量负）——按 0 处理并 PASS，
        // 不产生负成本或误报。
        let mut cfg = base_cfg();
        cfg.absolute.per_rule_ns_max = Some(10.0);
        let wall = vec![row("floor", 5_000_000.0, 1_000_000), row("rules", 9_000_000.0, 1_000_000)];
        let checks = evaluate_gate(&cfg, &wall).unwrap();
        assert_eq!(checks[0].measured, 0.0);
        assert!(checks[0].passed);
    }

    #[test]
    fn combined_abs_and_rel_produce_ordered_checks() {
        // 绝对 2 条 + 相对 1 条（默认 rules 档）→ 检查顺序：abs 先、rel 后。
        let dir = std::env::temp_dir();
        let base_path = dir.join(format!("wfgen_gate_combo_{}.txt", std::process::id()));
        write_baseline(&base_path, &[row("rules", 200_000.0, 1_000_000)]).unwrap();

        let mut cfg = base_cfg();
        cfg.absolute.rules_eps_min = Some(150_000.0);
        cfg.absolute.per_rule_ns_max = Some(300.0);
        cfg.relative.baseline = Some(base_path.clone());
        cfg.relative.max_regression_pct = Some(20.0);

        let checks = evaluate_gate(&cfg, &typical_wall()).unwrap();
        let _ = std::fs::remove_file(&base_path);
        assert_eq!(checks.len(), 3);
        assert_eq!(checks[0].metric, "rules_set_eps");
        assert_eq!(checks[1].metric, "per_rule_ns");
        assert_eq!(checks[2].metric, "eps_regression");
        assert!(checks.iter().all(|c| c.passed));
    }

    #[test]
    fn render_gate_human_marks_failures_with_detail() {
        let outcome = GateOutcome {
            config: "conf/perf-gate.toml".to_string(),
            passed: false,
            checks: vec![
                GateCheck {
                    metric: "rules_set_eps".to_string(),
                    stage: "rules".to_string(),
                    measured: 120_000.0,
                    threshold: 150_000.0,
                    unit: "eps".to_string(),
                    relation: ">=".to_string(),
                    passed: true,
                    detail: "ok".to_string(),
                },
                GateCheck {
                    metric: "eps_regression".to_string(),
                    stage: "rules".to_string(),
                    measured: 150_000.0,
                    threshold: 160_000.0,
                    unit: "eps".to_string(),
                    relation: ">=".to_string(),
                    passed: false,
                    detail: "rules 档 EPS=150000 vs 基线 200000（回退 25.0%，允许 ≤20.0%）"
                        .to_string(),
                },
            ],
        };
        let text = render_gate_human(&outcome);
        assert!(text.contains("== perf gate (conf/perf-gate.toml)"));
        assert!(text.contains("[PASS]"));
        assert!(text.contains("[FAIL]"));
        assert!(text.contains("回退 25.0%"), "失败明细必须自解释: {text}");
        assert!(text.contains("== 门禁判定: FAIL =="));
    }

    #[test]
    fn write_baseline_creates_parent_dirs_and_roundtrips() {
        let dir = std::env::temp_dir().join(format!("wfgen_gate_nested_{}", std::process::id()));
        let path = dir.join("data/perf_wall.baseline.txt");
        let rows = vec![row("rules", 168_000.0, 1_000_000), row("floor", 17_000_000.0, 1_000_000)];
        write_baseline(&path, &rows).unwrap();
        let parsed = read_wall_file(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(parsed, rows);
    }
}
