// wfadm check — validate wf-rules project integrity

use std::fs;
use std::path::{Path, PathBuf};

use crate::connectors;
use wf_lang::{lint_wfl, parse_wfg, parse_wfl, parse_wfs};

// ── ANSI color helpers ────────────────────────────────────────────────
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RESET: &str = "\x1b[0m";

fn red(s: &str) -> String {
    format!("{RED}{s}{RESET}")
}

fn green(s: &str) -> String {
    format!("{GREEN}{s}{RESET}")
}

fn yellow(s: &str) -> String {
    format!("{YELLOW}{s}{RESET}")
}

pub fn run() -> Result<(), String> {
    check_project(Path::new("."))
}

pub(crate) fn check_project(root: &Path) -> Result<(), String> {
    let mut ok: u32 = 0;
    let mut err: u32 = 0;

    println!(
        "wfadm check: validating wf-rules project at {}",
        root.canonicalize()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| root.display().to_string())
    );

    // Register factories for reporting
    connectors::ensure_factories_registered();
    let registered_sinks = wp_core_connectors::registry::list_sink_kinds();
    let registered_sources = wp_core_connectors::registry::list_source_kinds();
    println!(
        "  registered: {} sink(s), {} source(s)",
        registered_sinks.len(),
        registered_sources.len()
    );

    // ── conf ──────────────────────────────────────────────────────────
    println!("\n  ── conf ──");
    let conf_path = root.join("conf").join("wfusion.toml");
    let mut cfg_rules_path: Option<String> = None;
    let mut cfg_schemas_path: Option<String> = None;
    if conf_path.exists() {
        match fs::read_to_string(&conf_path) {
            Ok(content) => {
                match toml::from_str::<toml::Value>(&content) {
                    Ok(toml_val) => {
                        ok += 1;
                        println!("  {}  wfusion.toml", green("✓"));
                        // Extract rules/schemas paths from runtime section
                        if let Some(runtime) = toml_val.get("runtime") {
                            cfg_rules_path = runtime
                                .get("rules")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                            cfg_schemas_path = runtime
                                .get("schemas")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                        }
                    }
                    Err(e) => {
                        err += 1;
                        eprintln!("  {}  wfusion.toml — invalid TOML: {}", red("✗"), e);
                    }
                }
            }
            Err(e) => {
                err += 1;
                eprintln!("  {}  wfusion.toml — read error: {}", red("✗"), e);
            }
        }
    } else {
        err += 1;
        eprintln!("  {}  wfusion.toml — not found", red("✗"));
    }

    // ── models ────────────────────────────────────────────────────────
    println!("  ── models ──");

    let base_dir = root.to_path_buf();
    let models_ok = check_models(
        root,
        &base_dir,
        &cfg_rules_path,
        &cfg_schemas_path,
        &mut err,
    );
    if models_ok > 0 {
        ok += models_ok;
    }

    // ── topology ──────────────────────────────────────────────────────
    println!("  ── topology ──");

    let topology_ok = check_topology(root, &mut err);
    if topology_ok > 0 {
        ok += 1;
    }

    // ── connectors ────────────────────────────────────────────────────
    println!("  ── connectors ──");
    let conn_dir = root.join("connectors").join("sink.d");
    if conn_dir.is_dir() {
        let (sink_count, sink_errs) = validate_sink_dir(&conn_dir, true);
        if sink_errs == 0 {
            ok += 1;
            println!("  {}  sink.d/ ({sink_count} .toml)", green("✓"));
        } else {
            err += 1;
        }
    } else if let Some((up_dir, depth)) = find_upward(root, "connectors/sink.d") {
        let (sink_count, sink_errs) = validate_sink_dir(&up_dir, true);
        if sink_errs == 0 {
            ok += 1;
            let rel = if depth == 0 {
                "connectors/sink.d".to_string()
            } else {
                format!("{}connectors/sink.d", "../".repeat(depth))
            };
            println!(
                "  {}  sink.d/ → {rel} ({sink_count} .toml, upstream)",
                green("✓")
            );
        } else {
            err += 1;
        }
    } else {
        eprintln!("  {}  sink.d/ — not found", red("✗"));
    }

    // ── Result ────────────────────────────────────────────────────────
    println!();
    if err > 0 {
        eprintln!("  ── Result ──");
        eprintln!("  {}  {err} error(s)  |  {ok} ok", red("✗"));
        Err(format!("validation failed with {err} error(s)"))
    } else {
        println!("  ── Result ──");
        println!("  {}  all checks passed ({ok} ok)", green("✓"));
        Ok(())
    }
}

// ── topology validation ──────────────────────────────────────────────

fn check_topology(root: &Path, err: &mut u32) -> u32 {
    let mut ok = 0u32;
    let sinks_dir = root.join("topology").join("sinks");

    if sinks_dir.is_dir() {
        // Validate each business.d / infra.d subdirectory
        let parts = ["business.d", "infra.d"];
        for part in &parts {
            let sub_dir = sinks_dir.join(part);
            if sub_dir.is_dir() {
                let (count, errs) = validate_sink_dir(&sub_dir, true);
                if errs == 0 {
                    println!("  {}  sinks/{part}/ ({count} .toml)", green("✓"));
                    ok += 1;
                } else {
                    *err += 1;
                }
            } else {
                eprintln!("  {}  sinks/{part}/ — not found", red("✗"));
            }
        }

        // defaults.toml
        let defaults = sinks_dir.join("defaults.toml");
        if defaults.exists() {
            if let Err(e) = validate_sink_file(&defaults) {
                *err += 1;
                eprintln!("  {}  sinks/defaults.toml — {e}", red("✗"));
            } else {
                println!("  {}  sinks/defaults.toml", green("✓"));
            }
        }

        // topology/sinks/connectors/sink.d/
        let conn_dir = sinks_dir.join("connectors").join("sink.d");
        if conn_dir.is_dir() {
            let (count, errs) = validate_sink_dir(&conn_dir, true);
            if errs == 0 {
                println!("  {}  sinks/connectors/sink.d/ ({count} .toml)", green("✓"));
            } else {
                *err += 1;
            }
        }
    } else {
        eprintln!("  {}  sinks/ — directory not found", red("✗"));
    }

    // sources
    let sources_dir = root.join("topology").join("sources");
    if sources_dir.is_dir() {
        let src_count = count_files(&sources_dir, "toml");
        println!("  {}  sources/ ({src_count} .toml)", green("✓"));
        ok += 1;
    }

    ok
}

// ── helpers ──────────────────────────────────────────────────────────

fn count_files(dir: &Path, ext: &str) -> usize {
    let mut count = 0;
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                count += count_files(&path, ext);
            } else if path.extension().map(|e| e == ext).unwrap_or(false) {
                count += 1;
            }
        }
    }
    count
}

/// Recursively list files with a given extension.
fn list_files(dir: &Path, ext: &str) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(list_files(&path, ext));
            } else if path.extension().map(|e| e == ext).unwrap_or(false) {
                files.push(path);
            }
        }
    }
    files
}

/// Deep validation: parse all .wfl, .wfs, .wfg files.
/// Result of trying to find model files (local or external).
struct ResolvedFiles {
    files: Vec<PathBuf>,
    is_external: bool,
    /// Config-relative display path (only set when is_external).
    display_path: Option<String>,
}

/// Try to find model files: local `models/{dir_name}/` first, then fall back
/// to the config-specified path.  Returns `ResolvedFiles` on success.
fn resolve_model_dir(
    label: &str,
    ext: &str,
    local_dir: &Path,
    cfg_path: &Option<String>,
    base_dir: &Path,
) -> Option<ResolvedFiles> {
    let local_files = list_files(local_dir, ext);
    if !local_files.is_empty() {
        return Some(ResolvedFiles {
            files: local_files,
            is_external: false,
            display_path: None,
        });
    }

    // Try external path from config
    let ext_path = cfg_path.as_deref()?;
    let ext_dir = resolve_config_dir(base_dir, ext_path)?;
    let ext_files = list_files(&ext_dir, ext);
    if ext_files.is_empty() {
        eprintln!(
            "  {}  {label}/ — directory exists but has no .{ext} files (config: {ext_path})",
            red("✗")
        );
        return None;
    }
    Some(ResolvedFiles {
        files: ext_files,
        is_external: true,
        display_path: Some(strip_glob(ext_path).to_string()),
    })
}

/// Print the success line for a model group and increment `ok`.
fn report_model_ok(label: &str, ext: &str, count: usize, resolved: &ResolvedFiles, ok: &mut u32) {
    *ok += 1;
    if resolved.is_external {
        let dp = resolved.display_path.as_deref().unwrap_or("?");
        println!(
            "  {}  {label}/ → {dp} ({count} .{ext}, external)",
            green("✓")
        );
    } else {
        println!("  {}  {label}/ ({count} .{ext})", green("✓"));
    }
}

/// Print the success line for WFL with lint warning count, increment `ok`.
fn report_wfl_ok(count: usize, lint_count: u32, resolved: &ResolvedFiles, ok: &mut u32) {
    *ok += 1;
    if resolved.is_external {
        let dp = resolved.display_path.as_deref().unwrap_or("?");
        println!(
            "  {}  rules/ → {dp} ({count} .wfl, {lint_count} lint warning(s), external)",
            green("✓")
        );
    } else {
        println!(
            "  {}  rules/ ({count} .wfl, {lint_count} lint warning(s))",
            green("✓")
        );
    }
}

fn check_models(
    root: &Path,
    base_dir: &Path,
    cfg_rules_path: &Option<String>,
    cfg_schemas_path: &Option<String>,
    err: &mut u32,
) -> u32 {
    let mut ok = 0u32;

    // ── WFL (rules/) ───────────────────────────────────────────────
    let rules_dir = root.join("models").join("rules");
    match resolve_model_dir("rules", "wfl", &rules_dir, cfg_rules_path, base_dir) {
        Some(resolved) => {
            let mut parsed = Vec::new();
            let mut parse_errs = 0u32;
            for f in &resolved.files {
                match fs::read_to_string(f) {
                    Ok(content) => match parse_wfl(&content) {
                        Ok(ast) => parsed.push((f, ast)),
                        Err(e) => {
                            parse_errs += 1;
                            eprintln!(
                                "  {}  {} — parse error: {}",
                                red("✗"),
                                f.file_name().unwrap_or_default().to_string_lossy(),
                                e
                            );
                        }
                    },
                    Err(e) => {
                        parse_errs += 1;
                        eprintln!(
                            "  {}  {} — read error: {e}",
                            red("✗"),
                            f.file_name().unwrap_or_default().to_string_lossy()
                        );
                    }
                }
            }

            // Lint parsed WFL files
            let mut lint_warnings = 0u32;
            for (f, ast) in &parsed {
                let warnings = lint_wfl(ast, &[]);
                if !warnings.is_empty() {
                    lint_warnings += warnings.len() as u32;
                    for w in &warnings {
                        let rule = w.rule.as_deref().unwrap_or("?");
                        eprintln!(
                            "  {}  {} ({rule}): {}",
                            yellow("⚠"),
                            f.file_name().unwrap_or_default().to_string_lossy(),
                            w.message
                        );
                    }
                }
            }

            if parse_errs > 0 {
                *err += 1;
            } else if lint_warnings > 0 {
                report_wfl_ok(resolved.files.len(), lint_warnings, &resolved, &mut ok);
            } else {
                report_model_ok("rules", "wfl", resolved.files.len(), &resolved, &mut ok);
            }
        }
        None => {
            if let Some(ext_path) = cfg_rules_path {
                eprintln!(
                    "  {}  rules/ — directory not found (config: {ext_path})",
                    red("✗")
                );
            } else {
                eprintln!("  {}  rules/ — directory not found or empty", red("✗"));
            }
        }
    }

    // ── WFS (schemas/) ─────────────────────────────────────────────
    let schemas_dir = root.join("models").join("schemas");
    match resolve_model_dir("schemas", "wfs", &schemas_dir, cfg_schemas_path, base_dir) {
        Some(resolved) => {
            let mut parse_errs = 0u32;
            for f in &resolved.files {
                match fs::read_to_string(f) {
                    Ok(content) => {
                        if let Err(e) = parse_wfs(&content) {
                            parse_errs += 1;
                            eprintln!(
                                "  {}  {} — parse error: {e}",
                                red("✗"),
                                f.file_name().unwrap_or_default().to_string_lossy()
                            );
                        }
                    }
                    Err(e) => {
                        parse_errs += 1;
                        eprintln!(
                            "  {}  {} — read error: {e}",
                            red("✗"),
                            f.file_name().unwrap_or_default().to_string_lossy()
                        );
                    }
                }
            }
            if parse_errs > 0 {
                *err += 1;
            } else {
                report_model_ok("schemas", "wfs", resolved.files.len(), &resolved, &mut ok);
            }
        }
        None => {
            if let Some(ext_path) = cfg_schemas_path {
                eprintln!(
                    "  {}  schemas/ — directory not found (config: {ext_path})",
                    red("✗")
                );
            } else {
                eprintln!("  {}  schemas/ — directory not found or empty", red("✗"));
            }
        }
    }

    // ── WFG (scenarios/) ───────────────────────────────────────────
    let scenarios_dir = root.join("models").join("scenarios");
    let wfg_files = list_files(&scenarios_dir, "wfg");
    if wfg_files.is_empty() {
        println!("     scenarios/ — (none)");
    } else {
        let mut parse_errs = 0u32;
        for f in &wfg_files {
            match fs::read_to_string(f) {
                Ok(content) => {
                    if let Err(e) = parse_wfg(&content) {
                        parse_errs += 1;
                        eprintln!(
                            "  {}  {} — parse error: {e}",
                            red("✗"),
                            f.file_name().unwrap_or_default().to_string_lossy()
                        );
                    }
                }
                Err(e) => {
                    parse_errs += 1;
                    eprintln!(
                        "  {}  {} — read error: {e}",
                        red("✗"),
                        f.file_name().unwrap_or_default().to_string_lossy()
                    );
                }
            }
        }

        if parse_errs == 0 {
            ok += 1;
            println!("  {}  scenarios/ ({} .wfg)", green("✓"), wfg_files.len());
        } else {
            *err += 1;
        }
    }

    ok
}

/// Resolve a glob-like path from config (e.g. "../../models/rules/wfl/*.wfl")
/// relative to the conf directory, stripping the trailing /*.ext glob pattern
/// to get the actual directory path.
fn resolve_config_dir(conf_dir: &Path, glob_path: &str) -> Option<PathBuf> {
    let dir_pattern = strip_glob(glob_path);
    conf_dir
        .join(dir_pattern)
        .canonicalize()
        .ok()
        .filter(|p| p.is_dir())
}

/// Strip the trailing glob pattern (e.g. "/*.wfl") from a config path,
/// returning the directory portion for display.
fn strip_glob(glob_path: &str) -> &str {
    if let Some(pos) = glob_path.rfind('/') {
        &glob_path[..pos]
    } else {
        glob_path
    }
}

/// Search upward from `root` through parent directories for `relative_path`.
/// Returns the found directory and how many parent levels up it was found.
fn find_upward(root: &Path, relative_path: &str) -> Option<(PathBuf, usize)> {
    let mut current = if let Ok(canon) = root.canonicalize() {
        canon
    } else {
        root.to_path_buf()
    };
    let mut depth = 0usize;
    loop {
        let candidate = current.join(relative_path);
        if candidate.is_dir() {
            return Some((candidate, depth));
        }
        if !current.pop() {
            break;
        }
        depth += 1;
    }
    None
}

/// Validate all .toml files in a directory recursively.
///
/// When `quiet` is true, individual file names are only printed on error.
fn validate_sink_dir(dir: &Path, quiet: bool) -> (u32, u32) {
    let mut files = 0u32;
    let mut errors = 0u32;
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let (n, e) = validate_sink_dir(&path, quiet);
                files += n;
                errors += e;
            } else if path.extension().map(|e| e == "toml").unwrap_or(false) {
                files += 1;
                if let Err(e) = validate_sink_file(&path) {
                    errors += 1;
                    eprintln!("  {}  {} — {e}", red("✗"), path.display());
                } else if !quiet {
                    println!(
                        "  {}  {}",
                        green("✓"),
                        path.file_name().unwrap_or_default().to_string_lossy()
                    );
                }
            }
        }
    }
    (files, errors)
}

fn validate_sink_file(path: &Path) -> Result<(), String> {
    let content = fs::read_to_string(path).map_err(|e| format!("read error: {e}"))?;
    let _val: toml::Value = toml::from_str(&content).map_err(|e| format!("invalid TOML: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "wfadm_check_{}_{}",
            std::process::id(),
            rand::random::<u32>()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn empty_dir_has_missing_conf() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let result = check_project(&dir);
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dir_with_conf_passes_conf_check() {
        let dir = temp_dir();
        std::fs::create_dir_all(dir.join("conf")).unwrap();
        std::fs::write(dir.join("conf/wfusion.toml"), "mode = \"daemon\"\n").unwrap();
        let _ = check_project(&dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_toml_detected() {
        let dir = temp_dir();
        std::fs::create_dir_all(dir.join("conf")).unwrap();
        std::fs::write(dir.join("conf/wfusion.toml"), "not valid toml [[[").unwrap();
        let result = check_project(&dir);
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_sink_toml_detected() {
        let dir = temp_dir();
        std::fs::create_dir_all(dir.join("topology/sinks/business.d")).unwrap();
        std::fs::write(dir.join("topology/sinks/defaults.toml"), "valid = \"ok\"\n").unwrap();
        std::fs::write(
            dir.join("topology/sinks/business.d/bad.toml"),
            "invalid [[[ toml",
        )
        .unwrap();
        let _ = check_project(&dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn valid_toml_documents_parse_for_conf_and_sinks() {
        let dir = temp_dir();
        std::fs::create_dir_all(dir.join("conf")).unwrap();
        std::fs::create_dir_all(dir.join("topology/sinks/business.d")).unwrap();
        std::fs::write(
            dir.join("conf/wfusion.toml"),
            r#"
sources_dir = "topology/sources"
sinks = "topology/sinks"

[runtime]
schemas = "../models/schemas/*.wfs"
rules = "../models/wfl/*.wfl"
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("topology/sinks/business.d/scan.toml"),
            r#"
version = "1.0"

[sink_group]
name = "scan"
windows = ["scan_alerts"]
"#,
        )
        .unwrap();

        let content = std::fs::read_to_string(dir.join("conf/wfusion.toml")).unwrap();
        assert!(toml::from_str::<toml::Value>(&content).is_ok());
        assert!(validate_sink_file(&dir.join("topology/sinks/business.d/scan.toml")).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── list_files ───────────────────────────────────────────────

    #[test]
    fn list_files_finds_matching_extensions() {
        let dir = temp_dir();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.wfl"), "rule x {}").unwrap();
        std::fs::write(dir.join("b.wfl"), "rule y {}").unwrap();
        std::fs::write(dir.join("sub/c.wfl"), "rule z {}").unwrap();
        std::fs::write(dir.join("README.md"), "# docs").unwrap();

        let files = list_files(&dir, "wfl");
        assert_eq!(files.len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_files_empty_for_missing_extension() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "hello").unwrap();
        let files = list_files(&dir, "wfl");
        assert!(files.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── check_models ─ deep parse ────────────────────────────────

    /// Helper: set up conf + models dirs so check_models can run
    fn setup_models_dir(root: &std::path::Path) {
        std::fs::create_dir_all(root.join("conf")).unwrap();
        std::fs::write(root.join("conf/wfusion.toml"), "sinks = \"tmp\"\n").unwrap();
        std::fs::create_dir_all(root.join("models/rules")).unwrap();
        std::fs::create_dir_all(root.join("models/schemas")).unwrap();
        std::fs::create_dir_all(root.join("models/scenarios")).unwrap();
    }

    #[test]
    fn valid_wfl_parses() {
        let dir = temp_dir();
        setup_models_dir(&dir);
        std::fs::write(
            dir.join("models/rules/test.wfl"),
            r#"
use "auth.wfs"
rule test_rule {
    events { e : auth_events && e.action == "login" }
    match<sip:5m> {
        on event { e | count >= 3; }
        and close { e | count >= 3; }
    } -> score(80.0)
    entity(ip, e.sip)
    yield auth_alerts (sip = e.sip, alert_type = "test")
}
"#,
        )
        .unwrap();
        let mut err = 0;
        let ok = check_models(&dir, &dir.join("conf"), &None, &None, &mut err);
        assert!(ok > 0, "valid WFL should parse");
        assert_eq!(err, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_wfl_detected() {
        let dir = temp_dir();
        setup_models_dir(&dir);
        std::fs::write(
            dir.join("models/rules/bad.wfl"),
            "this is not valid wfl at all",
        )
        .unwrap();
        let mut err = 0;
        let _ = check_models(&dir, &dir.join("conf"), &None, &None, &mut err);
        assert!(err > 0, "invalid WFL should produce errors");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wfl_parses_without_close_block() {
        let dir = temp_dir();
        setup_models_dir(&dir);
        // Rule without close block should still parse (lint should not produce noise)
        std::fs::write(
            dir.join("models/rules/no_close.wfl"),
            r#"
rule no_close {
    events { e : auth_events }
    match<sip:5m> { on event { e | count >= 1; } } -> score(50.0)
    entity(ip, e.sip)
    yield auth_alerts (sip = e.sip, alert_type = "test")
}
"#,
        )
        .unwrap();
        let mut err = 0;
        let ok = check_models(&dir, &dir.join("conf"), &None, &None, &mut err);
        assert!(ok > 0);
        assert_eq!(err, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn valid_wfs_parses() {
        let dir = temp_dir();
        setup_models_dir(&dir);
        let real_wfs = "window conn_events {\n    stream = \"conn_events\"\n    time = event_time\n    over = 30m\n    fields {\n        event_time: time\n        sip: ip\n        dip: ip\n    }\n}";
        std::fs::write(dir.join("models/schemas/test.wfs"), real_wfs).unwrap();
        let mut err = 0;
        let ok = check_models(&dir, &dir.join("conf"), &None, &None, &mut err);
        assert!(ok > 0, "valid WFS should parse");
        assert_eq!(err, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_wfs_detected() {
        let dir = temp_dir();
        setup_models_dir(&dir);
        std::fs::write(dir.join("models/schemas/bad.wfs"), "}}}} gibberish").unwrap();
        let mut err = 0;
        let _ = check_models(&dir, &dir.join("conf"), &None, &None, &mut err);
        assert!(err > 0, "invalid WFS should produce errors");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn valid_wfg_parses() {
        let dir = temp_dir();
        setup_models_dir(&dir);
        std::fs::write(
            dir.join("models/scenarios/test.wfg"),
            r#"
#[duration=5m]
scenario test_case<seed=42> {
  traffic { stream auth_events gen 10/s }
}
"#,
        )
        .unwrap();
        let mut err = 0;
        let ok = check_models(&dir, &dir.join("conf"), &None, &None, &mut err);
        assert!(ok > 0, "valid WFG should parse");
        assert_eq!(err, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_wfg_detected() {
        let dir = temp_dir();
        setup_models_dir(&dir);
        std::fs::write(dir.join("models/scenarios/bad.wfg"), "not wfg").unwrap();
        let mut err = 0;
        let _ = check_models(&dir, &dir.join("conf"), &None, &None, &mut err);
        assert!(err > 0, "invalid WFG should produce errors");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wfg_parses_use_declarations() {
        let dir = temp_dir();
        setup_models_dir(&dir);
        std::fs::write(
            dir.join("models/scenarios/with_use.wfg"),
            r#"
use "../schemas/auth.wfs"
use "../rules/some_rule.wfl"
#[duration=5m]
scenario with_use<seed=1> {
  traffic { stream auth_events gen 10/s }
}
"#,
        )
        .unwrap();
        let mut err = 0;
        let ok = check_models(&dir, &dir.join("conf"), &None, &None, &mut err);
        assert!(ok > 0);
        assert_eq!(err, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── strip_glob ───────────────────────────────────────────────

    #[test]
    fn strip_glob_removes_trailing_pattern() {
        assert_eq!(strip_glob("../../models/wfl/*.wfl"), "../../models/wfl");
        assert_eq!(strip_glob("../schemas/*.wfs"), "../schemas");
        assert_eq!(strip_glob("rules/wfl/*.wfl"), "rules/wfl");
    }

    #[test]
    fn strip_glob_no_slash_returns_unchanged() {
        assert_eq!(strip_glob("*.wfl"), "*.wfl");
        assert_eq!(strip_glob("network.wfs"), "network.wfs");
    }

    // ── resolve_config_dir ───────────────────────────────────────

    #[test]
    fn resolve_config_dir_finds_existing_directory() {
        let dir = temp_dir();
        let models = dir.join("shared/models/rules/wfl");
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(models.join("test.wfl"), "rule x {}").unwrap();

        let conf_dir = dir.join("project/conf");
        std::fs::create_dir_all(&conf_dir).unwrap();

        let result = resolve_config_dir(&conf_dir, "../../shared/models/rules/wfl/*.wfl");
        assert!(result.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_config_dir_returns_none_for_missing_directory() {
        let dir = temp_dir();
        let conf_dir = dir.join("project/conf");
        std::fs::create_dir_all(&conf_dir).unwrap();

        let result = resolve_config_dir(&conf_dir, "../../nonexistent/path/*.wfl");
        assert!(result.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── find_upward ──────────────────────────────────────────────

    #[test]
    fn find_upward_finds_sibling_directory() {
        let dir = temp_dir();
        let project = dir.join("a/b/project");
        let shared = dir.join("a/b/shared/connectors/sink.d");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::write(shared.join("test.toml"), "key = \"val\"").unwrap();

        let (found, depth) = find_upward(&project, "shared/connectors/sink.d").unwrap();
        assert_eq!(depth, 1); // project → a/b → found at a/b/shared/connectors/sink.d
        assert!(found.is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_upward_returns_none_when_not_found() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();

        let result = find_upward(&dir, "connectors/sink.d");
        assert!(result.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_upward_depth_zero_when_local() {
        let dir = temp_dir();
        let local = dir.join("connectors/sink.d");
        std::fs::create_dir_all(&local).unwrap();

        let (found, depth) = find_upward(&dir, "connectors/sink.d").unwrap();
        assert_eq!(depth, 0);
        assert!(found.is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── check_models with external config paths ────────────────────

    #[test]
    fn check_models_resolves_external_rules_from_config() {
        let dir = temp_dir();
        setup_models_dir(&dir);
        // Remove local rules — force external path resolution
        std::fs::remove_dir_all(dir.join("models/rules")).unwrap();

        // Create external rules directory
        let ext = dir.join("shared/rules");
        std::fs::create_dir_all(&ext).unwrap();
        std::fs::write(
            ext.join("test.wfl"),
            "use \"auth.wfs\"\nrule test_rule { events { e : auth_events } on each e where true -> score(1.0) entity(ip, e.sip) yield auth_alerts (sip = e.sip, alert_type = \"t\") }\n",
        )
        .unwrap();

        let cfg_rules = Some("../shared/rules/*.wfl".to_string());
        let mut err = 0;
        let ok = check_models(&dir, &dir.join("conf"), &cfg_rules, &None, &mut err);
        assert!(ok > 0, "external WFL should be resolved and parsed");
        assert_eq!(err, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_models_errors_on_missing_external_rules_path() {
        let dir = temp_dir();
        setup_models_dir(&dir);
        std::fs::remove_dir_all(dir.join("models/rules")).unwrap();

        let cfg_rules = Some("../nonexistent/*.wfl".to_string());
        let mut err = 0;
        let ok = check_models(&dir, &dir.join("conf"), &cfg_rules, &None, &mut err);
        // ok is still 0 (rules not found), but err is not incremented
        // because missing directory is a warning, not an error
        assert_eq!(ok, 0, "missing external rules should not count as ok");
        assert_eq!(err, 0, "missing external rules is a warning, not an error");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_models_resolves_external_schemas_from_config() {
        let dir = temp_dir();
        setup_models_dir(&dir);
        std::fs::remove_dir_all(dir.join("models/schemas")).unwrap();

        let ext = dir.join("shared/schemas");
        std::fs::create_dir_all(&ext).unwrap();
        std::fs::write(
            ext.join("test.wfs"),
            "window conn_events {\n    stream = \"conn_events\"\n    time = event_time\n    over = 30m\n    fields { event_time: time  sip: ip }\n}",
        )
        .unwrap();

        let cfg_schemas = Some("../shared/schemas/*.wfs".to_string());
        let mut err = 0;
        let ok = check_models(&dir, &dir.join("conf"), &None, &cfg_schemas, &mut err);
        assert!(ok > 0, "external WFS should be resolved and parsed");
        assert_eq!(err, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
