use std::io::IsTerminal;
use std::path::PathBuf;
use std::process;

use orion_error::conversion::SourceErr;
use serde::Serialize;

use crate::error::{self, WflReason, WflResult, WflStructExt};
use wf_config::ConfigVarContext;
use wf_config::project::{load_schemas, parse_vars};
use wf_engine::match_engine::contract::run_test;
use wf_lang::ast::PermutationMode;

const GREEN: &str = "\x1b[1;32m";
const RED: &str = "\x1b[1;31m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

/// 单个测试的结构化结果（L1 回执：AI/CI 程序化消费）。
#[derive(Debug, Clone, Serialize)]
pub struct TestCaseReport {
    pub name: String,
    pub rule: String,
    pub passed: bool,
    /// 本测试触发的告警数（= 引擎实际 hits）。
    pub output_count: usize,
    /// 失败断言列表（human 可读文本，含 expected/got；随 run 前缀 run N:）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<String>,
}

/// 汇总统计。
#[derive(Debug, Clone, Serialize)]
pub struct TestSummary {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
}

/// `wfl test --format json` 的结构化回执（schema v1）。
///
/// 语义与退出码一致：`failed > 0` → `status="fail"` / `verdict="FAIL"` / exit 1；
/// 全部通过（含无测试的空跑）→ `status="pass"` / `verdict="PASS"` / exit 0。
#[derive(Debug, Clone, Serialize)]
pub struct TestReport {
    pub schema: String,
    pub rule_file: String,
    /// 本次请求的乱序/多轮参数（测试块自身 options 见逐条 tests）。
    pub shuffle: bool,
    pub runs: Option<usize>,
    pub summary: TestSummary,
    pub tests: Vec<TestCaseReport>,
    pub status: String,
    pub verdict: String,
}

/// 执行单个 test 变体并生成报告项（human 输出走 stderr；json 模式静默）。
/// 不 exit —— 退出码由调用方按汇总决定。
fn run_one(
    test: &wf_lang::ast::TestBlock,
    plan: &wf_lang::plan::RulePlan,
    time_field: Option<&str>,
    color: bool,
    json: bool,
) -> TestCaseReport {
    match run_test(test, plan, time_field.map(str::to_string)) {
        Ok(result) => {
            let passed = result.passed;
            if passed {
                if color {
                    eprintln!(
                        "{GREEN}PASS{RESET}  {} {DIM}({}){RESET}",
                        test.name, test.rule_name
                    );
                } else if !json {
                    eprintln!("PASS  {} ({})", test.name, test.rule_name);
                }
            } else if color {
                eprintln!(
                    "{RED}FAIL{RESET}  {} {DIM}({}){RESET}",
                    test.name, test.rule_name
                );
                for f in &result.failures {
                    eprintln!("      {RED}{f}{RESET}");
                }
            } else if !json {
                eprintln!("FAIL  {} ({})", test.name, test.rule_name);
                for f in &result.failures {
                    eprintln!("      {}", f);
                }
            }
            TestCaseReport {
                name: result.test_name,
                rule: result.rule_name,
                passed,
                output_count: result.output_count,
                failures: result.failures,
            }
        }
        Err(e) => {
            let msg = e.report().render().to_string();
            if color {
                eprintln!(
                    "{RED}FAIL{RESET}  {} {DIM}({}){RESET} — error: {}",
                    test.name,
                    test.rule_name,
                    e.report().render()
                );
            } else if !json {
                eprintln!(
                    "FAIL  {} ({}) — error: {}",
                    test.name,
                    test.rule_name,
                    e.report().render()
                );
            }
            TestCaseReport {
                name: test.name.clone(),
                rule: test.rule_name.clone(),
                passed: false,
                output_count: 0,
                failures: vec![msg],
            }
        }
    }
}

pub fn run(
    file: PathBuf,
    schemas: Vec<String>,
    vars: Vec<String>,
    shuffle: bool,
    runs: Option<usize>,
    gen_negatives: bool,
    format: String,
) -> WflResult<()> {
    if let Some(0) = runs {
        return error::fail(WflReason::Validation, "--runs must be greater than 0");
    }
    let json = matches!(format.as_str(), "json" | "jsonl");

    let cwd = std::env::current_dir().source_err(WflReason::Io, "reading cwd")?;
    let mut var_map = parse_vars(&vars).wfl()?;
    var_map
        .entry("WORK_DIR".to_string())
        .or_insert_with(|| cwd.to_string_lossy().to_string());
    let ctx = ConfigVarContext::from_explicit_vars(var_map);
    let color = !json && std::io::stderr().is_terminal();

    // Load schemas
    let all_schemas = load_schemas(&schemas, &cwd).wfl()?;

    // Load and preprocess the .wfl file + parse `use` imports (issue #73)
    let wfl_file = crate::load_wfl_with_imports(&file, &ctx, &cwd)?;

    // Compile rules into plans
    let plans = wf_lang::compile_wfl(&wfl_file, &all_schemas).wfl()?;

    let mut cases: Vec<TestCaseReport> = Vec::new();

    for test in &wfl_file.tests {
        let plan = match plans.iter().find(|p| p.name == test.rule_name) {
            Some(p) => p,
            None => {
                if color {
                    eprintln!(
                        "{RED}FAIL{RESET}  {} — target rule `{}` not found",
                        test.name, test.rule_name
                    );
                } else if !json {
                    eprintln!(
                        "FAIL  {} — target rule `{}` not found",
                        test.name, test.rule_name
                    );
                }
                cases.push(TestCaseReport {
                    name: test.name.clone(),
                    rule: test.rule_name.clone(),
                    passed: false,
                    output_count: 0,
                    failures: vec![format!("target rule `{}` not found", test.rule_name)],
                });
                continue;
            }
        };

        let time_field = all_schemas
            .iter()
            .find(|s| plan.binds.iter().any(|b| b.window == s.name))
            .and_then(|s| s.time_field.clone());

        let mut effective_test = test.clone();
        if shuffle || runs.is_some() {
            let mut opts = effective_test.options.unwrap_or_default();
            if shuffle {
                opts.permutation = Some(PermutationMode::Shuffle);
            }
            if let Some(n) = runs {
                opts.runs = Some(n);
            } else if shuffle && opts.runs.is_none() {
                opts.runs = Some(10);
            }
            effective_test.options = Some(opts);
        }

        let baseline = run_one(&effective_test, plan, time_field.as_deref(), color, json);
        let baseline_passed = baseline.passed;
        cases.push(baseline);

        // L2：基线通过后，追加 bind-guard 反例变体并逐个验证（hits 应不变）。
        if gen_negatives && baseline_passed {
            let negative_cases =
                crate::gen_negatives::gen_negative_cases(plan, &effective_test);
            if !negative_cases.is_empty() {
                for nc in negative_cases {
                    let mut variant = effective_test.clone();
                    variant.name = format!("{} [neg: {}]", effective_test.name, nc.desc);
                    variant.input = nc.input;
                    // 反例变体保持原始行序 + 单轮：乱序会改变窗口行为 → 假失败。
                    // 反例断言 = “追加反例行后 hits 不变”，须与基线同序可比。
                    variant.options = None;
                    let report = run_one(&variant, plan, time_field.as_deref(), color, json);
                    cases.push(report);
                }
            }
        }
    }

    let total = cases.len();
    let passed = cases.iter().filter(|c| c.passed).count();
    let failed = total - passed;
    let status = if failed > 0 { "fail" } else { "pass" };
    let verdict = if failed > 0 { "FAIL" } else { "PASS" };

    if json {
        let report = TestReport {
            schema: "wfl-test-report/v1".to_string(),
            rule_file: file.display().to_string(),
            shuffle,
            runs,
            summary: TestSummary {
                total,
                passed,
                failed,
            },
            tests: cases,
            status: status.to_string(),
            verdict: verdict.to_string(),
        };
        let out = serde_json::to_string_pretty(&report)
            .source_err(WflReason::Serialization, "serializing test report")?;
        println!("{out}");
    } else if total > 0 {
        if color {
            eprintln!(
                "\n{BOLD}{total} tests: {GREEN}{passed} passed{RESET}{BOLD}, {}{failed} failed{RESET}",
                if failed > 0 { RED } else { GREEN },
            );
        } else {
            eprintln!("\n{} tests: {} passed, {} failed", total, passed, failed);
        }
    } else {
        eprintln!("No tests found.");
    }

    if failed > 0 {
        process::exit(1);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_report_serializes_with_schema_v1() {
        let report = TestReport {
            schema: "wfl-test-report/v1".to_string(),
            rule_file: "rules/brute_force.wfl".to_string(),
            shuffle: true,
            runs: Some(3),
            summary: TestSummary {
                total: 2,
                passed: 1,
                failed: 1,
            },
            tests: vec![
                TestCaseReport {
                    name: "close_hit".to_string(),
                    rule: "brute_force_then_scan".to_string(),
                    passed: true,
                    output_count: 1,
                    failures: vec![],
                },
                TestCaseReport {
                    name: "below_threshold".to_string(),
                    rule: "brute_force_then_scan".to_string(),
                    passed: false,
                    output_count: 0,
                    failures: vec!["run 1: hits: expected hits == 5, got 1".to_string()],
                },
            ],
            status: "fail".to_string(),
            verdict: "FAIL".to_string(),
        };

        let json = serde_json::to_string_pretty(&report).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

        // schema 版本化（L1 回执契约）
        assert_eq!(v["schema"], "wfl-test-report/v1");
        assert_eq!(v["status"], "fail");
        assert_eq!(v["verdict"], "FAIL");
        assert_eq!(v["shuffle"], true);
        assert_eq!(v["runs"], 3);

        // 摘要
        assert_eq!(v["summary"]["total"], 2);
        assert_eq!(v["summary"]["passed"], 1);
        assert_eq!(v["summary"]["failed"], 1);

        // 逐测试：通过项不序列化空 failures（skip_serializing_if），失败项带 expected/got
        let tests = v["tests"].as_array().unwrap();
        assert_eq!(tests.len(), 2);
        assert!(tests[0].get("failures").is_none());
        assert_eq!(tests[0]["output_count"], 1);
        assert_eq!(
            tests[1]["failures"][0],
            "run 1: hits: expected hits == 5, got 1"
        );
    }

    #[test]
    fn test_empty_report_is_pass() {
        // 无测试的空跑 = pass / exit 0（与 human 模式既有语义一致）
        let report = TestReport {
            schema: "wfl-test-report/v1".to_string(),
            rule_file: "rules/empty.wfl".to_string(),
            shuffle: false,
            runs: None,
            summary: TestSummary {
                total: 0,
                passed: 0,
                failed: 0,
            },
            tests: vec![],
            status: "pass".to_string(),
            verdict: "PASS".to_string(),
        };

        let json = serde_json::to_string(&report).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["verdict"], "PASS");
        assert!(v["tests"].as_array().unwrap().is_empty());
    }
}
