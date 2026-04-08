use std::collections::HashSet;

use wf_lang::WindowSchema;
use wf_lang::ast::RuleDecl;

use super::ValidationError;
use crate::wfg_ast::{ExpectValue, WfgFile};

pub(super) fn validate_syntax(
    wfg: &WfgFile,
    schemas: &[WindowSchema],
    all_rules: &[&RuleDecl],
) -> Vec<ValidationError> {
    let mut errors = Vec::new();
    let Some(syntax) = &wfg.syntax else {
        return errors;
    };

    if syntax.traffic.streams.is_empty() {
        errors.push(ValidationError {
            code: "VN1",
            message: "traffic block must contain at least one stream".to_string(),
        });
    }

    for s in &syntax.traffic.streams {
        if s.rate.approx_eps() <= 0.0 {
            errors.push(ValidationError {
                code: "VN2",
                message: format!("stream '{}': rate must be greater than 0", s.stream),
            });
        }
        if !schemas.iter().any(|ws| ws.name == s.stream) {
            errors.push(ValidationError {
                code: "VN3",
                message: format!(
                    "stream '{}' not found in loaded schemas (.wfs windows)",
                    s.stream
                ),
            });
        }
    }

    if let Some(inj) = &syntax.injection {
        let mut sum = 0.0;
        for case in &inj.cases {
            if case.percent <= 0.0 || case.percent > 100.0 {
                errors.push(ValidationError {
                    code: "VN4",
                    message: format!(
                        "injection case '{}' percent {} must be in (0, 100]",
                        case.stream, case.percent
                    ),
                });
            }
            sum += case.percent;

            if case.seq.steps.is_empty() {
                errors.push(ValidationError {
                    code: "VN5",
                    message: format!(
                        "injection case '{}' must contain at least one seq step",
                        case.stream
                    ),
                });
            }

            for (step_idx, step) in case.seq.steps.iter().enumerate() {
                let crate::wfg_ast::SeqStep::Use { predicates, .. } = step else {
                    continue;
                };
                let mut seen = HashSet::new();
                for pred in predicates {
                    if !seen.insert(pred.field.as_str()) {
                        errors.push(ValidationError {
                            code: "VN9",
                            message: format!(
                                "injection case '{}' step {} has duplicate field '{}' in use(...)",
                                case.stream, step_idx, pred.field
                            ),
                        });
                    }
                }
            }
        }
        if sum > 100.0 {
            errors.push(ValidationError {
                code: "VN6",
                message: format!("injection percentages sum to {}, which exceeds 100%", sum),
            });
        }
    }

    if let Some(expect) = &syntax.expect {
        for check in &expect.checks {
            if !all_rules.iter().any(|r| r.name == check.rule) {
                errors.push(ValidationError {
                    code: "VN7",
                    message: format!("expect: rule '{}' not found in WFL files", check.rule),
                });
            }
            if let ExpectValue::Percent(p) = check.value
                && !(0.0..=100.0).contains(&p)
            {
                errors.push(ValidationError {
                    code: "VN8",
                    message: format!(
                        "expect percentage for rule '{}' must be in [0, 100], got {}",
                        check.rule, p
                    ),
                });
            }
        }
    }

    errors
}
