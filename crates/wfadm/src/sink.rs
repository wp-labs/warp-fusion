// wfadm sink — validate sink configuration

use std::fs;
use std::path::Path;

pub fn run() -> Result<(), String> {
    let root = Path::new(".");
    println!(
        "wfadm sink: validating sink config at {}",
        root.canonicalize()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| root.display().to_string())
    );

    let mut issues = 0u32;
    let mut files_checked = 0u32;

    // topology/sinks/ directory
    let sinks_dir = root.join("topology").join("sinks");
    if sinks_dir.is_dir() {
        // defaults.toml
        let defaults = sinks_dir.join("defaults.toml");
        if defaults.exists() {
            files_checked += 1;
            if let Err(e) = validate_sink_file(&defaults) {
                issues += 1;
                eprintln!("  [ERR] topology/sinks/defaults.toml: {e}");
            } else {
                println!("  [OK] topology/sinks/defaults.toml");
            }
        }

        for sub in &["business.d", "infra.d"] {
            let sub_dir = sinks_dir.join(sub);
            if sub_dir.is_dir() {
                let (n, errs) = validate_sink_dir(&sub_dir);
                files_checked += n;
                issues += errs;
                if errs == 0 {
                    println!("  [OK] topology/sinks/{sub}/: {n} file(s) ok");
                }
            }
        }

        // topology/sinks/connectors/sink.d/
        let conn_dir = sinks_dir.join("connectors").join("sink.d");
        if conn_dir.is_dir() {
            let (n, errs) = validate_sink_dir(&conn_dir);
            files_checked += n;
            issues += errs;
            if errs == 0 {
                println!("  [OK] topology/sinks/connectors/sink.d/: {n} file(s) ok");
            }
        }
    }

    // connectors/ directory (flat layout)
    let conn_dir = root.join("connectors").join("sink.d");
    if conn_dir.is_dir() {
        let (n, errs) = validate_sink_dir(&conn_dir);
        files_checked += n;
        issues += errs;
        if errs == 0 {
            println!("  [OK] connectors/sink.d/: {n} file(s) ok");
        }
    }

    println!();
    if issues > 0 {
        eprintln!("Result: {issues} issue(s) in {files_checked} file(s)");
        Err(format!("sink validation failed with {issues} issue(s)"))
    } else if files_checked == 0 {
        eprintln!("Result: no sink config files found");
        Err("no sink config files found".to_string())
    } else {
        println!("Result: all {files_checked} sink file(s) ok");
        Ok(())
    }
}

fn validate_sink_dir(dir: &Path) -> (u32, u32) {
    let mut files = 0u32;
    let mut errors = 0u32;
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let (n, e) = validate_sink_dir(&path);
                files += n;
                errors += e;
            } else if path.extension().map(|e| e == "toml").unwrap_or(false) {
                files += 1;
                if let Err(e) = validate_sink_file(&path) {
                    errors += 1;
                    eprintln!("  [ERR] {}: {e}", path.display());
                } else {
                    println!("  [OK] {}", path.display());
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
