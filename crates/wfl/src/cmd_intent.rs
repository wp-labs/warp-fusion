use std::io::IsTerminal;
use std::path::PathBuf;
use std::process;

use orion_error::conversion::SourceErr;
use serde::Serialize;

use crate::error::{self, WflReason, WflResult, WflStructExt};
use wf_config::ConfigVarContext;
use wf_config::project::{load_schemas, parse_vars};
use wf_engine::match_engine::contract::run_test;
use wf_lang::ast::{CmpOp, ExpectStmt, TestBlock};
use wf_lang::plan::RulePlan;

const GREEN: &str = "\x1b[1;32m";
const RED: &str = "\x1b[1;31m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

/// 意图样本类别：正样本 = "这条输入应触发规则"（漏报检查）；
/// 负样本 = "这条输入不应触发规则"（误报检查）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleKind {
    Positive,
    Negative,
}

impl SampleKind {
    /// 序列化用标签（report.kind 字段）。
    pub fn as_str(self) -> &'static str {
        match self {
            SampleKind::Positive => "positive",
            SampleKind::Negative => "negative",
        }
    }

    /// 归一化后的意图断言（正样本一律按"至少 1 命中"判定，
    /// 不继承 expect 里的具体阈值——意图是检出/不检出）。
    fn expect_text(self) -> &'static str {
        match self {
            SampleKind::Positive => "hits >= 1",
            SampleKind::Negative => "hits == 0",
        }
    }

    /// 失败样本的语义标签：漏报（正样本 0 命中）/ 误报（负样本 >0 命中）。
    fn failure_tag(self, actual_hits: usize) -> String {
        match self {
            SampleKind::Positive => format!(
                "漏报：正样本应触发（expect {}），实际 {} 命中——规则漏检该输入",
                self.expect_text(),
                actual_hits
            ),
            SampleKind::Negative => format!(
                "误报：负样本不应触发（expect {}），实际 {} 命中——规则误报该输入",
                self.expect_text(),
                actual_hits
            ),
        }
    }
}

/// 单条意图样本的执行结果。
#[derive(Debug, Clone, Serialize)]
pub struct IntentSampleReport {
    /// 样本名（.wfi 中 test 块名）。
    pub name: String,
    /// 样本类别：positive（该检出）/ negative（不该检出）。
    pub kind: String,
    /// 规则名。
    pub rule: String,
    /// 样本是否通过（引擎对 expect 全量断言的结果）。
    pub passed: bool,
    /// 引擎实际命中数。
    pub hits: usize,
    /// 引擎执行错误（样本本身无法运行——输入引用不存在的 alias/字段、
    /// schema 不匹配等）。这类失败是"样本写错"，不计入漏报/误报。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 失败明细：引擎断言失败 + 漏报/误报语义标签。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<String>,
}

/// 意图验证汇总。
#[derive(Debug, Clone, Copy, Serialize)]
pub struct IntentSummary {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    /// 样本本身无法运行（输入引用不存在字段/alias 等），是样本错误不是规则缺陷。
    pub errors: usize,
    /// 漏报：正样本应触发却 0 命中。
    pub false_negatives: usize,
    /// 误报：负样本不应触发却命中。
    pub false_positives: usize,
}

/// `wfl intent --format json` 的结构化报告（schema v1）。
#[derive(Debug, Clone, Serialize)]
pub struct IntentReport {
    pub schema: String,
    /// 被测规则文件。
    pub rule_file: String,
    /// 意图样本文件（.wfi）。
    pub intent_file: String,
    pub summary: IntentSummary,
    pub samples: Vec<IntentSampleReport>,
    pub status: String,
    pub verdict: String,
}

/// 判定 test 块是正样本还是负样本（严格 canonical 形状）：
/// - 正样本：`hits >= 1` 或 `hits > 0`（该检出，漏报检查）
/// - 负样本：`hits == 0` / `hits <= 0` / `hits < 1`（不该检出，误报检查）
///
/// 无法归类的形状如实拒绝而非静默猜测：
/// - 无 hits 断言 / 命中数阈值（如 `hits == 5`、`hits >= 3`）→ None
/// - 同时含正、负断言（自相矛盾）→ None
///
/// 调用方对 None 报错——AI 拿到的不是"没测到"，而是明确的"样本不合意图格式"。
fn classify(test: &TestBlock) -> Option<SampleKind> {
    let mut positive = false;
    let mut negative = false;
    for e in &test.expect {
        let ExpectStmt::Hits { cmp, count } = e else {
            continue; // hit[i] 字段断言不参与意图分类（由引擎全量校验）
        };
        match (cmp, count) {
            (CmpOp::Ge, n) if *n == 1 => positive = true,
            (CmpOp::Gt, n) if *n == 0 => positive = true,
            (CmpOp::Eq, n) if *n == 0 => negative = true,
            (CmpOp::Le, n) if *n == 0 => negative = true,
            (CmpOp::Lt, n) if *n == 1 => negative = true,
            _ => return None, // hits >= 0/>= 3/== 2/… 非 0-1 意图形状
        }
    }
    match (positive, negative) {
        (true, false) => Some(SampleKind::Positive),
        (false, true) => Some(SampleKind::Negative),
        _ => None, // 无 hits 断言，或正/负断言并存（矛盾）
    }
}

/// 执行单个样本（test 块）并返回报告项。human 输出走 stderr；json 模式静默。
///
/// 通过与否以引擎 `run_test` 的 expect 全量校验为准（含样本里附加的
/// hit[i].字段 断言）；漏报/误报标签按实际命中数与类别判定，只加在
/// 对应语义失败上（正样本 0 命中 / 负样本 >0 命中）。
fn run_sample(
    test: &TestBlock,
    kind: SampleKind,
    plan: &RulePlan,
    time_field: Option<&str>,
    color: bool,
    json: bool,
) -> IntentSampleReport {
    let name = test.name.clone();
    let rule = test.rule_name.clone();

    match run_test(test, plan, time_field.map(str::to_string)) {
        Ok(result) => {
            let mut failures = result.failures;
            if !result.passed {
                match kind {
                    SampleKind::Positive if result.output_count == 0 => {
                        failures.push(kind.failure_tag(0));
                    }
                    SampleKind::Negative if result.output_count > 0 => {
                        failures.push(kind.failure_tag(result.output_count));
                    }
                    _ => {}
                }
            }
            if !json {
                print_head(color, result.passed, &name, kind, &rule);
                if !result.passed {
                    print_failures(color, &failures);
                }
            }
            IntentSampleReport {
                name,
                kind: kind.as_str().to_string(),
                rule,
                passed: result.passed,
                hits: result.output_count,
                error: None,
                failures,
            }
        }
        Err(e) => {
            let msg = e.report().render().to_string();
            if !json {
                print_head(color, false, &name, kind, &rule);
                eprintln!("      error: {msg}");
            }
            IntentSampleReport {
                name,
                kind: kind.as_str().to_string(),
                rule,
                passed: false,
                hits: 0,
                error: Some(msg.clone()),
                failures: vec![msg],
            }
        }
    }
}

fn print_head(color: bool, passed: bool, name: &str, kind: SampleKind, rule: &str) {
    let label = if passed { "PASS" } else { "FAIL" };
    if color {
        let c = if passed { GREEN } else { RED };
        eprintln!(
            "{c}{label}{RESET}  {name} {DIM}[{}] ({rule}){RESET}",
            kind.as_str()
        );
    } else {
        eprintln!("{label}  {name} [{}] ({rule})", kind.as_str());
    }
}

fn print_failures(color: bool, failures: &[String]) {
    for f in failures {
        if color {
            eprintln!("      {RED}{f}{RESET}");
        } else {
            eprintln!("      {f}");
        }
    }
}

/// 汇总统计（纯函数，可单测）。漏报/误报只从"跑完的样本"计：
/// 执行错误（样本写错）不计入规则缺陷。
fn summarize(samples: &[IntentSampleReport]) -> IntentSummary {
    let total = samples.len();
    let passed = samples.iter().filter(|s| s.passed).count();
    let failed = total - passed;
    let errors = samples.iter().filter(|s| s.error.is_some()).count();
    let false_negatives = samples
        .iter()
        .filter(|s| s.error.is_none() && s.kind == "positive" && s.hits == 0 && !s.passed)
        .count();
    let false_positives = samples
        .iter()
        .filter(|s| s.error.is_none() && s.kind == "negative" && s.hits > 0 && !s.passed)
        .count();
    IntentSummary {
        total,
        passed,
        failed,
        errors,
        false_negatives,
        false_positives,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    rule_file: PathBuf,
    intent_file: PathBuf,
    schemas: Vec<String>,
    vars: Vec<String>,
    format: String,
) -> WflResult<()> {
    let json = matches!(format.as_str(), "json" | "jsonl");

    let cwd = std::env::current_dir().source_err(WflReason::Io, "reading cwd")?;
    let mut var_map = parse_vars(&vars).wfl()?;
    var_map
        .entry("WORK_DIR".to_string())
        .or_insert_with(|| cwd.to_string_lossy().to_string());
    let ctx = ConfigVarContext::from_explicit_vars(var_map);
    let color = !json && std::io::stderr().is_terminal();

    // 加载 schema（样本行/规则字段校验用）
    let all_schemas = load_schemas(&schemas, &cwd).wfl()?;

    // 规则文件：加载 + 编译 → plans（被测对象）。规则文件自带 test 块不参与，
    // 意图样本一律来自 .wfi。
    let rule_wfl = crate::load_wfl_with_imports(&rule_file, &ctx, &cwd)?;
    let plans = wf_lang::compile_wfl(&rule_wfl, &all_schemas).wfl()?;

    // 意图文件：.wfi = 纯 test 块集合（合法 .wfl 子集，无 rule）。
    // 复用公开 parse_wfl 解析——test 块即样本，expect 语义即意图。
    let intent_src = std::fs::read_to_string(&intent_file)
        .source_err(WflReason::Io, format!("reading {}", intent_file.display()))?;
    let intent_wfl = wf_lang::parse_wfl(&intent_src).wfl()?;

    if intent_wfl.tests.is_empty() {
        return error::fail(
            WflReason::Validation,
            format!(
                "意图文件 {} 不含 test 块（.wfi = 正/负样本集合，每个 test 块即一条样本：\
                 expect {{ hits >= 1 }} = 正样本(该检出)，expect {{ hits == 0 }} = 负样本(不该检出))",
                intent_file.display()
            ),
        );
    }

    let mut samples: Vec<IntentSampleReport> = Vec::new();

    for test in &intent_wfl.tests {
        let Some(kind) = classify(test) else {
            return error::fail(
                WflReason::Validation,
                format!(
                    "样本 `{}` 无法分类：expect 须含 hits >= 1（正样本，该检出）\
                     或 hits == 0（负样本，不该检出）且不自相矛盾",
                    test.name
                ),
            );
        };

        let plan = match plans.iter().find(|p| p.name == test.rule_name) {
            Some(p) => p,
            None => {
                return error::fail(
                    WflReason::Validation,
                    format!(
                        "样本 `{}` 引用的规则 `{}` 不在规则文件 {} 中",
                        test.name,
                        test.rule_name,
                        rule_file.display()
                    ),
                );
            }
        };

        let time_field = all_schemas
            .iter()
            .find(|s| plan.binds.iter().any(|b| b.window == s.name))
            .and_then(|s| s.time_field.clone());

        let sample = run_sample(test, kind, plan, time_field.as_deref(), color, json);
        samples.push(sample);
    }

    let summary = summarize(&samples);
    let status = if summary.failed > 0 { "fail" } else { "pass" };
    let verdict = if summary.failed > 0 { "FAIL" } else { "PASS" };

    if json {
        let report = IntentReport {
            schema: "wfl-intent-report/v1".to_string(),
            rule_file: rule_file.display().to_string(),
            intent_file: intent_file.display().to_string(),
            summary,
            samples,
            status: status.to_string(),
            verdict: verdict.to_string(),
        };
        let out = serde_json::to_string_pretty(&report)
            .source_err(WflReason::Serialization, "serializing intent report")?;
        println!("{out}");
    } else if summary.total > 0 {
        print_summary_human(color, &summary);
    } else {
        eprintln!("No samples found.");
    }

    if summary.failed > 0 {
        process::exit(1);
    }

    Ok(())
}

fn print_summary_human(color: bool, s: &IntentSummary) {
    let mut extra = String::new();
    if s.errors > 0 {
        extra.push_str(&format!(" · 样本错误={}", s.errors));
    }
    if s.false_negatives > 0 {
        extra.push_str(&format!(" · 漏报={}", s.false_negatives));
    }
    if s.false_positives > 0 {
        extra.push_str(&format!(" · 误报={}", s.false_positives));
    }
    if color {
        let c = if s.failed > 0 { RED } else { GREEN };
        eprintln!(
            "\n{BOLD}{total} samples: {GREEN}{passed} passed{RESET}{BOLD}, {c}{failed} failed{RESET}{extra}",
            total = s.total,
            passed = s.passed,
            failed = s.failed,
        );
    } else {
        eprintln!(
            "\n{} samples: {} passed, {} failed{extra}",
            s.total, s.passed, s.failed
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 从 .wfi 源字符串解析出样本 test 块（绕过 wf-lang AST 的 non_exhaustive，
    /// 与 L2 gen_negatives 测试同一手法——直接构造 struct 字面量会撞 E0639）。
    fn parse_sample(expect_line: &str, extra: &str) -> TestBlock {
        let src = format!(
            r#"test t1 for r1 {{
  input {{
    row(fail, action = "failed", sip = "10.0.0.1");
  }}
  expect {{
    {expect_line}
    {extra}
  }}
}}"#
        );
        let file = wf_lang::parse_wfl(&src).expect("parse .wfi source");
        assert_eq!(file.tests.len(), 1);
        file.tests.into_iter().next().unwrap()
    }

    fn classify_src(expect_line: &str) -> Option<SampleKind> {
        classify(&parse_sample(expect_line, ""))
    }

    // ---- classify：canonical 形状 ----

    #[test]
    fn classify_canonical_positive_shapes() {
        assert_eq!(classify_src("hits >= 1;"), Some(SampleKind::Positive));
        assert_eq!(classify_src("hits > 0;"), Some(SampleKind::Positive));
    }

    #[test]
    fn classify_canonical_negative_shapes() {
        assert_eq!(classify_src("hits == 0;"), Some(SampleKind::Negative));
        assert_eq!(classify_src("hits <= 0;"), Some(SampleKind::Negative));
        assert_eq!(classify_src("hits < 1;"), Some(SampleKind::Negative));
    }

    #[test]
    fn classify_non_canonical_threshold_is_none() {
        // 命中数阈值（>= 3 / == 5 / > 1 / < 0）不是"该检出/不该检出"意图形状
        assert!(classify_src("hits == 5;").is_none());
        assert!(classify_src("hits >= 3;").is_none());
        assert!(classify_src("hits > 1;").is_none());
        assert!(classify_src("hits < 0;").is_none());
        // hits >= 0 恒真（空转断言），也不表达检出意图
        assert!(classify_src("hits >= 0;").is_none());
    }

    #[test]
    fn classify_no_hits_expect_is_none() {
        // 只有 hit[i] 字段断言 → 无 hits 意图，拒绝
        let sample = parse_sample("", "hit[0].score == 70.0;");
        assert!(classify(&sample).is_none());
    }

    #[test]
    fn classify_contradictory_block_is_none() {
        // 正/负断言并存 = 自相矛盾（引擎永远无法满足），拒绝而非静默归类
        let sample = parse_sample("hits >= 1;", "hits == 0;");
        assert!(classify(&sample).is_none());
    }

    #[test]
    fn classify_hit_assert_does_not_override_kind() {
        // 正样本可附加 hit[i] 字段断言（增强精确性），不改变意图类别
        let sample = parse_sample("hits >= 1;", "hit[0].score == 70.0;");
        assert_eq!(classify(&sample), Some(SampleKind::Positive));
    }

    // ---- 失败标签 ----

    #[test]
    fn failure_tags_speak_fp_and_fn() {
        let fn_tag = SampleKind::Positive.failure_tag(0);
        assert!(fn_tag.contains("漏报"));
        assert!(fn_tag.contains("hits >= 1"));
        assert!(fn_tag.contains("0 命中"));

        let fp_tag = SampleKind::Negative.failure_tag(3);
        assert!(fp_tag.contains("误报"));
        assert!(fp_tag.contains("hits == 0"));
        assert!(fp_tag.contains("3 命中"));
    }

    // ---- summarize：漏报/误报/样本错误的归属 ----

    fn report(name: &str, kind: &str, passed: bool, hits: usize, error: Option<String>) -> IntentSampleReport {
        IntentSampleReport {
            name: name.to_string(),
            kind: kind.to_string(),
            rule: "r1".to_string(),
            passed,
            hits,
            error,
            failures: vec![],
        }
    }

    #[test]
    fn summarize_counts_fn_fp_and_errors_separately() {
        let samples = vec![
            report("pass_pos", "positive", true, 2, None),
            // 正样本 0 命中 → 漏报
            report("fn_pos", "positive", false, 0, None),
            // 负样本 2 命中 → 误报
            report("fp_neg", "negative", false, 2, None),
            // 负样本 0 命中通过
            report("pass_neg", "negative", true, 0, None),
            // 正样本执行错误（样本写错）→ 只计 failed+errors，不计漏报
            report("err_pos", "positive", false, 0, Some("row alias `nope` not found".to_string())),
        ];
        let s = summarize(&samples);
        assert_eq!(s.total, 5);
        assert_eq!(s.passed, 2);
        assert_eq!(s.failed, 3);
        assert_eq!(s.errors, 1);
        assert_eq!(s.false_negatives, 1);
        assert_eq!(s.false_positives, 1);
    }

    #[test]
    fn summarize_extra_assert_failure_is_not_fn() {
        // 正样本命中 1 次但附加断言（如 hit[0].score）失败 → FAIL 但不算漏报
        let samples = vec![report("strength_pos", "positive", false, 1, None)];
        let s = summarize(&samples);
        assert_eq!(s.failed, 1);
        assert_eq!(s.false_negatives, 0);
        assert_eq!(s.false_positives, 0);
    }

    #[test]
    fn summarize_negative_error_is_not_fp() {
        // 负样本执行错误：hits 记 0，若计入会虚增误报 → 必须被 error 排除
        let samples = vec![report(
            "err_neg",
            "negative",
            false,
            0,
            Some("boom".to_string()),
        )];
        let s = summarize(&samples);
        assert_eq!(s.failed, 1);
        assert_eq!(s.errors, 1);
        assert_eq!(s.false_negatives, 0);
        assert_eq!(s.false_positives, 0);
    }
}
