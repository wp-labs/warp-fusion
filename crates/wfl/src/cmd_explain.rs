use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::Result;

use wf_config::project::{load_schemas, load_wfl_with_context, parse_vars};
use wf_lang::explain::RuleExplanation;
use wf_vars::ConfigVarContext;

const BOLD: &str = "\x1b[1m";
const GREEN: &str = "\x1b[1;32m";
const CYAN: &str = "\x1b[36m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

pub fn run(file: PathBuf, schemas: Vec<String>, vars: Vec<String>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let mut var_map = parse_vars(&vars)?;
    var_map
        .entry("WORK_DIR".to_string())
        .or_insert_with(|| cwd.to_string_lossy().to_string());
    let ctx = ConfigVarContext::from_explicit_vars(var_map);
    let color = std::io::stdout().is_terminal();

    // Load schemas
    let all_schemas = load_schemas(&schemas, &cwd)?;

    // Load and preprocess the .wfl file
    let source = load_wfl_with_context(&file, &ctx, Some(&cwd))?;

    // Parse
    let wfl_file = wf_lang::parse_wfl(&source).map_err(|e| anyhow::anyhow!("parse error: {e}"))?;

    // Compile (runs check_wfl internally)
    let plans = wf_lang::compile_wfl(&wfl_file, &all_schemas)?;

    // Explain
    let explanations = wf_lang::explain::explain_rules(&plans, &all_schemas);

    if color {
        for expl in &explanations {
            print_colored(expl);
        }
    } else {
        for expl in &explanations {
            print!("{expl}");
        }
    }

    Ok(())
}

fn print_colored(e: &RuleExplanation) {
    println!("{BOLD}Rule: {GREEN}{}{RESET}", e.name);

    // Bindings
    println!("  {BOLD}Bindings:{RESET}");
    for b in &e.bindings {
        match &b.filter {
            Some(filter) => {
                println!(
                    "    {CYAN}{}{RESET} {DIM}->{RESET} {}  {DIM}[filter: {}]{RESET}",
                    b.alias, b.window, filter
                );
            }
            None => {
                println!("    {CYAN}{}{RESET} {DIM}->{RESET} {}", b.alias, b.window);
            }
        }
    }

    // Match
    println!(
        "  {BOLD}Match{RESET} {CYAN}<{}>{RESET} {}:",
        e.match_expl.keys, e.match_expl.window_spec
    );
    if !e.match_expl.event_steps.is_empty() {
        println!("    {BOLD}on event:{RESET}");
        for (i, step) in e.match_expl.event_steps.iter().enumerate() {
            println!("      step {}: {}", i + 1, step);
        }
    }
    if !e.match_expl.close_steps.is_empty() {
        let label = match e.match_expl.close_mode {
            Some(wf_lang::ast::CloseMode::And) => "and close:",
            _ => "on close:",
        };
        println!("    {BOLD}{label}{RESET}");
        for (i, step) in e.match_expl.close_steps.iter().enumerate() {
            println!("      step {}: {}", i + 1, step);
        }
    }

    // Score
    println!("  {BOLD}Score:{RESET} {GREEN}{}{RESET}", e.score);

    // Joins
    if !e.joins.is_empty() {
        println!("  {BOLD}Joins:{RESET}");
        for j in &e.joins {
            println!("    {}", j);
        }
    }

    // Entity
    println!(
        "  {BOLD}Entity:{RESET} {} {DIM}={RESET} {}",
        e.entity_type, e.entity_id
    );

    // Yield
    println!(
        "  {BOLD}Yield{RESET} {DIM}->{RESET} {CYAN}{}{RESET}:",
        e.yield_target
    );
    let yw = max_field_width(&e.yield_fields);
    for (name, value) in &e.yield_fields {
        println!(
            "    {CYAN}{:width$}{RESET} {DIM}={RESET} {}",
            name,
            value,
            width = yw
        );
    }

    // Lineage
    if !e.lineage.is_empty() {
        println!("  {BOLD}Field Lineage:{RESET}");
        let lw = max_field_width(&e.lineage);
        for (name, origin) in &e.lineage {
            println!(
                "    {CYAN}{:width$}{RESET} {DIM}<-{RESET} {}",
                name,
                origin,
                width = lw
            );
        }
    }

    // Limits
    if let Some(ref limits) = e.limits {
        println!("  {BOLD}Limits:{RESET} {}", limits);
    }
}

fn max_field_width(fields: &[(String, String)]) -> usize {
    fields.iter().map(|(n, _)| n.len()).max().unwrap_or(0)
}
