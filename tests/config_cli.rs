use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("failed to create parent dir");
    }
    std::fs::write(path, content).expect("failed to write test file");
}

fn wfusion() -> Command {
    Command::new(env!("CARGO_BIN_EXE_wfusion"))
}

fn sample_config() -> &'static str {
    r#"
mode = "batch"
sinks = "${CASE_PATH}/sinks"
work_root = "$HOME"

[[sources]]
type = "file"
path = "${CASE_PATH}/data/base.ndjson"
stream = "syslog"
format = "ndjson"

[runtime]
executor_parallelism = 2
rule_exec_timeout = "30s"
schemas = "${CASE_PATH}/schemas/base/*.wfs"
rules = "${CASE_PATH}/rules/base/*.wfl"

[window_defaults]
evict_interval = "30s"
max_window_bytes = "256MB"
max_total_bytes = "2GB"
evict_policy = "time_first"
watermark = "5s"
allowed_lateness = "0s"
late_policy = "drop"

[window.base_events]
mode = "local"
max_window_bytes = "256MB"
over_cap = "30m"

[vars]
CASE_PATH = "/tmp/from-file"
FAIL_THRESHOLD = "3"
"#
}

#[test]
fn config_render_and_vars_reflect_cli_overrides() {
    let temp = TempDir::new().expect("create temp dir");
    let config_path = temp.path().join("conf/wfusion.toml");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    write_file(&config_path, sample_config());

    let render = wfusion()
        .arg("config")
        .arg("render")
        .arg("--config")
        .arg(&config_path)
        .arg("--work-dir")
        .arg(&workspace)
        .arg("--var")
        .arg("CASE_PATH=/tmp/from-cli")
        .output()
        .expect("run config render");
    assert!(
        render.status.success(),
        "render failed: {}",
        String::from_utf8_lossy(&render.stderr)
    );
    let render_stdout = String::from_utf8(render.stdout).expect("render stdout utf8");
    assert!(render_stdout.contains("sinks = \"/tmp/from-cli/sinks\""));
    assert!(render_stdout.contains("path = \"/tmp/from-cli/data/base.ndjson\""));

    let vars = wfusion()
        .arg("config")
        .arg("vars")
        .arg("--config")
        .arg(&config_path)
        .arg("--work-dir")
        .arg(&workspace)
        .arg("--var")
        .arg("CASE_PATH=/tmp/from-cli")
        .arg("--var-prefix")
        .arg("CASE_")
        .arg("--var-prefix")
        .arg("WORK_")
        .arg("--var-prefix")
        .arg("HOME")
        .output()
        .expect("run config vars");
    assert!(
        vars.status.success(),
        "vars failed: {}",
        String::from_utf8_lossy(&vars.stderr)
    );
    let vars_stdout = String::from_utf8(vars.stdout).expect("vars stdout utf8");
    assert!(vars_stdout.contains("CASE_PATH\t/tmp/from-cli\t<cli:CASE_PATH>"));
    assert!(vars_stdout.contains("WORK_DIR\t"));
    assert!(vars_stdout.contains("\t<builtin:WORK_DIR>"));
    let home = std::env::var("HOME").expect("HOME env var");
    assert!(vars_stdout.contains(&format!("HOME\t{home}\t<env:HOME>")));
}

#[test]
fn config_origins_and_diff_support_prefix_filters() {
    let temp = TempDir::new().expect("create temp dir");
    let config_path = temp.path().join("conf/wfusion.toml");
    let old_overlay = temp.path().join("env/dev/old.toml");
    let new_overlay = temp.path().join("env/dev/new.toml");
    write_file(&config_path, sample_config());
    write_file(
        &old_overlay,
        r#"
[runtime]
rules = "../rules/old/*.wfl"
"#,
    );
    write_file(
        &new_overlay,
        r#"
[runtime]
rules = "../rules/new/*.wfl"

[vars]
CASE_PATH = "/tmp/overlay"
"#,
    );

    let origins = wfusion()
        .arg("config")
        .arg("origins")
        .arg("--config")
        .arg(&config_path)
        .arg("--path-prefix")
        .arg("runtime")
        .output()
        .expect("run config origins");
    assert!(
        origins.status.success(),
        "origins failed: {}",
        String::from_utf8_lossy(&origins.stderr)
    );
    let origins_stdout = String::from_utf8(origins.stdout).expect("origins stdout utf8");
    assert!(origins_stdout.contains("runtime.rules"));
    assert!(!origins_stdout.contains("sinks\t"));

    let diff = wfusion()
        .arg("config")
        .arg("diff")
        .arg("--config")
        .arg(&config_path)
        .arg("--overlay")
        .arg(&old_overlay)
        .arg("--to-overlay")
        .arg(&new_overlay)
        .arg("--path-prefix")
        .arg("runtime")
        .output()
        .expect("run config diff");
    assert!(
        diff.status.success(),
        "diff failed: {}",
        String::from_utf8_lossy(&diff.stderr)
    );
    let diff_stdout = String::from_utf8(diff.stdout).expect("diff stdout utf8");
    assert!(diff_stdout.contains("path: runtime.rules"));
    assert!(!diff_stdout.contains("path: vars"));
    assert!(diff_stdout.contains(old_overlay.to_string_lossy().as_ref()));
    assert!(diff_stdout.contains(new_overlay.to_string_lossy().as_ref()));
}

#[test]
fn config_diff_expanded_detects_toml_value_changes_from_vars() {
    let temp = TempDir::new().expect("create temp dir");
    let config_path = temp.path().join("conf/wfusion.toml");
    write_file(&config_path, sample_config());

    let diff = wfusion()
        .arg("config")
        .arg("diff")
        .arg("--config")
        .arg(&config_path)
        .arg("--var")
        .arg("CASE_PATH=/tmp/left")
        .arg("--to-var")
        .arg("CASE_PATH=/tmp/right")
        .arg("--expanded")
        .arg("--path-prefix")
        .arg("sinks")
        .output()
        .expect("run expanded config diff");
    assert!(
        diff.status.success(),
        "expanded diff failed: {}",
        String::from_utf8_lossy(&diff.stderr)
    );
    let diff_stdout = String::from_utf8(diff.stdout).expect("diff stdout utf8");
    assert!(diff_stdout.contains("path: sinks"));
    assert!(diff_stdout.contains("/tmp/left/sinks"));
    assert!(diff_stdout.contains("/tmp/right/sinks"));
    assert!(diff_stdout.contains("old_origin: <mixed:file:"));
    assert!(diff_stdout.contains("new_origin: <mixed:file:"));
    assert!(diff_stdout.contains("cli:CASE_PATH>"));
}

#[test]
fn config_diff_expanded_reports_array_field_sources() {
    let temp = TempDir::new().expect("create temp dir");
    let config_path = temp.path().join("conf/wfusion.toml");
    write_file(&config_path, sample_config());

    let diff = wfusion()
        .arg("config")
        .arg("diff")
        .arg("--config")
        .arg(&config_path)
        .arg("--var")
        .arg("CASE_PATH=/tmp/left")
        .arg("--to-var")
        .arg("CASE_PATH=/tmp/right")
        .arg("--expanded")
        .arg("--path-prefix")
        .arg("sources")
        .output()
        .expect("run expanded config diff for array field");
    assert!(
        diff.status.success(),
        "expanded diff failed: {}",
        String::from_utf8_lossy(&diff.stderr)
    );
    let diff_stdout = String::from_utf8(diff.stdout).expect("diff stdout utf8");
    assert!(diff_stdout.contains("path: sources"));
    assert!(diff_stdout.contains("/tmp/left/data/base.ndjson"));
    assert!(diff_stdout.contains("/tmp/right/data/base.ndjson"));
    assert!(diff_stdout.contains("old_origin: <mixed:file:"));
    assert!(diff_stdout.contains("new_origin: <mixed:file:"));
    assert!(diff_stdout.contains("cli:CASE_PATH>"));
}
