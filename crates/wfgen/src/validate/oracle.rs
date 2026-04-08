use super::ValidationError;
use crate::wfg_ast::{ParamValue, ScenarioDecl};

/// SV8: oracle param type/range validation.
pub(super) fn validate_oracle_params(scenario: &ScenarioDecl) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    let Some(oracle) = &scenario.oracle else {
        return errors;
    };

    for param in &oracle.params {
        match param.name.as_str() {
            "time_tolerance" => {
                if !matches!(&param.value, ParamValue::Duration(_)) {
                    errors.push(ValidationError {
                        code: "SV8",
                        message: "oracle.time_tolerance must be a duration (e.g. 1s, 500ms)"
                            .to_string(),
                    });
                }
            }
            "score_tolerance" => match &param.value {
                ParamValue::Number(n) if *n >= 0.0 => {}
                ParamValue::Number(n) => {
                    errors.push(ValidationError {
                        code: "SV8",
                        message: format!("oracle.score_tolerance must be >= 0, got {}", n),
                    });
                }
                _ => {
                    errors.push(ValidationError {
                        code: "SV8",
                        message: "oracle.score_tolerance must be a number".to_string(),
                    });
                }
            },
            _ => {}
        }
    }

    errors
}
