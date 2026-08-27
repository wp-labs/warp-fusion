use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process;

use orion_error::conversion::SourceErr;

use crate::error::{WflReason, WflResult, WflStructExt};
use wf_lang::{CheckError, Severity};

use wf_config::ConfigVarContext;
use wf_config::project::{load_schemas, parse_vars};

fn print_diag(diag: &CheckError, color: bool) {
    let (prefix, code) = match diag.severity {
        Severity::Error => ("error", "\x1b[1;31m"), // bold red
        Severity::Warning => ("warning", "\x1b[1;38;5;208m"), // bold orange
    };
    let reset = "\x1b[0m";

    let context = match (&diag.rule, &diag.test) {
        (Some(r), _) => format!(": rule `{r}`"),
        (_, Some(t)) => format!(": test `{t}`"),
        _ => String::new(),
    };

    if color {
        eprintln!("{code}{prefix}{reset}{context}: {}", diag.message);
    } else {
        eprintln!("{prefix}{context}: {}", diag.message);
    }
}

pub fn run(file: PathBuf, schemas: Vec<String>, vars: Vec<String>) -> WflResult<()> {
    let cwd = std::env::current_dir().source_err(WflReason::Io, "reading cwd")?;
    let mut var_map = parse_vars(&vars).wfl()?;
    var_map
        .entry("WORK_DIR".to_string())
        .or_insert_with(|| cwd.to_string_lossy().to_string());
    let ctx = ConfigVarContext::from_explicit_vars(var_map);
    let color = std::io::stderr().is_terminal();

    // Load schemas
    let all_schemas = load_schemas(&schemas, &cwd).wfl()?;

    // Load and preprocess the .wfl file + parse `use` imports (issue #73)
    let wfl_file = crate::load_wfl_with_imports(&file, &ctx, &cwd)?;
    // 列表引用展开——lint 只跑 check_wfl（不经 compile_wfl 的展开）, 必须显式
    // 展开, 否则 checker 只见 ListRef（infer None）跳过类型/未知名检查。
    let wfl_file = wf_lang::compiler::lists::resolve_list_refs(&wfl_file).map_err(|e| {
        crate::error::error(
            WflReason::Validation,
            e.detail().clone().unwrap_or_else(|| e.to_string()),
        )
    })?;

    // Run error-level checks
    let errors = wf_lang::check_wfl(&wfl_file, &all_schemas);

    // Run lint-level checks
    let warnings = wf_lang::lint_wfl(&wfl_file, &all_schemas);

    let total = errors.len() + warnings.len();
    let mut has_errors = false;

    // Print all diagnostics
    for diag in errors.iter().chain(warnings.iter()) {
        if diag.severity == Severity::Error {
            has_errors = true;
        }
        print_diag(diag, color);
    }

    if total == 0 {
        if color {
            eprintln!("\x1b[1;32mNo issues found.\x1b[0m");
        } else {
            eprintln!("No issues found.");
        }
    } else {
        let ec = errors.len();
        let wc = warnings.len();
        if color {
            let mut buf = String::new();
            buf.push_str("\n\x1b[1m");
            if ec > 0 {
                buf.push_str(&format!("\x1b[31m{ec} error(s)\x1b[0m\x1b[1m"));
            }
            if ec > 0 && wc > 0 {
                buf.push_str(", ");
            }
            if wc > 0 {
                buf.push_str(&format!("\x1b[38;5;208m{wc} warning(s)\x1b[0m"));
            }
            eprint!("{buf}");
            let _ = std::io::stderr().flush();
            eprintln!();
        } else {
            eprintln!("\n{ec} error(s), {wc} warning(s)");
        }
    }

    if has_errors {
        process::exit(1);
    }

    Ok(())
}
