mod gen_compat;
mod inject;
mod oracle;
mod scenario;
mod stream_rule;
mod stream_schema;
mod syntax;

#[cfg(test)]
mod tests;

use wf_lang::WindowSchema;
use wf_lang::ast::WflFile;

use crate::wfg_ast::WfgFile;

/// A validation error found in a `.wfg` file.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidationError {
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

/// Validate a parsed `.wfg` file against schemas and WFL rules.
///
/// Returns a list of validation errors (empty if valid).
///
/// When `skip_wfl` is true, validations that require WFL rules to exist
/// (expect / inject referencing rules) are skipped, so a scenario can be
/// generated as baseline events without any rule files.
pub fn validate_wfg(
    wfg: &WfgFile,
    schemas: &[WindowSchema],
    wfl_files: &[WflFile],
    skip_wfl: bool,
) -> Vec<ValidationError> {
    let all_rules: Vec<_> = wfl_files.iter().flat_map(|f| f.rules.iter()).collect();

    if wfg.syntax.is_some() {
        return syntax::validate_syntax(wfg, schemas, &all_rules, skip_wfl);
    }

    let mut errors = Vec::new();
    let scenario = &wfg.scenario;

    errors.extend(scenario::validate_scenario_basics(scenario));
    errors.extend(stream_schema::validate_streams_with_schemas(
        scenario, schemas,
    ));

    errors.extend(stream_rule::validate_stream_rule_bindings(
        scenario, &all_rules,
    ));
    if !skip_wfl {
        errors.extend(inject::validate_inject_blocks(scenario, &all_rules));
    }
    errors.extend(oracle::validate_oracle_params(scenario));

    errors
}
