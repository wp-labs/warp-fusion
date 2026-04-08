use std::io::IsTerminal;
use std::path::PathBuf;
use std::process;

use anyhow::Result;

use wf_config::project::{load_schemas, load_wfl_with_context, parse_vars};
use wf_core::rule::contract::run_test;
use wf_lang::ast::PermutationMode;
use wf_vars::ConfigVarContext;

const GREEN: &str = "\x1b[1;32m";
const RED: &str = "\x1b[1;31m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

pub fn run(
    file: PathBuf,
    schemas: Vec<String>,
    vars: Vec<String>,
    shuffle: bool,
    runs: Option<usize>,
) -> Result<()> {
    if let Some(0) = runs {
        anyhow::bail!("--runs must be greater than 0");
    }

    let cwd = std::env::current_dir()?;
    let mut var_map = parse_vars(&vars)?;
    var_map
        .entry("WORK_DIR".to_string())
        .or_insert_with(|| cwd.to_string_lossy().to_string());
    let ctx = ConfigVarContext::from_explicit_vars(var_map);
    let color = std::io::stderr().is_terminal();

    // Load schemas
    let all_schemas = load_schemas(&schemas, &cwd)?;

    // Load and preprocess the .wfl file
    let source = load_wfl_with_context(&file, &ctx, Some(&cwd))?;

    // Parse
    let wfl_file = wf_lang::parse_wfl(&source).map_err(|e| anyhow::anyhow!("parse error: {e}"))?;

    // Compile rules into plans
    let plans = wf_lang::compile_wfl(&wfl_file, &all_schemas)?;

    if wfl_file.tests.is_empty() {
        eprintln!("No tests found.");
        return Ok(());
    }

    let mut total = 0;
    let mut passed = 0;
    let mut failed = 0;

    for test in &wfl_file.tests {
        total += 1;

        let plan = match plans.iter().find(|p| p.name == test.rule_name) {
            Some(p) => p,
            None => {
                if color {
                    eprintln!(
                        "{RED}FAIL{RESET}  {} — target rule `{}` not found",
                        test.name, test.rule_name
                    );
                } else {
                    eprintln!(
                        "FAIL  {} — target rule `{}` not found",
                        test.name, test.rule_name
                    );
                }
                failed += 1;
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

        match run_test(&effective_test, plan, time_field) {
            Ok(result) => {
                if result.passed {
                    if color {
                        eprintln!(
                            "{GREEN}PASS{RESET}  {} {DIM}({}){RESET}",
                            test.name, test.rule_name
                        );
                    } else {
                        eprintln!("PASS  {} ({})", test.name, test.rule_name);
                    }
                    passed += 1;
                } else {
                    if color {
                        eprintln!(
                            "{RED}FAIL{RESET}  {} {DIM}({}){RESET}",
                            test.name, test.rule_name
                        );
                        for f in &result.failures {
                            eprintln!("      {RED}{f}{RESET}");
                        }
                    } else {
                        eprintln!("FAIL  {} ({})", test.name, test.rule_name);
                        for f in &result.failures {
                            eprintln!("      {}", f);
                        }
                    }
                    failed += 1;
                }
            }
            Err(e) => {
                if color {
                    eprintln!(
                        "{RED}FAIL{RESET}  {} {DIM}({}){RESET} — error: {}",
                        test.name, test.rule_name, e
                    );
                } else {
                    eprintln!("FAIL  {} ({}) — error: {}", test.name, test.rule_name, e);
                }
                failed += 1;
            }
        }
    }

    if color {
        eprintln!(
            "\n{BOLD}{total} tests: {GREEN}{passed} passed{RESET}{BOLD}, {}{failed} failed{RESET}",
            if failed > 0 { RED } else { GREEN },
        );
    } else {
        eprintln!("\n{} tests: {} passed, {} failed", total, passed, failed);
    }

    if failed > 0 {
        process::exit(1);
    }

    Ok(())
}
