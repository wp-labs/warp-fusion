//! Functional tests for `--no-oracle` / `--no-wfl` (issue #58).
//!
//! When the WFL pipeline is opted out, the tool must skip rule loading
//! (`_global.wfl` / yield-preset evaluation), WFL compilation and oracle /
//! expected-output generation, and only generate baseline scenario events.
//! These tests use the `examples/count` fixture, which declares an `expect`
//! block (so normal mode would emit `.except.jsonl` sidecars).

use std::collections::HashMap;
use std::path::PathBuf;

use wfgen::cmd_gen::run;
use wfgen::datagen::generate;
use wfgen::loader::load_from_uses;
use wfgen::validate::validate_wfg;
use wfgen::wfg_parser::parse_wfg;

const WFG_REL: &str = "examples/count/scenarios/brute_force.wfg";

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn skip_wfl_loads_schemas_but_no_rules() {
    let wfg_path = manifest().join(WFG_REL);
    let content = std::fs::read_to_string(&wfg_path).expect("read wfg");
    let wfg = parse_wfg(&content).expect("parse wfg");

    let (schemas, wfl_files) =
        load_from_uses(&wfg, &wfg_path, &HashMap::new(), true).expect("load with skip_wfl");
    assert!(!schemas.is_empty(), "schemas must still load under skip_wfl");
    assert!(wfl_files.is_empty(), "WFL files must not load under skip_wfl");
}

#[test]
fn skip_wfl_validation_passes_without_rules() {
    let wfg_path = manifest().join(WFG_REL);
    let content = std::fs::read_to_string(&wfg_path).expect("read wfg");
    let wfg = parse_wfg(&content).expect("parse wfg");

    let (schemas, wfl_files) =
        load_from_uses(&wfg, &wfg_path, &HashMap::new(), true).expect("load with skip_wfl");
    let errors = validate_wfg(&wfg, &schemas, &wfl_files, true);
    assert!(
        errors.is_empty(),
        "validation must pass under skip_wfl (no rule-not-found errors): {:?}",
        errors
    );
}

#[test]
fn skip_wfl_generates_baseline_events() {
    let wfg_path = manifest().join(WFG_REL);
    let content = std::fs::read_to_string(&wfg_path).expect("read wfg");
    let wfg = parse_wfg(&content).expect("parse wfg");

    let (schemas, _wfl_files) =
        load_from_uses(&wfg, &wfg_path, &HashMap::new(), true).expect("load with skip_wfl");
    let result = generate(&wfg, &schemas, &[]).expect("generate with empty rule_plans");
    assert!(
        !result.events.is_empty(),
        "baseline events must be generated without rule plans"
    );
}

#[tokio::test]
async fn no_oracle_run_writes_events_without_sidecars() {
    let tmp = std::env::temp_dir().join("wfgen-e2e-no-oracle");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("create temp out dir");
    let out = tmp.clone();

    let res = run(
        manifest().join(WFG_REL),
        "jsonl".to_string(),
        Some(out.clone()),
        Vec::new(), // ws
        Vec::new(), // wfl
        false,      // no_wfl
        true,       // no_oracle
        false,      // send
        "127.0.0.1:1".to_string(),
    )
    .await;
    assert!(res.is_ok(), "--no-oracle run failed: {:?}", res.err());

    let files: Vec<String> = std::fs::read_dir(&out)
        .expect("read out dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        files.iter().any(|f| f.ends_with(".jsonl") && !f.contains(".except.")),
        "events file written: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f.contains(".except.")),
        "no oracle sidecars under --no-oracle: {files:?}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[tokio::test]
async fn no_wfl_run_writes_events_without_sidecars() {
    // `--no-wfl` is equivalent to `--no-oracle` (both skip the whole WFL
    // pipeline); confirm the flag works identically.
    let tmp = std::env::temp_dir().join("wfgen-e2e-no-wfl");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("create temp out dir");
    let out = tmp.clone();

    let res = run(
        manifest().join(WFG_REL),
        "jsonl".to_string(),
        Some(out.clone()),
        Vec::new(), // ws
        Vec::new(), // wfl
        true,       // no_wfl
        false,      // no_oracle
        false,      // send
        "127.0.0.1:1".to_string(),
    )
    .await;
    assert!(res.is_ok(), "--no-wfl run failed: {:?}", res.err());

    let files: Vec<String> = std::fs::read_dir(&out)
        .expect("read out dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        files.iter().any(|f| f.ends_with(".jsonl") && !f.contains(".except.")),
        "events file written: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f.contains(".except.")),
        "no oracle sidecars under --no-wfl: {files:?}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn skip_wfl_generation_drops_injected_fixed_values() {
    // The fixture injects `use(action="failed")` / `use(action="success")` fixed
    // values. Under skip_wfl (empty rule_plans → no inject-aware generation),
    // those fixed values must not dominate: events are pure baseline with random
    // `action`.
    let wfg_path = manifest().join(WFG_REL);
    let content = std::fs::read_to_string(&wfg_path).expect("read wfg");
    let wfg = parse_wfg(&content).expect("parse wfg");

    let (schemas, _wfl_files) =
        load_from_uses(&wfg, &wfg_path, &HashMap::new(), true).expect("load with skip_wfl");
    let result = generate(&wfg, &schemas, &[]).expect("generate with empty rule_plans");

    let total = result.events.len();
    assert!(total > 0, "baseline events must be generated");
    let failed = result
        .events
        .iter()
        .filter(|e| e.fields.get("action").and_then(|v| v.as_str()) == Some("failed"))
        .count();
    assert!(
        failed < total / 20,
        "injected fixed values must not appear under skip_wfl (failed={failed}/{total})"
    );
}

#[tokio::test]
async fn no_oracle_run_skips_cli_wfl_files() {
    // A .wfl passed via the CLI `--wfl` flag is also skipped under --no-oracle:
    // it must not be loaded or compiled, so the run still succeeds with no WFL
    // errors and no sidecars.
    let tmp = std::env::temp_dir().join("wfgen-e2e-no-oracle-wfl");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("create temp out dir");
    let out = tmp.clone();

    let wfl_file = manifest().join("examples/count/rules/brute_force.wfl");
    let res = run(
        manifest().join(WFG_REL),
        "jsonl".to_string(),
        Some(out.clone()),
        Vec::new(),        // ws
        vec![wfl_file],    // wfl — must be skipped
        false,             // no_wfl
        true,              // no_oracle
        false,             // send
        "127.0.0.1:1".to_string(),
    )
    .await;
    assert!(
        res.is_ok(),
        "--no-oracle with CLI --wfl failed: {:?}",
        res.err()
    );

    let files: Vec<String> = std::fs::read_dir(&out)
        .expect("read out dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        !files.iter().any(|f| f.contains(".except.")),
        "no oracle sidecars: {files:?}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
