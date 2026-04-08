use wf_lang::ast::RuleDecl;

use super::ValidationError;
use crate::wfg_ast::ScenarioDecl;

/// SC5, SC6: inject block cross-checks (rule exists, stream aliases valid).
pub(super) fn validate_inject_blocks(
    scenario: &ScenarioDecl,
    all_rules: &[&RuleDecl],
) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    // SC5: inject.rule must exist in WFL files
    for inject in &scenario.injects {
        if !all_rules.iter().any(|r| r.name == inject.rule) {
            errors.push(ValidationError {
                code: "SC5",
                message: format!("inject: rule '{}' not found in WFL files", inject.rule),
            });
        }
    }

    // SC6: inject.streams must be aliases of streams defined in the scenario
    // and must be declared in the target rule's events (alias + window).
    for inject in &scenario.injects {
        let target_rule = all_rules.iter().find(|r| r.name == inject.rule);

        for stream_name in &inject.streams {
            let scenario_stream = scenario.streams.iter().find(|s| s.alias == *stream_name);
            match scenario_stream {
                None => {
                    errors.push(ValidationError {
                        code: "SC6",
                        message: format!(
                            "inject for '{}': stream alias '{}' not found in scenario streams",
                            inject.rule, stream_name
                        ),
                    });
                }
                Some(stream) => {
                    if let Some(rule) = target_rule {
                        let alias_events: Vec<_> = rule
                            .events
                            .decls
                            .iter()
                            .filter(|e| e.alias == *stream_name)
                            .collect();

                        if alias_events.is_empty() {
                            errors.push(ValidationError {
                                code: "SC6",
                                message: format!(
                                    "inject for '{}': stream alias '{}' is not declared in rule '{}' events",
                                    inject.rule,
                                    stream_name,
                                    inject.rule
                                ),
                            });
                            continue;
                        }

                        if !alias_events.iter().any(|e| e.window == stream.window) {
                            let windows = alias_events
                                .iter()
                                .map(|e| e.window.as_str())
                                .collect::<Vec<_>>()
                                .join(", ");
                            errors.push(ValidationError {
                                code: "SC6",
                                message: format!(
                                    "inject for '{}': stream '{}' uses window '{}' but rule '{}' maps alias '{}' to: {}",
                                    inject.rule,
                                    stream_name,
                                    stream.window,
                                    inject.rule,
                                    stream_name,
                                    windows
                                ),
                            });
                        }
                    }
                }
            }
        }
    }

    errors
}
