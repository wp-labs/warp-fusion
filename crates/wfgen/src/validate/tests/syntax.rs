use super::*;
use crate::wfg_parser::parse_wfg;

#[test]
fn test_syntax_valid_minimal() {
    let input = r#"
use "schemas/security.wfs"
use "rules/brute_force.wfl"

#[duration=10m]
scenario brute_force_detect<seed=42> {
    traffic {
        stream auth_events gen 100/s
    }
    injection {
        hit<30%> auth_events {
            user seq {
                use(login="failed") with(3,2m)
            }
        }
        near_miss<10%> auth_events {
            user seq {
                use(login="failed") with(2,2m)
            }
        }
        miss<60%> auth_events {
            user seq {
                use(login="success") with(1,30s)
            }
        }
    }
    expect {
        hit(brute_force_then_scan) >= 95%
    }
}
"#;
    let wfg = parse_wfg(input).unwrap();
    let schemas = vec![make_schema(
        "auth_events",
        vec![
            ("sip", BaseType::Ip),
            ("login", BaseType::Chars),
            ("action", BaseType::Chars),
        ],
    )];
    let wfl = make_wfl("brute_force_then_scan", vec![("fail", "auth_events")]);
    let errors = validate_wfg(&wfg, &schemas, &[wfl]);
    assert!(
        !errors.iter().any(|e| e.code.starts_with("VN")),
        "unexpected VN errors: {:?}",
        errors
    );
}

#[test]
fn test_syntax_injection_percent_sum_exceeds_100() {
    let input = r#"
#[duration=10m]
scenario s<seed=1> {
    traffic { stream auth_events gen 100/s }
    injection {
        hit<70%> auth_events { user seq { use(login="failed") with(1,1m) } }
        near_miss<40%> auth_events { user seq { use(login="failed") with(1,1m) } }
    }
}
"#;
    let wfg = parse_wfg(input).unwrap();
    let schemas = vec![make_schema("auth_events", vec![])];
    let errors = validate_wfg(&wfg, &schemas, &[]);
    assert!(
        errors.iter().any(|e| e.code == "VN6"),
        "errors: {:?}",
        errors
    );
}

#[test]
fn test_syntax_expect_rule_missing() {
    let input = r#"
#[duration=10m]
scenario s<seed=1> {
    traffic { stream auth_events gen 100/s }
    expect {
        hit(rule_not_found) >= 95%
    }
}
"#;
    let wfg = parse_wfg(input).unwrap();
    let schemas = vec![make_schema("auth_events", vec![])];
    let errors = validate_wfg(&wfg, &schemas, &[]);
    assert!(
        errors.iter().any(|e| e.code == "VN7"),
        "errors: {:?}",
        errors
    );
}

#[test]
fn test_syntax_stream_missing_in_schema() {
    let input = r#"
#[duration=10m]
scenario s<seed=1> {
    traffic { stream missing_window gen 100/s }
}
"#;
    let wfg = parse_wfg(input).unwrap();
    let schemas = vec![make_schema("auth_events", vec![])];
    let errors = validate_wfg(&wfg, &schemas, &[]);
    assert!(
        errors.iter().any(|e| e.code == "VN3"),
        "errors: {:?}",
        errors
    );
}

#[test]
fn test_syntax_use_step_duplicate_field_rejected() {
    let input = r#"
#[duration=10m]
scenario s<seed=1> {
    traffic { stream auth_events gen 100/s }
    injection {
        hit<50%> auth_events {
            user seq {
                use(login="failed", login="success") with(1,1m)
            }
        }
    }
}
"#;
    let wfg = parse_wfg(input).unwrap();
    let schemas = vec![make_schema("auth_events", vec![])];
    let errors = validate_wfg(&wfg, &schemas, &[]);
    assert!(
        errors.iter().any(|e| e.code == "VN9"),
        "errors: {:?}",
        errors
    );
}
