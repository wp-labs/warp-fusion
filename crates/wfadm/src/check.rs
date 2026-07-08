// wfadm check — validate wf-rules project integrity.
//
// Mirrors the structure of wpadm's `project check`
// (`wparse/wp-motor/crates/wp-proj/src/project/checker/`): component-based
// selection (`--what`), `--json` / `--console` / `--only-fail` output modes,
// `--fail-fast` early exit, and a per-component "X/Y passed" summary with a
// detail table and failure details.
//
// The component set reflects wfusion's project layout (no oml / semantic_dict /
// wpgen): conf, sources, connectors, sinks, rules (wfl), schemas (wfs),
// scenarios (wfg).
//
// Missing model directories are treated as non-fatal (success-with-message),
// matching wpadm's `CheckStatus::Miss` semantics — a minimal-scope project
// (e.g. `init --mode conf`) legitimately lacks `models/rules` and must not
// fail the check solely on that basis. Only actual parse/validation errors
// are failures.

use std::fs;
use std::path::{Path, PathBuf};

use clap::Args;
use comfy_table::{Cell as TCell, ContentArrangement, Table, presets::UTF8_FULL};
use serde::Serialize;
use wf_lang::{lint_wfl, parse_wfg, parse_wfl, parse_wfs};

use crate::connectors;

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

// ── CLI ────────────────────────────────────────────────────────────────

/// `wfadm check` arguments — mirrors wpadm `ProjectCheckArgs`.
#[derive(Args, Debug, Clone)]
pub struct CheckArgs {
    /// 根目录 | Root path
    #[clap(short, long, default_value = ".", visible_alias = "工作目录")]
    pub work_root: String,
    /// 检查项：conf,sources,connectors,sinks,rules,schemas,scenarios,all | What to check
    #[clap(long = "what", default_value = "all", visible_alias = "检查项")]
    pub what: String,
    /// 强制日志输出到控制台 | Log to console
    #[clap(long, default_value_t = false, visible_alias = "控制台日志")]
    pub console: bool,
    /// 命中第一处失败立即退出 | Fail fast
    #[clap(long, default_value_t = false, visible_alias = "快速失败")]
    pub fail_fast: bool,
    /// JSON 输出 | JSON output
    #[clap(long = "json", default_value_t = false, visible_alias = "输出JSON")]
    pub json: bool,
    /// 仅输出失败项 | Only print failed items
    #[clap(long = "only-fail", default_value_t = false, visible_alias = "仅失败")]
    pub only_fail: bool,
}

// ── options / components ───────────────────────────────────────────────

#[derive(Clone, Debug, Default)]
struct CheckOptions {
    console: bool,
    fail_fast: bool,
    json: bool,
    only_fail: bool,
}

#[derive(Clone, Debug)]
struct CheckComponents {
    conf: bool,
    sources: bool,
    connectors: bool,
    sinks: bool,
    rules: bool,
    schemas: bool,
    scenarios: bool,
}

impl Default for CheckComponents {
    fn default() -> Self {
        Self {
            conf: true,
            sources: true,
            connectors: true,
            sinks: true,
            rules: true,
            schemas: true,
            scenarios: true,
        }
    }
}

impl CheckComponents {
    fn disable_all(&mut self) {
        self.conf = false;
        self.sources = false;
        self.connectors = false;
        self.sinks = false;
        self.rules = false;
        self.schemas = false;
        self.scenarios = false;
    }

    fn with_only<I: IntoIterator<Item = CheckComponent>>(mut self, comps: I) -> Self {
        self.disable_all();
        for c in comps {
            self.set(c, true);
        }
        self
    }

    fn set(&mut self, c: CheckComponent, v: bool) {
        match c {
            CheckComponent::Conf => self.conf = v,
            CheckComponent::Sources => self.sources = v,
            CheckComponent::Connectors => self.connectors = v,
            CheckComponent::Sinks => self.sinks = v,
            CheckComponent::Rules => self.rules = v,
            CheckComponent::Schemas => self.schemas = v,
            CheckComponent::Scenarios => self.scenarios = v,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CheckComponent {
    Conf,
    Sources,
    Connectors,
    Sinks,
    Rules,
    Schemas,
    Scenarios,
}

fn build_components(args: &CheckArgs) -> Result<CheckComponents, String> {
    let what = args.what.trim();
    if what.is_empty() || what.eq_ignore_ascii_case("all") {
        return Ok(CheckComponents::default());
    }
    let selections: Vec<_> = what
        .split(',')
        .filter_map(|t| parse_component(t.trim()))
        .collect();
    if selections.is_empty() {
        return Err(format!("unknown check target: '{}'", args.what));
    }
    Ok(CheckComponents::default().with_only(selections))
}

fn parse_component(token: &str) -> Option<CheckComponent> {
    match token.to_ascii_lowercase().as_str() {
        "conf" | "config" | "engine" => Some(CheckComponent::Conf),
        "sources" | "source" => Some(CheckComponent::Sources),
        "connectors" | "connector" | "conn" => Some(CheckComponent::Connectors),
        "sinks" | "sink" => Some(CheckComponent::Sinks),
        "rules" | "wpl" | "rule" => Some(CheckComponent::Rules),
        "schemas" | "schema" => Some(CheckComponent::Schemas),
        "scenarios" | "scenario" => Some(CheckComponent::Scenarios),
        "all" => None,
        _ => None,
    }
}

// ── result types ───────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct Cell {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    msg: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ok: true,
            msg: None,
            warnings: Vec::new(),
        }
    }
}

impl Cell {
    fn failure(msg: String) -> Self {
        Self {
            ok: false,
            msg: Some(msg),
            warnings: Vec::new(),
        }
    }
    fn success_with_message(msg: String) -> Self {
        Self {
            ok: true,
            msg: Some(msg),
            warnings: Vec::new(),
        }
    }
    fn success_with_warnings(msg: String, warnings: Vec<String>) -> Self {
        Self {
            ok: true,
            msg: Some(msg),
            warnings,
        }
    }
    fn skipped() -> Self {
        Self::success_with_message("skipped".to_string())
    }
}

#[derive(Clone, Debug, Default, Serialize)]
struct Row {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    conf_detail: Option<String>,
    conf: Cell,
    sources: Cell,
    connectors: Cell,
    sinks: Cell,
    rules: Cell,
    schemas: Cell,
    scenarios: Cell,
}

impl Row {
    fn new(path: String) -> Self {
        Self {
            path,
            ..Default::default()
        }
    }

    /// Mark every component as skipped. Components that get evaluated
    /// overwrite their cell; the rest stay skipped so that a `--fail-fast`
    /// early return never reports an unchecked component as "passed".
    fn all_skipped() -> [Cell; 7] {
        [
            Cell::skipped(),
            Cell::skipped(),
            Cell::skipped(),
            Cell::skipped(),
            Cell::skipped(),
            Cell::skipped(),
            Cell::skipped(),
        ]
    }

    fn count_failures(&self) -> usize {
        [
            !self.conf.ok,
            !self.sources.ok,
            !self.connectors.ok,
            !self.sinks.ok,
            !self.rules.ok,
            !self.schemas.ok,
            !self.scenarios.ok,
        ]
        .iter()
        .filter(|&&f| f)
        .count()
    }
}

// ── entry point ────────────────────────────────────────────────────────

pub fn run(args: CheckArgs) -> Result<(), String> {
    let comps = build_components(&args)?;
    let opts = CheckOptions {
        console: args.console,
        fail_fast: args.fail_fast,
        json: args.json,
        only_fail: args.only_fail,
    };
    check_project(&Path::new(&args.work_root).to_path_buf(), &opts, &comps)
}

fn check_project(root: &Path, opts: &CheckOptions, comps: &CheckComponents) -> Result<(), String> {
    connectors::ensure_factories_registered();

    let display_root = root
        .canonicalize()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| root.display().to_string());

    // Header is shown only in the default text mode — `--console` prints just
    // the table and `--json` must emit pure JSON on stdout.
    if !opts.json && !opts.console {
        println!("wfadm check: validating wf-rules project at {display_root}");
        let registered_sinks = wp_core_connectors::registry::list_sink_kinds();
        let registered_sources = wp_core_connectors::registry::list_source_kinds();
        println!(
            "  registered: {} sink(s), {} source(s)",
            registered_sinks.len(),
            registered_sources.len()
        );
    }

    let row = evaluate_target(root, opts, comps);

    let mut stats = SummaryCounts::default();
    record_component(&mut stats, comps, &row);

    render_output(&row, &stats, opts, comps, &display_root);

    if has_failures(&row, comps) {
        Err(format!(
            "project check failed: {} component(s) reported validation errors",
            row.count_failures()
        ))
    } else {
        Ok(())
    }
}

fn evaluate_target(root: &Path, opts: &CheckOptions, comps: &CheckComponents) -> Row {
    let mut row = Row::new(
        root.canonicalize()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| root.display().to_string()),
    );

    // Start from "all skipped" so an early `--fail-fast` return never leaves a
    // component looking like it passed. Evaluated components overwrite these.
    let [c_conf, c_sources, c_connectors, c_sinks, c_rules, c_schemas, c_scenarios] =
        Row::all_skipped();
    row.conf = c_conf;
    row.sources = c_sources;
    row.connectors = c_connectors;
    row.sinks = c_sinks;
    row.rules = c_rules;
    row.schemas = c_schemas;
    row.scenarios = c_scenarios;

    // Read conf/wfusion.toml once; reuse for both the conf check (structure)
    // and [runtime] path extraction. The conf check is structure-only so a
    // partial-scope project (e.g. `init --mode rules`, which has no topology)
    // is not rejected solely because `sources_dir` points at a missing dir —
    // directory existence is the job of the sources/sinks component checks,
    // matching wpadm's Miss semantics.
    let conf_value = read_conf_value(root);
    let (cfg_rules_path, cfg_schemas_path) = conf_value
        .as_ref()
        .map(extract_runtime_paths)
        .unwrap_or_default();
    let base_dir = root.to_path_buf();

    if comps.conf {
        let (cell, detail) = check_conf(root, conf_value.as_ref());
        row.conf = cell;
        row.conf_detail = detail;
        if !row.conf.ok && opts.fail_fast {
            return row;
        }
    }

    if comps.sources {
        row.sources = check_sources(root);
        if !row.sources.ok && opts.fail_fast {
            return row;
        }
    }

    if comps.connectors {
        row.connectors = check_connectors(root);
        if !row.connectors.ok && opts.fail_fast {
            return row;
        }
    }

    if comps.sinks {
        row.sinks = check_sinks(root);
        if !row.sinks.ok && opts.fail_fast {
            return row;
        }
    }

    if comps.rules {
        row.rules = check_rules(root, &base_dir, &cfg_rules_path);
        if !row.rules.ok && opts.fail_fast {
            return row;
        }
    }

    if comps.schemas {
        row.schemas = check_schemas(root, &base_dir, &cfg_schemas_path);
        if !row.schemas.ok && opts.fail_fast {
            return row;
        }
    }

    if comps.scenarios {
        row.scenarios = check_scenarios(root);
        if !row.scenarios.ok && opts.fail_fast {
            return row;
        }
    }

    row
}

// ── component checks ───────────────────────────────────────────────────

/// Structure-only check of `conf/wfusion.toml`: the file must exist and parse
/// as valid TOML. Semantic/engine-loadability validation (which requires
/// referenced dirs to exist) is intentionally NOT done here — it lives in the
/// `conf update` flow (`conf::validate_config_loads`) and the per-component
/// directory checks. This keeps `check` usable on partial-scope projects.
fn check_conf(root: &Path, conf_value: Option<&toml::Value>) -> (Cell, Option<String>) {
    let conf_path = root.join("conf").join("wfusion.toml");
    if !conf_path.exists() {
        return (Cell::failure("wfusion.toml — not found".to_string()), None);
    }
    let detail = conf_path
        .canonicalize()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| conf_path.display().to_string());
    match conf_value {
        Some(_) => (
            Cell::success_with_message("wfusion.toml".to_string()),
            Some(detail),
        ),
        None => (Cell::failure("wfusion.toml — invalid TOML".to_string()), None),
    }
}

/// Read and parse `conf/wfusion.toml` into a `toml::Value`. Returns `None`
/// when the file is absent or not valid TOML (the conf check reports those).
fn read_conf_value(root: &Path) -> Option<toml::Value> {
    let conf_path = root.join("conf").join("wfusion.toml");
    let content = fs::read_to_string(&conf_path).ok()?;
    toml::from_str(&content).ok()
}

/// Extract `[runtime].rules` / `[runtime].schemas` glob paths from an
/// already-parsed config.
fn extract_runtime_paths(value: &toml::Value) -> (Option<String>, Option<String>) {
    let runtime = match value.get("runtime") {
        Some(r) => r,
        None => return (None, None),
    };
    let rules = runtime
        .get("rules")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let schemas = runtime
        .get("schemas")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    (rules, schemas)
}

fn check_sources(root: &Path) -> Cell {
    let sources_dir = root.join("topology").join("sources");
    if !sources_dir.is_dir() {
        // Missing topology is a valid state for partial-scope projects
        // (e.g. `init --mode rules`); non-fatal, like wpadm's Miss.
        return Cell::success_with_message("sources/ — (missing)".to_string());
    }
    let (count, errs) = validate_toml_dir(&sources_dir, true);
    if errs > 0 {
        Cell::failure(format!("sources/ — {errs} invalid .toml file(s)"))
    } else {
        Cell::success_with_message(format!("sources/ ({count} .toml)"))
    }
}

fn check_connectors(root: &Path) -> Cell {
    // sink.d — local first, then upward.
    let sink_dir = root.join("connectors").join("sink.d");
    let sink_located = if sink_dir.is_dir() {
        Some(sink_dir)
    } else {
        find_upward(root, "connectors/sink.d").map(|(p, _)| p)
    };

    let source_dir = root.join("connectors").join("source.d");
    let source_located = if source_dir.is_dir() {
        Some(source_dir)
    } else {
        find_upward(root, "connectors/source.d").map(|(p, _)| p)
    };

    if sink_located.is_none() && source_located.is_none() {
        return Cell::success_with_message("connectors/ — (missing)".to_string());
    }

    let mut total = 0u32;
    let mut errs = 0u32;
    if let Some(dir) = &sink_located {
        let (n, e) = validate_toml_dir(dir, true);
        total += n;
        errs += e;
    }
    if let Some(dir) = &source_located {
        let (n, e) = validate_toml_dir(dir, true);
        total += n;
        errs += e;
    }

    if errs > 0 {
        Cell::failure(format!("connectors/ — {errs} invalid .toml file(s)"))
    } else {
        Cell::success_with_message(format!("connectors/ ({total} .toml)"))
    }
}

fn check_sinks(root: &Path) -> Cell {
    let sinks_dir = root.join("topology").join("sinks");
    if !sinks_dir.is_dir() {
        return Cell::success_with_message("sinks/ — (missing)".to_string());
    }

    let mut total = 0u32;
    let mut errs = 0u32;
    let mut missing: Vec<&str> = Vec::new();

    for part in ["business.d", "infra.d"] {
        let sub = sinks_dir.join(part);
        if sub.is_dir() {
            let (n, e) = validate_toml_dir(&sub, true);
            total += n;
            errs += e;
        } else {
            missing.push(part);
        }
    }

    let defaults = sinks_dir.join("defaults.toml");
    if defaults.exists() {
        total += 1;
        if let Err(e) = validate_toml_file(&defaults) {
            errs += 1;
            eprintln!("  {}  sinks/defaults.toml — {e}", red("✗"));
        }
    }

    let conn_dir = sinks_dir.join("connectors").join("sink.d");
    if conn_dir.is_dir() {
        let (n, e) = validate_toml_dir(&conn_dir, true);
        total += n;
        errs += e;
    }

    if errs > 0 {
        Cell::failure(format!("sinks/ — {errs} invalid .toml file(s)"))
    } else if !missing.is_empty() {
        Cell::success_with_warnings(
            format!("sinks/ ({total} .toml)"),
            vec![format!("missing subdirs: {}", missing.join(", "))],
        )
    } else {
        Cell::success_with_message(format!("sinks/ ({total} .toml)"))
    }
}

fn check_rules(root: &Path, base_dir: &Path, cfg_rules_path: &Option<String>) -> Cell {
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
                Cell::failure(format!("rules/ — {parse_errs} parse error(s)"))
            } else {
                let label = model_label(&resolved);
                Cell::success_with_message(format!(
                    "rules/{label} ({} .wfl, {lint_warnings} lint warning(s))",
                    resolved.files.len()
                ))
            }
        }
        None => Cell::success_with_message(missing_note("rules", cfg_rules_path)),
    }
}

fn check_schemas(root: &Path, base_dir: &Path, cfg_schemas_path: &Option<String>) -> Cell {
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

            // windows.toml is referenced by [windows] in wfusion.toml; validate it too.
            let windows_path = schemas_dir.join("windows.toml");
            let mut windows_err = false;
            if windows_path.exists() {
                if let Err(e) = validate_toml_file(&windows_path) {
                    windows_err = true;
                    eprintln!("  {}  windows.toml — {e}", red("✗"));
                }
            }

            if parse_errs > 0 || windows_err {
                Cell::failure(format!("schemas/ — {parse_errs} parse error(s)"))
            } else {
                let label = model_label(&resolved);
                Cell::success_with_message(format!(
                    "schemas/{label} ({} .wfs)",
                    resolved.files.len()
                ))
            }
        }
        None => Cell::success_with_message(missing_note("schemas", cfg_schemas_path)),
    }
}

fn check_scenarios(root: &Path) -> Cell {
    let scenarios_dir = root.join("models").join("scenarios");
    let wfg_files = list_files(&scenarios_dir, "wfg");
    if wfg_files.is_empty() {
        return Cell::success_with_message("scenarios/ — (none)".to_string());
    }
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
    if parse_errs > 0 {
        Cell::failure(format!("scenarios/ — {parse_errs} parse error(s)"))
    } else {
        Cell::success_with_message(format!("scenarios/ ({} .wfg)", wfg_files.len()))
    }
}

// ── model resolution helpers ───────────────────────────────────────────

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

/// Non-fatal message for a model group whose files could not be resolved.
/// `cfg_path` is included when the group was configured via `[runtime]`.
fn missing_note(label: &str, cfg_path: &Option<String>) -> String {
    match cfg_path {
        Some(p) => format!("{label}/ — (missing, config: {p})"),
        None => format!("{label}/ — (missing)"),
    }
}

fn model_label(resolved: &ResolvedFiles) -> String {
    if resolved.is_external {
        format!(
            " → {}",
            resolved.display_path.as_deref().unwrap_or("?")
        )
    } else {
        String::new()
    }
}

/// Recursively list files with a given extension.
fn list_files(dir: &Path, ext: &str) -> Vec<PathBuf> {
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

/// Resolve a glob-like path from config (e.g. "models/rules/*/*.wfl" or
/// "../shared/rules/*.wfl") relative to the project root, stripping the
/// trailing `/*.ext` glob to get the actual directory path.
fn resolve_config_dir(root: &Path, glob_path: &str) -> Option<PathBuf> {
    let dir_pattern = strip_glob(glob_path);
    root.join(dir_pattern)
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
fn validate_toml_dir(dir: &Path, quiet: bool) -> (u32, u32) {
    let mut files = 0u32;
    let mut errors = 0u32;
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let (n, e) = validate_toml_dir(&path, quiet);
                files += n;
                errors += e;
            } else if path.extension().map(|e| e == "toml").unwrap_or(false) {
                files += 1;
                if let Err(e) = validate_toml_file(&path) {
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

/// Parse a file as TOML — used for sources, connectors, sinks, defaults,
/// and windows.toml. Despite the generality, returns a structured error
/// suitable for display.
fn validate_toml_file(path: &Path) -> Result<(), String> {
    let content = fs::read_to_string(path).map_err(|e| format!("read error: {e}"))?;
    let _val: toml::Value = toml::from_str(&content).map_err(|e| format!("invalid TOML: {e}"))?;
    Ok(())
}

// ── summary / rendering ────────────────────────────────────────────────

#[derive(Default)]
struct ComponentCount {
    ok: usize,
    total: usize,
}

impl ComponentCount {
    fn record(&mut self, passed: bool) {
        self.total += 1;
        if passed {
            self.ok += 1;
        }
    }
}

#[derive(Default)]
struct SummaryCounts {
    conf: ComponentCount,
    sources: ComponentCount,
    connectors: ComponentCount,
    sinks: ComponentCount,
    rules: ComponentCount,
    schemas: ComponentCount,
    scenarios: ComponentCount,
}

/// True when a cell represents a component that was never evaluated (set by
/// `Row::all_skipped` and never overwritten, e.g. after a `--fail-fast` early
/// return). Such components must not be counted as "passed".
fn is_skipped(cell: &Cell) -> bool {
    cell.msg.as_deref() == Some("skipped")
}

/// Accumulate per-component pass/total counts from a finished row. Skipped
/// (un-evaluated) cells are excluded so they don't inflate the pass count.
fn record_component(stats: &mut SummaryCounts, comps: &CheckComponents, row: &Row) {
    if comps.conf && !is_skipped(&row.conf) {
        stats.conf.record(row.conf.ok);
    }
    if comps.sources && !is_skipped(&row.sources) {
        stats.sources.record(row.sources.ok);
    }
    if comps.connectors && !is_skipped(&row.connectors) {
        stats.connectors.record(row.connectors.ok);
    }
    if comps.sinks && !is_skipped(&row.sinks) {
        stats.sinks.record(row.sinks.ok);
    }
    if comps.rules && !is_skipped(&row.rules) {
        stats.rules.record(row.rules.ok);
    }
    if comps.schemas && !is_skipped(&row.schemas) {
        stats.schemas.record(row.schemas.ok);
    }
    if comps.scenarios && !is_skipped(&row.scenarios) {
        stats.scenarios.record(row.scenarios.ok);
    }
}

fn component_cells<'a>(row: &'a Row, comps: &CheckComponents) -> Vec<(&'static str, &'a Cell)> {
    let mut cells = Vec::new();
    if comps.conf {
        cells.push(("Config", &row.conf));
    }
    if comps.sources {
        cells.push(("Sources", &row.sources));
    }
    if comps.connectors {
        cells.push(("Connectors", &row.connectors));
    }
    if comps.sinks {
        cells.push(("Sinks", &row.sinks));
    }
    if comps.rules {
        cells.push(("Rules", &row.rules));
    }
    if comps.schemas {
        cells.push(("Schemas", &row.schemas));
    }
    if comps.scenarios {
        cells.push(("Scenarios", &row.scenarios));
    }
    cells
}

fn has_failures(row: &Row, comps: &CheckComponents) -> bool {
    (comps.conf && !row.conf.ok)
        || (comps.sources && !row.sources.ok)
        || (comps.connectors && !row.connectors.ok)
        || (comps.sinks && !row.sinks.ok)
        || (comps.rules && !row.rules.ok)
        || (comps.schemas && !row.schemas.ok)
        || (comps.scenarios && !row.scenarios.ok)
}

fn render_output(
    row: &Row,
    stats: &SummaryCounts,
    opts: &CheckOptions,
    comps: &CheckComponents,
    display_root: &str,
) {
    if opts.json {
        let value = build_json_output(row, stats, comps);
        println!(
            "{}",
            serde_json::to_string_pretty(&value)
                .expect("JSON serialize should not fail for Row/stat types")
        );
        return;
    }

    if opts.console {
        println!();
        println!("{}", build_detail_table(row, comps, opts, display_root));
        return;
    }

    print_text_summary(row, stats, comps, opts);
    println!("\n{}", build_detail_table(row, comps, opts, display_root));
    output_failure_details(row, comps, opts);
}

/// Build the `--json` payload as a JSON value, separated from printing so it
/// can be unit-tested. Shape mirrors wpadm: `{ "stat": { "total": N, ... },
/// "detail": [Row] }`.
fn build_json_output(row: &Row, stats: &SummaryCounts, comps: &CheckComponents) -> serde_json::Value {
    use serde_json::{Map, Value, json};
    let mut stat = Map::new();
    stat.insert("total".into(), Value::from(1));
    stat.insert("conf".into(), component_stat_value(comps.conf, &stats.conf));
    stat.insert(
        "sources".into(),
        component_stat_value(comps.sources, &stats.sources),
    );
    stat.insert(
        "connectors".into(),
        component_stat_value(comps.connectors, &stats.connectors),
    );
    stat.insert("sinks".into(), component_stat_value(comps.sinks, &stats.sinks));
    stat.insert("rules".into(), component_stat_value(comps.rules, &stats.rules));
    stat.insert(
        "schemas".into(),
        component_stat_value(comps.schemas, &stats.schemas),
    );
    stat.insert(
        "scenarios".into(),
        component_stat_value(comps.scenarios, &stats.scenarios),
    );

    let detail = serde_json::to_value(row).expect("Row serializes to JSON");
    json!({
        "stat": Value::Object(stat),
        "detail": Value::Array(vec![detail]),
    })
}

fn component_stat_value(enabled: bool, count: &ComponentCount) -> serde_json::Value {
    use serde_json::json;
    // `enabled && total > 0` means the component actually ran; anything else
    // (disabled, or skipped via fail-fast) serializes as null.
    if enabled && count.total > 0 {
        json!({ "passed": count.ok, "total": count.total })
    } else {
        serde_json::Value::Null
    }
}

fn print_text_summary(
    row: &Row,
    stats: &SummaryCounts,
    comps: &CheckComponents,
    opts: &CheckOptions,
) {
    println!("Project check completed");
    print_summary_line("Config", comps.conf, &stats.conf, &row.conf, opts);
    print_summary_line("Sources", comps.sources, &stats.sources, &row.sources, opts);
    print_summary_line(
        "Connectors",
        comps.connectors,
        &stats.connectors,
        &row.connectors,
        opts,
    );
    print_summary_line("Sinks", comps.sinks, &stats.sinks, &row.sinks, opts);
    print_summary_line("Rules", comps.rules, &stats.rules, &row.rules, opts);
    print_summary_line("Schemas", comps.schemas, &stats.schemas, &row.schemas, opts);
    print_summary_line(
        "Scenarios",
        comps.scenarios,
        &stats.scenarios,
        &row.scenarios,
        opts,
    );
}

fn print_summary_line(
    label: &str,
    enabled: bool,
    count: &ComponentCount,
    cell: &Cell,
    opts: &CheckOptions,
) {
    if !enabled || is_skipped(cell) {
        // Skipped components are informational; hide them under --only-fail
        // (which wants failures only), matching the detail-table filter.
        if !opts.only_fail {
            println!("{label}: skipped");
        }
        return;
    }
    if opts.only_fail && cell.ok {
        return;
    }
    let mark = if cell.ok { green("✓") } else { red("✗") };
    println!("{label}: {mark} {}/{} passed", count.ok, count.total);
}

fn build_detail_table(
    row: &Row,
    comps: &CheckComponents,
    opts: &CheckOptions,
    display_root: &str,
) -> Table {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_content_arrangement(ContentArrangement::Dynamic);
    table.set_header(vec![
        TCell::new("Category"),
        TCell::new("Item"),
        TCell::new("Data"),
        TCell::new("Result"),
    ]);
    let cat = |section: &str| format!("{} / {}", truncate_path(display_root, 1), section);
    for entry in detail_entries_for(row, comps, &cat, opts) {
        table.add_row(vec![
            TCell::new(entry.category),
            TCell::new(entry.item),
            TCell::new(entry.data),
            TCell::new(entry.result),
        ]);
    }
    table
}

struct DetailEntry {
    category: String,
    item: String,
    data: String,
    result: String,
}

fn detail_entries_for<F>(
    row: &Row,
    comps: &CheckComponents,
    cat: &F,
    opts: &CheckOptions,
) -> Vec<DetailEntry>
where
    F: Fn(&str) -> String,
{
    let mut entries = Vec::new();
    let mut push = |entry: DetailEntry, cell: &Cell| {
        if opts.only_fail && cell.ok {
            return;
        }
        entries.push(entry);
    };

    if comps.conf {
        let data = row
            .conf_detail
            .as_ref()
            .map(|p| truncate_path(p, 3))
            .unwrap_or_else(|| cell_data(&row.conf));
        push(
            DetailEntry {
                category: cat("Config"),
                item: "Engine config".into(),
                data,
                result: status_mark(&row.conf).to_string(),
            },
            &row.conf,
        );
    }
    if comps.sources {
        push(
            DetailEntry {
                category: cat("Sources"),
                item: "Topology".into(),
                data: cell_data(&row.sources),
                result: status_mark(&row.sources).to_string(),
            },
            &row.sources,
        );
    }
    if comps.connectors {
        push(
            DetailEntry {
                category: cat("Connectors"),
                item: "Definitions".into(),
                data: cell_data(&row.connectors),
                result: status_mark(&row.connectors).to_string(),
            },
            &row.connectors,
        );
    }
    if comps.sinks {
        push(
            DetailEntry {
                category: cat("Sinks"),
                item: "Targets".into(),
                data: cell_data(&row.sinks),
                result: status_mark(&row.sinks).to_string(),
            },
            &row.sinks,
        );
    }
    if comps.rules {
        push(
            DetailEntry {
                category: cat("Rules"),
                item: "Models (wfl)".into(),
                data: cell_data(&row.rules),
                result: status_mark(&row.rules).to_string(),
            },
            &row.rules,
        );
    }
    if comps.schemas {
        push(
            DetailEntry {
                category: cat("Schemas"),
                item: "Models (wfs)".into(),
                data: cell_data(&row.schemas),
                result: status_mark(&row.schemas).to_string(),
            },
            &row.schemas,
        );
    }
    if comps.scenarios {
        push(
            DetailEntry {
                category: cat("Scenarios"),
                item: "Models (wfg)".into(),
                data: cell_data(&row.scenarios),
                result: status_mark(&row.scenarios).to_string(),
            },
            &row.scenarios,
        );
    }

    // Non-blocking warnings (e.g. missing sink subdirs) as extra rows.
    for (label, cell) in component_cells(row, comps) {
        for w in &cell.warnings {
            entries.push(DetailEntry {
                category: cat(label),
                item: "Warning".into(),
                data: w.clone(),
                result: "!".into(),
            });
        }
    }

    entries
}

fn output_failure_details(row: &Row, comps: &CheckComponents, opts: &CheckOptions) {
    if opts.only_fail {
        // Only-fail mode: the detail table already shows just failures.
        return;
    }
    let failed: Vec<_> = component_cells(row, comps)
        .into_iter()
        .filter(|(_, c)| !c.ok)
        .collect();
    if failed.is_empty() {
        return;
    }
    println!("Failure details:");
    for (label, cell) in failed {
        let detail = cell.msg.as_deref().unwrap_or("no error message");
        println!("  - {} -> {}: {}", row.path, label, detail);
    }
}

fn status_mark(cell: &Cell) -> &'static str {
    if cell.ok {
        "✓"
    } else {
        "✗"
    }
}

fn cell_data(cell: &Cell) -> String {
    cell.msg.clone().unwrap_or_else(|| "ok".to_string())
}

/// Truncate a path to its last `levels` components, prefixed with `.../`.
fn truncate_path(path: &str, levels: usize) -> String {
    let p = Path::new(path);
    let components: Vec<_> = p.components().collect();
    if components.len() <= levels {
        return path.to_string();
    }
    let start = components.len() - levels;
    let truncated: Vec<_> = components[start..]
        .iter()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    format!(".../{}", truncated.join("/"))
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

    fn default_opts() -> CheckOptions {
        CheckOptions::default()
    }

    // ── component parsing ────────────────────────────────────────────

    #[test]
    fn parse_component_recognizes_aliases() {
        assert_eq!(parse_component("conf"), Some(CheckComponent::Conf));
        assert_eq!(parse_component("Config"), Some(CheckComponent::Conf));
        assert_eq!(parse_component("engine"), Some(CheckComponent::Conf));
        assert_eq!(parse_component("sources"), Some(CheckComponent::Sources));
        assert_eq!(parse_component("SOURCE"), Some(CheckComponent::Sources));
        assert_eq!(parse_component("conn"), Some(CheckComponent::Connectors));
        assert_eq!(parse_component("sinks"), Some(CheckComponent::Sinks));
        assert_eq!(parse_component("wpl"), Some(CheckComponent::Rules));
        assert_eq!(parse_component("rules"), Some(CheckComponent::Rules));
        assert_eq!(parse_component("schemas"), Some(CheckComponent::Schemas));
        assert_eq!(parse_component("scenario"), Some(CheckComponent::Scenarios));
        assert_eq!(parse_component("all"), None);
        assert_eq!(parse_component("nonsense"), None);
    }

    #[test]
    fn build_components_all_default() {
        let args = CheckArgs {
            work_root: ".".into(),
            what: "all".into(),
            console: false,
            fail_fast: false,
            json: false,
            only_fail: false,
        };
        let comps = build_components(&args).unwrap();
        assert!(comps.conf && comps.sources && comps.connectors && comps.sinks);
        assert!(comps.rules && comps.schemas && comps.scenarios);
    }

    #[test]
    fn build_components_empty_means_all() {
        let args = CheckArgs {
            work_root: ".".into(),
            what: "  ".into(),
            console: false,
            fail_fast: false,
            json: false,
            only_fail: false,
        };
        let comps = build_components(&args).unwrap();
        assert!(comps.conf && comps.rules);
    }

    #[test]
    fn build_components_selects_only_listed() {
        let args = CheckArgs {
            work_root: ".".into(),
            what: "conf,sinks".into(),
            console: false,
            fail_fast: false,
            json: false,
            only_fail: false,
        };
        let comps = build_components(&args).unwrap();
        assert!(comps.conf);
        assert!(comps.sinks);
        assert!(!comps.sources);
        assert!(!comps.connectors);
        assert!(!comps.rules);
    }

    #[test]
    fn build_components_all_token_filtered_from_list() {
        // `conf,all` → "all" contributes nothing, so only conf remains.
        let args = CheckArgs {
            work_root: ".".into(),
            what: "conf,all".into(),
            console: false,
            fail_fast: false,
            json: false,
            only_fail: false,
        };
        let comps = build_components(&args).unwrap();
        assert!(comps.conf);
        assert!(!comps.sinks);
    }

    #[test]
    fn build_components_rejects_unknown() {
        let args = CheckArgs {
            work_root: ".".into(),
            what: "bogus".into(),
            console: false,
            fail_fast: false,
            json: false,
            only_fail: false,
        };
        assert!(build_components(&args).is_err());
    }

    // ── Cell / Row ───────────────────────────────────────────────────

    #[test]
    fn row_count_failures() {
        let mut row = Row::new("/tmp".into());
        row.sources = Cell::failure("bad".into());
        row.rules = Cell::failure("boom".into());
        assert_eq!(row.count_failures(), 2);
    }

    #[test]
    fn skipped_cell_is_ok_with_skipped_message() {
        let cell = Cell::skipped();
        assert!(cell.ok);
        assert_eq!(cell.msg.as_deref(), Some("skipped"));
    }

    #[test]
    fn skipped_components_are_not_counted_as_passed() {
        // Simulate a fail-fast row: conf failed, sources never evaluated.
        let mut row = Row::new("/tmp".into());
        row.conf = Cell::failure("bad".into());
        row.sources = Cell::skipped();

        let mut stats = SummaryCounts::default();
        record_component(&mut stats, &CheckComponents::default(), &row);

        assert_eq!(stats.conf.total, 1);
        assert_eq!(stats.conf.ok, 0);
        assert_eq!(
            stats.sources.total, 0,
            "skipped component must not count toward total"
        );
        // A skipped-but-enabled component serializes as null (not passed:0/total:0).
        assert!(component_stat_value(true, &stats.sources).is_null());
        // An evaluated component still serializes as {passed, total}.
        assert!(component_stat_value(true, &stats.conf).is_object());
    }

    // ── missing model dirs are non-fatal (wpadm Miss semantics) ──────

    #[test]
    fn missing_rules_dir_is_nonfatal() {
        // conf-scope-like project: conf present, models/rules absent.
        let dir = temp_dir();
        std::fs::create_dir_all(dir.join("conf")).unwrap();
        std::fs::write(dir.join("conf/wfusion.toml"), "sinks = \"tmp\"\n").unwrap();
        // No models/rules directory at all.
        let cell = check_rules(&dir, &dir, &None);
        assert!(cell.ok, "missing rules dir must not fail: {:?}", cell.msg);
        assert!(cell.msg.as_deref().unwrap_or("").contains("missing"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_schemas_dir_is_nonfatal() {
        let dir = temp_dir();
        std::fs::create_dir_all(dir.join("conf")).unwrap();
        std::fs::write(dir.join("conf/wfusion.toml"), "sinks = \"tmp\"\n").unwrap();
        let cell = check_schemas(&dir, &dir, &None);
        assert!(cell.ok, "missing schemas dir must not fail: {:?}", cell.msg);
        assert!(cell.msg.as_deref().unwrap_or("").contains("missing"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_scenarios_dir_is_nonfatal() {
        let dir = temp_dir();
        std::fs::create_dir_all(dir.join("conf")).unwrap();
        std::fs::write(dir.join("conf/wfusion.toml"), "sinks = \"tmp\"\n").unwrap();
        let cell = check_scenarios(&dir);
        assert!(cell.ok, "{:?}", cell.msg);
        assert_eq!(cell.msg.as_deref(), Some("scenarios/ — (none)"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn conf_scope_project_passes_full_check() {
        // A real `init --mode conf` project (topology + conf + connectors, no
        // models) must pass `check` with default `--what all`: missing
        // rules/schemas/scenarios are non-fatal.
        let dir = temp_dir();
        crate::init::init_project(dir.to_str().unwrap(), "test", "conf")
            .expect("init conf-scope project");

        let result = check_project(&dir, &default_opts(), &CheckComponents::default());
        assert!(result.is_ok(), "conf-scope project should pass: {:?}", result);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rules_scope_project_passes_full_check() {
        // A real `init --mode rules` project (models + conf + connectors, no
        // topology) must pass `check` with default `--what all`: missing
        // sources/sinks are non-fatal, and conf is structure-only so the
        // absent `sources_dir` does not reject the config.
        let dir = temp_dir();
        crate::init::init_project(dir.to_str().unwrap(), "test", "rules")
            .expect("init rules-scope project");

        let result = check_project(&dir, &default_opts(), &CheckComponents::default());
        assert!(result.is_ok(), "rules-scope project should pass: {:?}", result);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_sources_dir_is_nonfatal() {
        let dir = temp_dir();
        std::fs::create_dir_all(dir.join("conf")).unwrap();
        std::fs::write(dir.join("conf/wfusion.toml"), "sinks = \"tmp\"\n").unwrap();
        let cell = check_sources(&dir);
        assert!(cell.ok, "missing sources dir must not fail: {:?}", cell.msg);
        assert!(cell.msg.as_deref().unwrap_or("").contains("missing"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_sinks_dir_is_nonfatal() {
        let dir = temp_dir();
        std::fs::create_dir_all(dir.join("conf")).unwrap();
        std::fs::write(dir.join("conf/wfusion.toml"), "sinks = \"tmp\"\n").unwrap();
        let cell = check_sinks(&dir);
        assert!(cell.ok, "missing sinks dir must not fail: {:?}", cell.msg);
        assert!(cell.msg.as_deref().unwrap_or("").contains("missing"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_connectors_dirs_are_nonfatal() {
        let dir = temp_dir();
        std::fs::create_dir_all(dir.join("conf")).unwrap();
        std::fs::write(dir.join("conf/wfusion.toml"), "sinks = \"tmp\"\n").unwrap();
        let cell = check_connectors(&dir);
        assert!(cell.ok, "missing connectors must not fail: {:?}", cell.msg);
        assert!(cell.msg.as_deref().unwrap_or("").contains("missing"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── fail-fast marks unchecked components as skipped ──────────────

    #[test]
    fn fail_fast_leaves_unchecked_as_skipped() {
        let dir = temp_dir();
        std::fs::create_dir_all(dir.join("conf")).unwrap();
        // Invalid conf → conf check fails → fail-fast returns immediately.
        std::fs::write(dir.join("conf/wfusion.toml"), "not valid [[[").unwrap();

        let opts = CheckOptions {
            fail_fast: true,
            ..Default::default()
        };
        let row = evaluate_target(&dir, &opts, &CheckComponents::default());

        assert!(!row.conf.ok, "conf should have failed");
        // Sources was never evaluated (fail-fast after conf) — must read as
        // skipped, NOT as a silent "passed".
        assert!(row.sources.ok);
        assert_eq!(row.sources.msg.as_deref(), Some("skipped"));
        assert_eq!(row.rules.msg.as_deref(), Some("skipped"));
        assert_eq!(row.scenarios.msg.as_deref(), Some("skipped"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fail_fast_still_reports_failure() {
        let dir = temp_dir();
        std::fs::create_dir_all(dir.join("conf")).unwrap();
        std::fs::write(dir.join("conf/wfusion.toml"), "not valid [[[").unwrap();
        let opts = CheckOptions {
            fail_fast: true,
            ..Default::default()
        };
        let result = check_project(&dir, &opts, &CheckComponents::default());
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── JSON shape parity with wpadm ─────────────────────────────────

    #[test]
    fn json_output_has_total_and_array_detail() {
        let dir = temp_dir();
        crate::init::init_project(dir.to_str().unwrap(), "test", "normal")
            .expect("init normal project");

        let row = evaluate_target(&dir, &default_opts(), &CheckComponents::default());
        let mut stats = SummaryCounts::default();
        record_component(&mut stats, &CheckComponents::default(), &row);
        let value = build_json_output(&row, &stats, &CheckComponents::default());

        let stat = value.get("stat").expect("stat present");
        assert_eq!(stat.get("total").and_then(|v| v.as_u64()), Some(1));
        // detail is an array (wpadm shape), not a bare object.
        let detail = value.get("detail").expect("detail present");
        assert!(detail.is_array(), "detail must be an array");
        assert_eq!(detail.as_array().unwrap().len(), 1);
        // Each enabled component has a {passed, total} stat object.
        let conf_stat = stat.get("conf").expect("conf stat");
        assert_eq!(conf_stat.get("passed").and_then(|v| v.as_u64()), Some(1));
        assert_eq!(conf_stat.get("total").and_then(|v| v.as_u64()), Some(1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn json_stat_null_for_disabled_components() {
        let dir = temp_dir();
        crate::init::init_project(dir.to_str().unwrap(), "test", "normal")
            .expect("init normal project");

        let row = evaluate_target(&dir, &default_opts(), &CheckComponents::default());
        let mut stats = SummaryCounts::default();
        let comps = CheckComponents::default().with_only([CheckComponent::Conf]);
        record_component(&mut stats, &comps, &row);
        let value = build_json_output(&row, &stats, &comps);

        let stat = value.get("stat").unwrap();
        assert!(stat.get("conf").unwrap().is_object());
        assert!(stat.get("sinks").unwrap().is_null(), "disabled => null");
        assert!(stat.get("rules").unwrap().is_null());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── end-to-end check_project ─────────────────────────────────────

    #[test]
    fn empty_dir_has_missing_conf() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let result = check_project(&dir, &default_opts(), &CheckComponents::default());
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dir_with_conf_passes_conf_check() {
        let dir = temp_dir();
        std::fs::create_dir_all(dir.join("conf")).unwrap();
        std::fs::write(dir.join("conf/wfusion.toml"), "mode = \"daemon\"\n").unwrap();
        let _ = check_project(&dir, &default_opts(), &CheckComponents::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_toml_detected() {
        let dir = temp_dir();
        std::fs::create_dir_all(dir.join("conf")).unwrap();
        std::fs::write(dir.join("conf/wfusion.toml"), "not valid toml [[[").unwrap();
        let result = check_project(&dir, &default_opts(), &CheckComponents::default());
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
        let result = check_project(&dir, &default_opts(), &CheckComponents::default());
        assert!(result.is_err());
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
        assert!(validate_toml_file(&dir.join("topology/sinks/business.d/scan.toml")).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── extract_runtime_paths / read_conf_value ──────────────────────

    #[test]
    fn extract_runtime_paths_reads_rules_and_schemas() {
        let v: toml::Value = toml::from_str(
            r#"
[runtime]
rules = "models/rules/*/*.wfl"
schemas = "models/schemas/*.wfs"
"#,
        )
        .unwrap();
        let (rules, schemas) = extract_runtime_paths(&v);
        assert_eq!(rules.as_deref(), Some("models/rules/*/*.wfl"));
        assert_eq!(schemas.as_deref(), Some("models/schemas/*.wfs"));
    }

    #[test]
    fn extract_runtime_paths_none_without_runtime_section() {
        let v: toml::Value = toml::from_str("sinks = \"tmp\"\n").unwrap();
        let (rules, schemas) = extract_runtime_paths(&v);
        assert!(rules.is_none());
        assert!(schemas.is_none());
    }

    #[test]
    fn read_conf_value_returns_none_for_missing_or_invalid() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        assert!(read_conf_value(&dir).is_none());

        std::fs::create_dir_all(dir.join("conf")).unwrap();
        std::fs::write(dir.join("conf/wfusion.toml"), "not valid [[[").unwrap();
        assert!(read_conf_value(&dir).is_none());

        std::fs::write(dir.join("conf/wfusion.toml"), "ok = true\n").unwrap();
        assert!(read_conf_value(&dir).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── missing_note ─────────────────────────────────────────────────

    #[test]
    fn missing_note_includes_config_when_set() {
        assert_eq!(
            missing_note("rules", &None),
            "rules/ — (missing)"
        );
        assert_eq!(
            missing_note("rules", &Some("models/rules/*/*.wfl".into())),
            "rules/ — (missing, config: models/rules/*/*.wfl)"
        );
    }

    // ── list_files ───────────────────────────────────────────────────

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

    // ── model deep parse ─────────────────────────────────────────────

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
        let cell = check_rules(&dir, &dir, &None);
        assert!(cell.ok, "valid WFL should parse: {:?}", cell.msg);
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
        let cell = check_rules(&dir, &dir, &None);
        assert!(!cell.ok, "invalid WFL should fail: {:?}", cell.msg);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wfl_parses_without_close_block() {
        let dir = temp_dir();
        setup_models_dir(&dir);
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
        let cell = check_rules(&dir, &dir, &None);
        assert!(cell.ok, "{:?}", cell.msg);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn valid_wfs_parses() {
        let dir = temp_dir();
        setup_models_dir(&dir);
        let real_wfs = "window conn_events {\n    stream = \"conn_events\"\n    time = event_time\n    over = 30m\n    fields {\n        event_time: time\n        sip: ip\n        dip: ip\n    }\n}";
        std::fs::write(dir.join("models/schemas/test.wfs"), real_wfs).unwrap();
        let cell = check_schemas(&dir, &dir, &None);
        assert!(cell.ok, "valid WFS should parse: {:?}", cell.msg);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_wfs_detected() {
        let dir = temp_dir();
        setup_models_dir(&dir);
        std::fs::write(dir.join("models/schemas/bad.wfs"), "}}}} gibberish").unwrap();
        let cell = check_schemas(&dir, &dir, &None);
        assert!(!cell.ok, "invalid WFS should fail: {:?}", cell.msg);
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
        let cell = check_scenarios(&dir);
        assert!(cell.ok, "valid WFG should parse: {:?}", cell.msg);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_wfg_detected() {
        let dir = temp_dir();
        setup_models_dir(&dir);
        std::fs::write(dir.join("models/scenarios/bad.wfg"), "not wfg").unwrap();
        let cell = check_scenarios(&dir);
        assert!(!cell.ok, "invalid WFG should fail: {:?}", cell.msg);
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
        let cell = check_scenarios(&dir);
        assert!(cell.ok, "{:?}", cell.msg);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── strip_glob ───────────────────────────────────────────────────

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

    // ── resolve_config_dir (root-relative) ───────────────────────────

    #[test]
    fn resolve_config_dir_finds_existing_directory() {
        let dir = temp_dir();
        let models = dir.join("shared/models/rules/wfl");
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(models.join("test.wfl"), "rule x {}").unwrap();

        // Config globs are resolved relative to the project root.
        let result = resolve_config_dir(&dir, "shared/models/rules/wfl/*.wfl");
        assert!(result.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_config_dir_returns_none_for_missing_directory() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();

        let result = resolve_config_dir(&dir, "nonexistent/path/*.wfl");
        assert!(result.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── find_upward ──────────────────────────────────────────────────

    #[test]
    fn find_upward_finds_sibling_directory() {
        let dir = temp_dir();
        let project = dir.join("a/b/project");
        let shared = dir.join("a/b/shared/connectors/sink.d");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::write(shared.join("test.toml"), "key = \"val\"").unwrap();

        let (found, depth) = find_upward(&project, "shared/connectors/sink.d").unwrap();
        assert_eq!(depth, 1);
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

    // ── external config paths (root-relative) ────────────────────────

    #[test]
    fn check_rules_resolves_external_from_config() {
        let dir = temp_dir();
        setup_models_dir(&dir);
        std::fs::remove_dir_all(dir.join("models/rules")).unwrap();

        let ext = dir.join("shared/rules");
        std::fs::create_dir_all(&ext).unwrap();
        std::fs::write(
            ext.join("test.wfl"),
            "use \"auth.wfs\"\nrule test_rule { events { e : auth_events } on each e where true -> score(1.0) entity(ip, e.sip) yield auth_alerts (sip = e.sip, alert_type = \"t\") }\n",
        )
        .unwrap();

        // External glob resolved relative to the project root.
        let cfg_rules = Some("shared/rules/*.wfl".to_string());
        let cell = check_rules(&dir, &dir, &cfg_rules);
        assert!(cell.ok, "external WFL should be resolved: {:?}", cell.msg);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_rules_missing_external_path_is_nonfatal() {
        let dir = temp_dir();
        setup_models_dir(&dir);
        std::fs::remove_dir_all(dir.join("models/rules")).unwrap();

        // Configured path does not resolve — non-fatal (wpadm Miss semantics).
        let cfg_rules = Some("nonexistent/*.wfl".to_string());
        let cell = check_rules(&dir, &dir, &cfg_rules);
        assert!(cell.ok, "missing external rules must not fail: {:?}", cell.msg);
        assert!(cell.msg.as_deref().unwrap_or("").contains("missing"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_schemas_resolves_external_from_config() {
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

        let cfg_schemas = Some("shared/schemas/*.wfs".to_string());
        let cell = check_schemas(&dir, &dir, &cfg_schemas);
        assert!(cell.ok, "external WFS should be resolved: {:?}", cell.msg);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── truncate_path ────────────────────────────────────────────────

    #[test]
    fn truncate_path_short() {
        assert_eq!(truncate_path("my_example", 1), "my_example");
        assert_eq!(truncate_path("my_example", 3), "my_example");
    }

    #[test]
    fn truncate_path_long() {
        let path = "a/b/c/d/wfusion.toml";
        assert_eq!(truncate_path(path, 3), ".../c/d/wfusion.toml");
    }

    #[test]
    fn truncate_path_relative() {
        assert_eq!(truncate_path("./conf/wfusion.toml", 3), "./conf/wfusion.toml");
        assert_eq!(truncate_path("./conf/wfusion.toml", 2), ".../conf/wfusion.toml");
    }
}
