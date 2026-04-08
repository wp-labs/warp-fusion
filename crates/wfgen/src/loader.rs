use std::collections::HashMap;
use std::path::Path;

use anyhow::Context;

use wf_config::load_wfl_with_context;
use wf_vars::ConfigVarContext;

use crate::wfg_ast::WfgFile;
use crate::wfg_parser::parse_wfg;

/// Everything loaded and compiled from a `.wfg` scenario and its `use` declarations.
pub struct LoadedScenario {
    pub wfg: WfgFile,
    pub schemas: Vec<wf_lang::WindowSchema>,
    pub wfl_files: Vec<wf_lang::ast::WflFile>,
    pub rule_plans: Vec<wf_lang::plan::RulePlan>,
}

/// Load a `.wfg` scenario file, resolve `use` declarations, and compile rules.
///
/// `.wfl` files are preprocessed with `vars` before parsing, falling back to
/// environment variables for any undefined references.
pub fn load_scenario(
    wfg_path: &Path,
    vars: &HashMap<String, String>,
) -> anyhow::Result<LoadedScenario> {
    let wfg_content = std::fs::read_to_string(wfg_path)
        .with_context(|| format!("reading .wfg file: {}", wfg_path.display()))?;
    let wfg = parse_wfg(&wfg_content)
        .with_context(|| format!("parsing .wfg file: {}", wfg_path.display()))?;

    let (schemas, wfl_files) = load_from_uses(&wfg, wfg_path, vars)?;

    let mut rule_plans = Vec::new();
    for wfl_file in &wfl_files {
        let plans = wf_lang::compile_wfl(wfl_file, &schemas).context("compiling .wfl rules")?;
        rule_plans.extend(plans);
    }

    Ok(LoadedScenario {
        wfg,
        schemas,
        wfl_files,
        rule_plans,
    })
}

/// Load `.wfs` schemas and `.wfl` rule files referenced by `use` declarations.
///
/// Paths in `use` declarations are resolved relative to `wfg_path`'s directory.
/// `.wfl` sources are preprocessed with `vars` (and environment variable
/// fallback) via [`wf_lang::preprocess_vars_with_env`] before parsing.
pub fn load_from_uses(
    wfg: &WfgFile,
    wfg_path: &Path,
    vars: &HashMap<String, String>,
) -> anyhow::Result<(Vec<wf_lang::WindowSchema>, Vec<wf_lang::ast::WflFile>)> {
    let base_dir = wfg_path.parent().unwrap_or_else(|| Path::new("."));
    let mut wfl_vars = vars.clone();
    wfl_vars
        .entry("WORK_DIR".to_string())
        .or_insert_with(|| base_dir.to_string_lossy().to_string());
    let wfl_ctx = ConfigVarContext::from_explicit_vars(wfl_vars);

    let mut schemas = Vec::new();
    let mut wfl_files = Vec::new();

    for use_decl in &wfg.uses {
        let resolved = base_dir.join(&use_decl.path);
        let ext = resolved.extension().and_then(|e| e.to_str()).unwrap_or("");

        match ext {
            "wfs" => {
                let content = std::fs::read_to_string(&resolved).with_context(|| {
                    format!("reading .wfs from use declaration: {}", resolved.display())
                })?;
                let parsed = wf_lang::parse_wfs(&content)
                    .with_context(|| format!("parsing .wfs: {}", resolved.display()))?;
                schemas.extend(parsed);
            }
            "wfl" => {
                let source = load_wfl_with_context(&resolved, &wfl_ctx, Some(base_dir))
                    .with_context(|| format!("preprocessing .wfl: {}", resolved.display()))?;
                let parsed = wf_lang::parse_wfl(&source)
                    .with_context(|| format!("parsing .wfl: {}", resolved.display()))?;
                wfl_files.push(parsed);
            }
            other => {
                anyhow::bail!(
                    "unsupported file extension '{}' in use declaration: {}",
                    other,
                    use_decl.path
                );
            }
        }
    }

    Ok((schemas, wfl_files))
}
