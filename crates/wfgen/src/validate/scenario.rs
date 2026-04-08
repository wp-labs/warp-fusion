use super::ValidationError;
use crate::wfg_ast::ScenarioDecl;

/// SV2-SV6: basic scenario value checks (total, rates, percents).
pub(super) fn validate_scenario_basics(scenario: &ScenarioDecl) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    // SV2: total > 0
    if scenario.total == 0 {
        errors.push(ValidationError {
            code: "SV2",
            message: "total must be greater than 0".to_string(),
        });
    }

    // SV3: rate.count > 0 for all streams
    for stream in &scenario.streams {
        if stream.rate.count == 0 {
            errors.push(ValidationError {
                code: "SV3",
                message: format!(
                    "stream '{}': rate count must be greater than 0",
                    stream.alias
                ),
            });
        }
    }

    // SV4: percent in (0, 100] for inject lines
    for inject in &scenario.injects {
        for line in &inject.lines {
            if line.percent <= 0.0 || line.percent > 100.0 {
                errors.push(ValidationError {
                    code: "SV4",
                    message: format!(
                        "inject for '{}': percent {} must be in (0, 100]",
                        inject.rule, line.percent
                    ),
                });
            }
        }
    }

    // SV4: percent in (0, 100] for fault lines
    if let Some(faults) = &scenario.faults {
        for fault in &faults.faults {
            if fault.percent <= 0.0 || fault.percent > 100.0 {
                errors.push(ValidationError {
                    code: "SV4",
                    message: format!(
                        "fault '{}': percent {} must be in (0, 100]",
                        fault.fault_type, fault.percent
                    ),
                });
            }
        }
    }

    // SV5: inject line percentages sum <= 100%
    for inject in &scenario.injects {
        let sum: f64 = inject.lines.iter().map(|l| l.percent).sum();
        if sum > 100.0 {
            errors.push(ValidationError {
                code: "SV5",
                message: format!(
                    "inject for '{}': percentages sum to {}, which exceeds 100%",
                    inject.rule, sum
                ),
            });
        }
    }

    // SV6: fault line percentages sum <= 100%
    if let Some(faults) = &scenario.faults {
        let sum: f64 = faults.faults.iter().map(|f| f.percent).sum();
        if sum > 100.0 {
            errors.push(ValidationError {
                code: "SV6",
                message: format!("faults: percentages sum to {}, which exceeds 100%", sum),
            });
        }
    }

    errors
}
