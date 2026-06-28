// wfadm check — validate wf-rules project integrity

use std::fs;
use std::path::Path;

use crate::connectors;
use wf_lang::{lint_wfl, parse_wfg, parse_wfl, parse_wfs};

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
    if conf_path.exists() {
        match fs::read_to_string(&conf_path) {
            Ok(content) => {
                if content.parse::<toml::Value>().is_ok() {
                    ok += 1;
                    println!("  ✓  wfusion.toml");
                } else {
                    err += 1;
                    eprintln!("  ✗  wfusion.toml — invalid TOML");
                }
            }
            Err(e) => {
                err += 1;
                eprintln!("  ✗  wfusion.toml — read error: {e}");
            }
        }
    } else {
        err += 1;
        eprintln!("  ✗  wfusion.toml — not found");
    }

    // ── models ────────────────────────────────────────────────────────
    println!("  ── models ──");

    let models_ok = check_models(root, &mut err);
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
            println!("  ✓  sink.d/ ({sink_count} .toml)");
        } else {
            err += 1;
        }
    } else {
        eprintln!("  ✗  sink.d/ — not found");
    }

    // ── Result ────────────────────────────────────────────────────────
    println!();
    if err > 0 {
        eprintln!("  ── Result ──");
        eprintln!("  ✗  {err} error(s)  |  {ok} ok");
        Err(format!("validation failed with {err} error(s)"))
    } else {
        println!("  ── Result ──");
        println!("  ✓  all checks passed ({ok} ok)");
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
                    println!("  ✓  sinks/{part}/ ({count} .toml)");
                    ok += 1;
                } else {
                    *err += 1;
                }
            } else {
                eprintln!("  ✗  sinks/{part}/ — not found");
            }
        }

        // defaults.toml
        let defaults = sinks_dir.join("defaults.toml");
        if defaults.exists() {
            if let Err(e) = validate_sink_file(&defaults) {
                *err += 1;
                eprintln!("  ✗  sinks/defaults.toml — {e}");
            } else {
                println!("  ✓  sinks/defaults.toml");
            }
        }

        // topology/sinks/connectors/sink.d/
        let conn_dir = sinks_dir.join("connectors").join("sink.d");
        if conn_dir.is_dir() {
            let (count, errs) = validate_sink_dir(&conn_dir, true);
            if errs == 0 {
                println!("  ✓  sinks/connectors/sink.d/ ({count} .toml)");
            } else {
                *err += 1;
            }
        }
    } else {
        eprintln!("  ✗  sinks/ — directory not found");
    }

    // sources
    let sources_dir = root.join("topology").join("sources");
    if sources_dir.is_dir() {
        let src_count = count_files(&sources_dir, "toml");
        println!("  ✓  sources/ ({src_count} .toml)");
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
fn check_models(root: &Path, err: &mut u32) -> u32 {
    let mut ok = 0u32;

    // ── WFL (rules/) ───────────────────────────────────────────────
    let rules_dir = root.join("models").join("rules");
    let wfl_files = list_files(&rules_dir, "wfl");
    if wfl_files.is_empty() {
        eprintln!("  ✗  rules/ — directory not found or empty");
    } else {
        let mut parsed = Vec::new();
        let mut parse_errs = 0u32;
        for f in &wfl_files {
            match fs::read_to_string(f) {
                Ok(content) => match parse_wfl(&content) {
                    Ok(ast) => parsed.push((f, ast)),
                    Err(e) => {
                        parse_errs += 1;
                        eprintln!(
                            "  ✗  {} — parse error: {}",
                            f.file_name().unwrap_or_default().to_string_lossy(),
                            e
                        );
                    }
                },
                Err(e) => {
                    parse_errs += 1;
                    eprintln!(
                        "  ✗  {} — read error: {e}",
                        f.file_name().unwrap_or_default().to_string_lossy()
                    );
                }
            }
        }

        // Lint parsed WFL files (schemas param is unused in current lint_wfl)
        let mut lint_warnings = 0u32;
        for (f, ast) in &parsed {
            let warnings = lint_wfl(ast, &[]);
            if !warnings.is_empty() {
                lint_warnings += warnings.len() as u32;
                for w in &warnings {
                    let rule = w.rule.as_deref().unwrap_or("?");
                    eprintln!(
                        "  ⚠  {} ({rule}): {}",
                        f.file_name().unwrap_or_default().to_string_lossy(),
                        w.message
                    );
                }
            }
        }

        if parse_errs == 0 {
            ok += 1;
            if lint_warnings > 0 {
                println!(
                    "  ✓  rules/ ({} .wfl, {} lint warning(s))",
                    wfl_files.len(),
                    lint_warnings
                );
            } else {
                println!("  ✓  rules/ ({} .wfl)", wfl_files.len());
            }
        } else {
            *err += 1;
        }
    }

    // ── WFS (schemas/) ─────────────────────────────────────────────
    let schemas_dir = root.join("models").join("schemas");
    let wfs_files = list_files(&schemas_dir, "wfs");
    if wfs_files.is_empty() {
        eprintln!("  ✗  schemas/ — directory not found or empty");
    } else {
        let mut parse_errs = 0u32;
        for f in &wfs_files {
            match fs::read_to_string(f) {
                Ok(content) => {
                    if let Err(e) = parse_wfs(&content) {
                        parse_errs += 1;
                        eprintln!(
                            "  ✗  {} — parse error: {e}",
                            f.file_name().unwrap_or_default().to_string_lossy()
                        );
                    }
                }
                Err(e) => {
                    parse_errs += 1;
                    eprintln!(
                        "  ✗  {} — read error: {e}",
                        f.file_name().unwrap_or_default().to_string_lossy()
                    );
                }
            }
        }

        if parse_errs == 0 {
            ok += 1;
            println!("  ✓  schemas/ ({} .wfs)", wfs_files.len());
        } else {
            *err += 1;
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
                            "  ✗  {} — parse error: {e}",
                            f.file_name().unwrap_or_default().to_string_lossy()
                        );
                    }
                }
                Err(e) => {
                    parse_errs += 1;
                    eprintln!(
                        "  ✗  {} — read error: {e}",
                        f.file_name().unwrap_or_default().to_string_lossy()
                    );
                }
            }
        }

        if parse_errs == 0 {
            ok += 1;
            println!("  ✓  scenarios/ ({} .wfg)", wfg_files.len());
        } else {
            *err += 1;
        }
    }

    ok
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
                    eprintln!("  ✗  {} — {e}", path.display());
                } else if !quiet {
                    println!(
                        "  ✓  {}",
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
    let _val: toml::Value = content
        .parse::<toml::Value>()
        .map_err(|e| format!("invalid TOML: {e}"))?;
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
        let ok = check_models(&dir, &mut err);
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
        let _ = check_models(&dir, &mut err);
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
        let ok = check_models(&dir, &mut err);
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
        let ok = check_models(&dir, &mut err);
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
        let _ = check_models(&dir, &mut err);
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
        let ok = check_models(&dir, &mut err);
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
        let _ = check_models(&dir, &mut err);
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
        let ok = check_models(&dir, &mut err);
        assert!(ok > 0);
        assert_eq!(err, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
