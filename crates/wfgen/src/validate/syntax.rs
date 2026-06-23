use std::collections::{HashMap, HashSet};

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
        let traffic_streams: HashSet<&str> = syntax
            .traffic
            .streams
            .iter()
            .map(|stream| stream.stream.as_str())
            .collect();
        let schemas_by_name: HashMap<&str, &WindowSchema> = schemas
            .iter()
            .map(|schema| (schema.name.as_str(), schema))
            .collect();
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

            let case_schema = schemas_by_name.get(case.stream.as_str()).copied();
            if !traffic_streams.contains(case.stream.as_str()) {
                errors.push(ValidationError {
                    code: "VN10",
                    message: format!(
                        "injection case stream '{}' is not declared in traffic",
                        case.stream
                    ),
                });
            }

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
                let (step_kind, predicates, count) = match step {
                    crate::wfg_ast::SeqStep::Use {
                        predicates, count, ..
                    } => ("use(...)", predicates, Some(*count)),
                    crate::wfg_ast::SeqStep::Not { predicates, .. } => {
                        errors.push(ValidationError {
                            code: "VN16",
                            message: format!(
                                "injection case '{}' step {} not(...) is not supported by datagen yet",
                                case.stream, step_idx
                            ),
                        });
                        ("not(...)", predicates, None)
                    }
                };
                if count == Some(0) {
                    errors.push(ValidationError {
                        code: "VN15",
                        message: format!(
                            "injection case '{}' step {} use(...) count must be greater than 0",
                            case.stream, step_idx
                        ),
                    });
                }
                let mut seen = HashSet::new();
                for pred in predicates {
                    if !seen.insert(pred.field.as_str()) {
                        errors.push(ValidationError {
                            code: "VN9",
                            message: format!(
                                "injection case '{}' step {} has duplicate field '{}' in {}",
                                case.stream, step_idx, pred.field, step_kind
                            ),
                        });
                    }
                    if pred.field == case.seq.entity {
                        errors.push(ValidationError {
                            code: "VN12",
                            message: format!(
                                "injection case '{}' step {} repeats seq entity field '{}' in {}",
                                case.stream, step_idx, pred.field, step_kind
                            ),
                        });
                    }
                    if let Some(schema) = case_schema
                        && !schema.fields.iter().any(|field| field.name == pred.field)
                    {
                        errors.push(ValidationError {
                            code: "VN11",
                            message: format!(
                                "injection case '{}' step {} field '{}' not found in schema '{}'",
                                case.stream, step_idx, pred.field, schema.name
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

    let expected_rules: HashSet<&str> = syntax
        .expect
        .as_ref()
        .map(|expect| {
            expect
                .checks
                .iter()
                .map(|check| check.rule.as_str())
                .collect()
        })
        .unwrap_or_default();

    if let Some(inj) = &syntax.injection {
        for case in &inj.cases {
            if let Some(target_rule) = case.target_rule.as_deref() {
                if !all_rules.iter().any(|rule| rule.name == target_rule) {
                    errors.push(ValidationError {
                        code: "VN14",
                        message: format!(
                            "injection case '{}' targets rule '{}' not found in WFL files",
                            case.stream, target_rule
                        ),
                    });
                }
            } else if expected_rules.len() != 1 {
                errors.push(ValidationError {
                    code: "VN13",
                    message: format!(
                        "injection case '{}' must use 'for RULE' because expect identifies {} target rules",
                        case.stream,
                        expected_rules.len()
                    ),
                });
            }
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
