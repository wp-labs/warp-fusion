// wfadm check — validate wf-rules project integrity

use std::fs;
use std::path::Path;

pub fn run() -> Result<(), String> {
    let root = Path::new(".");
    let mut ok: u32 = 0;
    let mut err: u32 = 0;
    let mut warn: u32 = 0;

    println!(
        "wfadm check: validating wf-rules project at {}",
        root.canonicalize()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| root.display().to_string())
    );

    // 1. conf/wfusion.toml
    let conf_path = root.join("conf").join("wfusion.toml");
    if conf_path.exists() {
        match fs::read_to_string(&conf_path) {
            Ok(content) => {
                if content.parse::<toml::Value>().is_ok() {
                    ok += 1;
                    println!("  [OK] conf/wfusion.toml: valid TOML");
                } else {
                    err += 1;
                    eprintln!("  [ERR] conf/wfusion.toml: invalid TOML");
                }
            }
            Err(e) => {
                err += 1;
                eprintln!("  [ERR] conf/wfusion.toml: read error: {e}");
            }
        }
    } else {
        err += 1;
        eprintln!("  [ERR] conf/wfusion.toml: not found");
    }

    // 2. models/rules/
    let rules_dir = root.join("models").join("rules");
    if rules_dir.is_dir() {
        let wfl_count = count_files(&rules_dir, "wfl");
        if wfl_count > 0 {
            ok += 1;
            println!("  [OK] models/rules/: {wfl_count} .wfl file(s)");
        } else {
            warn += 1;
            eprintln!("  [WARN] models/rules/: no .wfl files found");
        }
    } else {
        warn += 1;
        eprintln!("  [WARN] models/rules/: directory not found");
    }

    // 3. models/schemas/
    let schemas_dir = root.join("models").join("schemas");
    if schemas_dir.is_dir() {
        let wfs_count = count_files(&schemas_dir, "wfs");
        if wfs_count > 0 {
            ok += 1;
            println!("  [OK] models/schemas/: {wfs_count} .wfs file(s)");
        } else {
            warn += 1;
            eprintln!("  [WARN] models/schemas/: no .wfs files found");
        }
    } else {
        warn += 1;
        eprintln!("  [WARN] models/schemas/: directory not found");
    }

    // 4. models/scenarios/
    let scenarios_dir = root.join("models").join("scenarios");
    if scenarios_dir.is_dir() {
        let wfg_count = count_files(&scenarios_dir, "wfg");
        if wfg_count > 0 {
            ok += 1;
            println!("  [OK] models/scenarios/: {wfg_count} .wfg file(s)");
        } else {
            println!("  [INFO] models/scenarios/: no .wfg files");
        }
    }

    // 5. topology/sinks/
    let sinks_dir = root.join("topology").join("sinks");
    if sinks_dir.is_dir() {
        ok += 1;
        let parts = ["business.d", "infra.d"];
        for part in &parts {
            if sinks_dir.join(part).is_dir() {
                println!("  [OK] topology/sinks/{part}/: present");
            } else {
                warn += 1;
                eprintln!("  [WARN] topology/sinks/{part}/: not found");
            }
        }
        if sinks_dir.join("defaults.toml").exists() {
            println!("  [OK] topology/sinks/defaults.toml: present");
        } else {
            warn += 1;
            eprintln!("  [WARN] topology/sinks/defaults.toml: not found");
        }
    } else {
        warn += 1;
        eprintln!("  [WARN] topology/sinks/: directory not found");
    }

    // 6. topology/sources/
    let sources_dir = root.join("topology").join("sources");
    if sources_dir.is_dir() {
        ok += 1;
        println!("  [OK] topology/sources/: present");
    }

    // 7. connectors/
    let connectors_dir = root.join("connectors");
    if connectors_dir.is_dir() {
        let sink_d = connectors_dir.join("sink.d");
        if sink_d.is_dir() {
            ok += 1;
            println!("  [OK] connectors/sink.d/: present");
        } else {
            warn += 1;
            eprintln!("  [WARN] connectors/sink.d/: not found");
        }
    } else {
        warn += 1;
        eprintln!("  [WARN] connectors/: directory not found");
    }

    // Summary
    println!();
    if err > 0 {
        eprintln!("Result: {err} error(s), {warn} warning(s), {ok} ok");
        Err(format!("validation failed with {err} error(s)"))
    } else if warn > 0 {
        println!("Result: {warn} warning(s), {ok} ok (no errors)");
        Ok(())
    } else {
        println!("Result: all checks passed ({ok} ok)");
        Ok(())
    }
}

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
