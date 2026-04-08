use wf_lang::ast::RuleDecl;

use super::ValidationError;
use crate::wfg_ast::ScenarioDecl;

/// SC2, SC2a: stream alias / rule events binding consistency.
pub(super) fn validate_stream_rule_bindings(
    scenario: &ScenarioDecl,
    all_rules: &[&RuleDecl],
) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    if all_rules.is_empty() {
        return errors;
    }

    for stream in &scenario.streams {
        let alias_decls: Vec<_> = all_rules
            .iter()
            .flat_map(|r| r.events.decls.iter())
            .filter(|e| e.alias == stream.alias)
            .collect();

        if alias_decls.is_empty() {
            errors.push(ValidationError {
                code: "SC2",
                message: format!(
                    "stream '{}': alias '{}' is not referenced by any rule events",
                    stream.alias, stream.alias
                ),
            });
            continue;
        }

        if !alias_decls.iter().any(|e| e.window == stream.window) {
            let windows = alias_decls
                .iter()
                .map(|e| e.window.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            errors.push(ValidationError {
                code: "SC2a",
                message: format!(
                    "stream '{}': alias '{}' maps to window '{}' but rules map it to: {}",
                    stream.alias, stream.alias, stream.window, windows
                ),
            });
        }
    }

    errors
}
