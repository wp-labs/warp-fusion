//! Project rule prelude (`_global.wfl`) support.
//!
//! Mirrors `wf-runtime`'s prelude convention: a `_global.wfl` file living in
//! the same directory as a rule file may only declare `yield preset` blocks,
//! and those presets are merged into every rule file in that directory before
//! WFL compilation. Without this, a rule referencing a prelude preset fails
//! with "unknown yield preset".
//!
//! wfgen discovers rule files through a `.wfg` scenario's `use` declarations
//! (there is no rules glob), so the prelude is resolved per rule file: a
//! `<rule-dir>/_global.wfl` next to the `.wfl` file being loaded.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use wf_config::load_wfl_with_context;
use wf_config::ConfigVarContext;

use wf_lang::ast::{WflFile, YieldPresetDecl};

use crate::error::{self, WfgenReason, WfgenResult, WfgenStructExt};

const RULE_PRELUDE_FILE: &str = "_global.wfl";

/// A parsed `_global.wfl` rule prelude.
#[derive(Debug, Clone)]
pub struct RulePrelude {
    pub path: PathBuf,
    pub file: WflFile,
}

/// Resolve the prelude path for a rule file: `_global.wfl` in the same directory.
pub fn prelude_path_for(wfl_path: &Path) -> Option<PathBuf> {
    let prelude = wfl_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(RULE_PRELUDE_FILE);
    prelude.exists().then_some(prelude)
}

/// True when `wfl_path` is itself the prelude file (e.g. a scenario explicitly
/// `use`s `_global.wfl`). Mirrors wf-runtime's `same_path` exclusion so the
/// prelude is never treated as one of its own rule files.
pub fn is_prelude_file(wfl_path: &Path, prelude_path: &Path) -> bool {
    wfl_path == prelude_path
        || wfl_path
            .canonicalize()
            .ok()
            .zip(prelude_path.canonicalize().ok())
            .is_some_and(|(left, right)| left == right)
}

/// Load, preprocess, and parse a `_global.wfl` prelude, validating that it is
/// prelude-only (only `yield preset` declarations allowed).
pub fn load_rule_prelude(
    prelude_path: &Path,
    ctx: &ConfigVarContext,
    base_dir: &Path,
) -> WfgenResult<RulePrelude> {
    let source = load_wfl_with_context(prelude_path, ctx, Some(base_dir)).wfgen()?;
    parse_rule_prelude(&source, prelude_path)
}

/// Parse and validate a prelude from an already-loaded source string.
pub fn parse_rule_prelude(source: &str, path: &Path) -> WfgenResult<RulePrelude> {
    let file = wf_lang::parse_wfl(source).wfgen()?;
    validate_prelude_only(&file, path)?;
    Ok(RulePrelude {
        path: path.to_path_buf(),
        file,
    })
}

/// `_global.wfl` may only declare `yield preset` blocks.
fn validate_prelude_only(file: &WflFile, path: &Path) -> WfgenResult<()> {
    let invalid = if !file.uses.is_empty() {
        Some("use declarations")
    } else if !file.patterns.is_empty() {
        Some("pattern declarations")
    } else if !file.rules.is_empty() {
        Some("rule declarations")
    } else if !file.tests.is_empty() {
        Some("test blocks")
    } else {
        None
    };
    if let Some(kind) = invalid {
        return error::fail(
            WfgenReason::Validation,
            format!(
                "{} is a rule prelude and only allows `yield preset` declarations; found {}",
                path.display(),
                kind
            ),
        );
    }
    validate_unique_yield_presets(&file.yield_presets, path, "rule prelude")?;
    Ok(())
}

/// Reject a rule file that redefines a preset already provided by the prelude.
pub fn validate_rule_prelude_conflicts(
    file: &WflFile,
    path: &Path,
    prelude: &RulePrelude,
) -> WfgenResult<()> {
    for preset in &file.yield_presets {
        if prelude
            .file
            .yield_presets
            .iter()
            .any(|prelude_preset| prelude_preset.name == preset.name)
        {
            return error::fail(
                WfgenReason::Validation,
                format!(
                    "{} defines yield preset `{}` that already exists in prelude {}",
                    path.display(),
                    preset.name,
                    prelude.path.display()
                ),
            );
        }
    }
    Ok(())
}

/// Merge the prelude's presets into a rule file: prelude presets first, then
/// the rule's own presets (which take precedence on name conflict).
pub fn apply_rule_prelude(file: &mut WflFile, prelude: &RulePrelude) {
    let mut yield_presets = prelude.file.yield_presets.clone();
    yield_presets.extend(file.yield_presets.clone());
    file.yield_presets = yield_presets;
}

fn validate_unique_yield_presets(
    presets: &[YieldPresetDecl],
    path: &Path,
    scope: &str,
) -> WfgenResult<()> {
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for preset in presets {
        let count = seen.entry(preset.name.as_str()).or_insert(0);
        *count += 1;
        if *count > 1 {
            return error::fail(
                WfgenReason::Validation,
                format!(
                    "{} duplicate yield preset `{}` in {}",
                    path.display(),
                    preset.name,
                    scope
                ),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_wfl(source: &str) -> WflFile {
        wf_lang::parse_wfl(source).expect("test source should parse")
    }

    const PRELUDE_SRC: &str = "yield preset base_alerts (\n    workflow_status = \"NEW\",\n)";
    const RULE_SRC: &str = "yield preset local_alert (\n    kind = \"local\",\n)";

    #[test]
    fn prelude_path_for_finds_sibling_global_wfl() {
        let dir = std::env::temp_dir().join(format!("wfgen-prelude-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("_global.wfl"), PRELUDE_SRC).unwrap();
        let rule_path = dir.join("rule.wfl");
        assert_eq!(prelude_path_for(&rule_path), Some(dir.join("_global.wfl")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn is_prelude_file_detects_self() {
        let dir = std::env::temp_dir().join(format!("wfgen-prelude-self-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let prelude = dir.join("_global.wfl");
        std::fs::write(&prelude, PRELUDE_SRC).unwrap();
        let rule = dir.join("rule.wfl");
        std::fs::write(&rule, RULE_SRC).unwrap();
        assert!(is_prelude_file(&prelude, &prelude));
        assert!(!is_prelude_file(&rule, &prelude));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn apply_rule_prelude_merges_presets_prelude_first() {
        let prelude = parse_rule_prelude(PRELUDE_SRC, Path::new("_global.wfl")).unwrap();
        let mut rule = parse_wfl(RULE_SRC);
        apply_rule_prelude(&mut rule, &prelude);
        let names: Vec<&str> = rule.yield_presets.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["base_alerts", "local_alert"]);
    }

    #[test]
    fn prelude_only_rejects_rule_declarations() {
        let err = parse_rule_prelude(
            "yield preset p (\n    a = 1,\n)\nrule r {\n    events { x : w }\n    on each x -> score(1.0)\n    entity(h, x.f)\n    yield out (a = x.a)\n}\n",
            Path::new("_global.wfl"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("only allows `yield preset`"));
        assert!(err.to_string().contains("rule declarations"));
    }

    #[test]
    fn prelude_only_rejects_duplicate_presets() {
        let err = parse_rule_prelude(
            "yield preset p (\n    a = 1,\n)\nyield preset p (\n    a = 2,\n)\n",
            Path::new("_global.wfl"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("duplicate yield preset `p`"));
    }

    #[test]
    fn rule_redefining_prelude_preset_is_conflict() {
        let prelude = parse_rule_prelude(PRELUDE_SRC, Path::new("_global.wfl")).unwrap();
        let rule = parse_wfl(&format!("{PRELUDE_SRC}\n{RULE_SRC}"));
        let err = validate_rule_prelude_conflicts(&rule, Path::new("rule.wfl"), &prelude).unwrap_err();
        assert!(err.to_string().contains("already exists in prelude"));
    }
}
