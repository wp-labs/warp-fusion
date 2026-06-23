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
                use(login="failed") with(3)
            }
        }
        near_miss<10%> auth_events {
            user seq {
                use(login="failed") with(2)
            }
        }
        miss<60%> auth_events {
            user seq {
                use(login="success") with(1)
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
        hit<70%> auth_events { user seq { use(login="failed") with(1) } }
        near_miss<40%> auth_events { user seq { use(login="failed") with(1) } }
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
                use(login="failed", login="success") with(1)
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

#[test]
fn test_syntax_use_step_count_must_be_positive() {
    let input = r#"
#[duration=10m]
scenario s<seed=1> {
    traffic { stream auth_events gen 100/s }
    injection {
        hit<50%> auth_events {
            user seq {
                use(login="failed") with(0)
            }
        }
    }
}
"#;
    let wfg = parse_wfg(input).unwrap();
    let schemas = vec![make_schema(
        "auth_events",
        vec![("user", BaseType::Chars), ("login", BaseType::Chars)],
    )];
    let errors = validate_wfg(&wfg, &schemas, &[]);
    assert!(
        errors.iter().any(|e| e.code == "VN15"),
        "errors: {:?}",
        errors
    );
}

#[test]
fn test_syntax_not_step_is_rejected_until_datagen_supports_it() {
    let input = r#"
#[duration=10m]
scenario s<seed=1> {
    traffic { stream auth_events gen 100/s }
    injection {
        near_miss<50%> auth_events {
            user seq {
                use(login="failed") with(1)
                not(action="scan") within(1m)
            }
        }
    }
}
"#;
    let wfg = parse_wfg(input).unwrap();
    let schemas = vec![make_schema(
        "auth_events",
        vec![
            ("user", BaseType::Chars),
            ("login", BaseType::Chars),
            ("action", BaseType::Chars),
        ],
    )];
    let errors = validate_wfg(&wfg, &schemas, &[]);
    assert!(
        errors.iter().any(|e| e.code == "VN16"),
        "errors: {:?}",
        errors
    );
}

#[test]
fn test_syntax_not_step_fields_are_still_validated() {
    let input = r#"
#[duration=10m]
scenario s<seed=1> {
    traffic { stream auth_events gen 100/s }
    injection {
        near_miss<50%> auth_events {
            user seq {
                use(login="failed") with(1)
                not(missing_action="scan") within(1m)
            }
        }
    }
}
"#;
    let wfg = parse_wfg(input).unwrap();
    let schemas = vec![make_schema(
        "auth_events",
        vec![("user", BaseType::Chars), ("login", BaseType::Chars)],
    )];
    let errors = validate_wfg(&wfg, &schemas, &[]);
    assert!(
        errors.iter().any(|e| e.code == "VN16"),
        "errors: {:?}",
        errors
    );
    assert!(
        errors.iter().any(|e| e.code == "VN11"),
        "errors: {:?}",
        errors
    );
}

#[test]
fn test_syntax_not_step_duplicate_field_message_names_not_step() {
    let input = r#"
#[duration=10m]
scenario s<seed=1> {
    traffic { stream auth_events gen 100/s }
    injection {
        near_miss<50%> auth_events {
            user seq {
                use(login="failed") with(1)
                not(action="scan", action="probe") within(1m)
            }
        }
    }
}
"#;
    let wfg = parse_wfg(input).unwrap();
    let schemas = vec![make_schema(
        "auth_events",
        vec![
            ("user", BaseType::Chars),
            ("login", BaseType::Chars),
            ("action", BaseType::Chars),
        ],
    )];
    let errors = validate_wfg(&wfg, &schemas, &[]);
    assert!(
        errors
            .iter()
            .any(|e| e.code == "VN9" && e.message.contains("not(...)")),
        "errors: {:?}",
        errors
    );
}

#[test]
fn test_syntax_not_step_entity_field_message_names_not_step() {
    let input = r#"
#[duration=10m]
scenario s<seed=1> {
    traffic { stream auth_events gen 100/s }
    injection {
        near_miss<50%> auth_events {
            user seq {
                use(login="failed") with(1)
                not(user="alice") within(1m)
            }
        }
    }
}
"#;
    let wfg = parse_wfg(input).unwrap();
    let schemas = vec![make_schema(
        "auth_events",
        vec![("user", BaseType::Chars), ("login", BaseType::Chars)],
    )];
    let errors = validate_wfg(&wfg, &schemas, &[]);
    assert!(
        errors
            .iter()
            .any(|e| e.code == "VN12" && e.message.contains("not(...)")),
        "errors: {:?}",
        errors
    );
}

#[test]
fn test_syntax_injection_stream_must_be_declared_in_traffic() {
    let input = r#"
#[duration=10m]
scenario s<seed=1> {
    traffic { stream auth_events gen 100/s }
    injection {
        hit<50%> typo_events {
            user seq {
                use(login="failed") with(1)
            }
        }
    }
}
"#;
    let wfg = parse_wfg(input).unwrap();
    let schemas = vec![
        make_schema(
            "auth_events",
            vec![("user", BaseType::Chars), ("login", BaseType::Chars)],
        ),
        make_schema(
            "typo_events",
            vec![("user", BaseType::Chars), ("login", BaseType::Chars)],
        ),
    ];
    let errors = validate_wfg(&wfg, &schemas, &[]);
    assert!(
        errors.iter().any(|e| e.code == "VN10"),
        "errors: {:?}",
        errors
    );
}

#[test]
fn test_syntax_injection_fields_must_exist_in_schema() {
    let input = r#"
#[duration=10m]
scenario s<seed=1> {
    traffic { stream auth_events gen 100/s }
    injection {
        hit<50%> auth_events {
            missing_user seq {
                use(missing_login="failed") with(1)
            }
        }
    }
}
"#;
    let wfg = parse_wfg(input).unwrap();
    let schemas = vec![make_schema("auth_events", vec![("login", BaseType::Chars)])];
    let errors = validate_wfg(&wfg, &schemas, &[]);
    assert!(
        errors.iter().any(|e| e.code == "VN11"),
        "errors: {:?}",
        errors
    );
}

#[test]
fn test_syntax_entity_field_must_not_be_redeclared_in_use() {
    let input = r#"
#[duration=10m]
scenario s<seed=1> {
    traffic { stream auth_events gen 100/s }
    injection {
        hit<50%> auth_events {
            user seq {
                use(user="alice", login="failed") with(1)
            }
        }
    }
}
"#;
    let wfg = parse_wfg(input).unwrap();
    let schemas = vec![make_schema(
        "auth_events",
        vec![("user", BaseType::Chars), ("login", BaseType::Chars)],
    )];
    let errors = validate_wfg(&wfg, &schemas, &[]);
    assert!(
        errors.iter().any(|e| e.code == "VN12"),
        "errors: {:?}",
        errors
    );
}

#[test]
fn test_syntax_injection_multi_rule_expect_requires_target_rule() {
    let input = r#"
#[duration=10m]
scenario s<seed=1> {
    traffic { stream auth_events gen 100/s }
    injection {
        hit<50%> auth_events {
            user seq {
                use(login="failed") with(1)
            }
        }
    }
    expect {
        hit(rule_a) >= 90%
        hit(rule_b) >= 90%
    }
}
"#;
    let wfg = parse_wfg(input).unwrap();
    let schemas = vec![make_schema(
        "auth_events",
        vec![("login", BaseType::Chars), ("sip", BaseType::Ip)],
    )];
    let wfl_a = make_wfl("rule_a", vec![("a", "auth_events")]);
    let wfl_b = make_wfl("rule_b", vec![("b", "auth_events")]);
    let errors = validate_wfg(&wfg, &schemas, &[wfl_a, wfl_b]);
    assert!(
        errors.iter().any(|e| e.code == "VN13"),
        "errors: {:?}",
        errors
    );
}

#[test]
fn test_syntax_injection_without_expect_requires_target_rule() {
    let input = r#"
#[duration=10m]
scenario s<seed=1> {
    traffic { stream auth_events gen 100/s }
    injection {
        hit<50%> auth_events {
            user seq {
                use(login="failed") with(1)
            }
        }
    }
}
"#;
    let wfg = parse_wfg(input).unwrap();
    let schemas = vec![make_schema(
        "auth_events",
        vec![("login", BaseType::Chars), ("sip", BaseType::Ip)],
    )];
    let errors = validate_wfg(&wfg, &schemas, &[]);
    assert!(
        errors.iter().any(|e| e.code == "VN13"),
        "errors: {:?}",
        errors
    );
}

#[test]
fn test_syntax_injection_target_rule_allows_multi_rule_expect() {
    let input = r#"
#[duration=10m]
scenario s<seed=1> {
    traffic { stream auth_events gen 100/s }
    injection {
        hit<50%> for rule_a auth_events {
            user seq {
                use(login="failed") with(1)
            }
        }
        hit<20%> for rule_b auth_events {
            user seq {
                use(login="failed") with(1)
            }
        }
    }
    expect {
        hit(rule_a) >= 90%
        hit(rule_b) >= 90%
    }
}
"#;
    let wfg = parse_wfg(input).unwrap();
    let schemas = vec![make_schema(
        "auth_events",
        vec![("login", BaseType::Chars), ("sip", BaseType::Ip)],
    )];
    let wfl_a = make_wfl("rule_a", vec![("a", "auth_events")]);
    let wfl_b = make_wfl("rule_b", vec![("b", "auth_events")]);
    let errors = validate_wfg(&wfg, &schemas, &[wfl_a, wfl_b]);
    assert!(
        !errors.iter().any(|e| e.code == "VN13" || e.code == "VN14"),
        "errors: {:?}",
        errors
    );
}

#[test]
fn test_syntax_injection_target_rule_must_exist() {
    let input = r#"
#[duration=10m]
scenario s<seed=1> {
    traffic { stream auth_events gen 100/s }
    injection {
        hit<50%> for missing_rule auth_events {
            user seq {
                use(login="failed") with(1)
            }
        }
    }
}
"#;
    let wfg = parse_wfg(input).unwrap();
    let schemas = vec![make_schema(
        "auth_events",
        vec![("login", BaseType::Chars), ("sip", BaseType::Ip)],
    )];
    let errors = validate_wfg(&wfg, &schemas, &[]);
    assert!(
        errors.iter().any(|e| e.code == "VN14"),
        "errors: {:?}",
        errors
    );
}
