use super::*;

#[test]
fn test_same_seed_same_output() {
    let input = r#"
#[duration=10s]
scenario deterministic<seed=42> {
    traffic {
        stream LoginWindow gen 10/s
    }
}
"#;
    let wfg = parse_wfg(input).unwrap();
    let schemas = vec![make_login_schema()];

    let result1 = generate(&wfg, &schemas, &[]).unwrap();
    let result2 = generate(&wfg, &schemas, &[]).unwrap();

    assert_eq!(result1.events.len(), result2.events.len());
    for (e1, e2) in result1.events.iter().zip(result2.events.iter()) {
        assert_eq!(e1.timestamp, e2.timestamp);
        assert_eq!(e1.fields, e2.fields);
    }
}

#[test]
fn test_different_seed_different_output() {
    let input1 = r#"
#[duration=5s]
scenario seed_a<seed=42> {
    traffic {
        stream LoginWindow gen 10/s
    }
}
"#;
    let input2 = r#"
#[duration=5s]
scenario seed_b<seed=99> {
    traffic {
        stream LoginWindow gen 10/s
    }
}
"#;
    let wsc1 = parse_wfg(input1).unwrap();
    let wsc2 = parse_wfg(input2).unwrap();
    let schemas = vec![make_login_schema()];

    let result1 = generate(&wsc1, &schemas, &[]).unwrap();
    let result2 = generate(&wsc2, &schemas, &[]).unwrap();

    assert_eq!(result1.events.len(), result2.events.len());
    // At least some fields should differ
    let mut any_different = false;
    for (e1, e2) in result1.events.iter().zip(result2.events.iter()) {
        if e1.fields != e2.fields {
            any_different = true;
            break;
        }
    }
    assert!(
        any_different,
        "different seeds should produce different output"
    );
}

#[test]
fn test_correct_event_count() {
    let input = r#"
#[duration=20s]
scenario count_test<seed=1> {
    traffic {
        stream LoginWindow gen 10/s
    }
}
"#;
    let wfg = parse_wfg(input).unwrap();
    let schemas = vec![make_login_schema()];

    let result = generate(&wfg, &schemas, &[]).unwrap();
    assert_eq!(result.events.len(), 200);
}

#[test]
fn test_field_types_correct() {
    let input = r#"
#[duration=1s]
scenario types_test<seed=7> {
    traffic {
        stream LoginWindow gen 10/s
    }
}
"#;
    let wfg = parse_wfg(input).unwrap();
    let schemas = vec![make_login_schema()];

    let result = generate(&wfg, &schemas, &[]).unwrap();
    assert!(!result.events.is_empty());

    let event = &result.events[0];
    // Time field should be an epoch nanos number
    assert!(event.fields["timestamp"].is_number());
    // IP field should be a string
    assert!(event.fields["src_ip"].is_string());
    // Chars field should be a string
    assert!(event.fields["username"].is_string());
    // Bool field should be a boolean
    assert!(event.fields["success"].is_boolean());
    // Digit field should be a number
    assert!(event.fields["attempts"].is_number());
    // Float field should be a number
    assert!(event.fields["score"].is_number());
    // Hex field should be a string
    assert!(event.fields["request_id"].is_string());
}

#[test]
fn test_events_sorted_by_time() {
    let input = r#"
#[duration=10s]
scenario sorted_test<seed=42> {
    traffic {
        stream LoginWindow gen 10/s
    }
}
"#;
    let wfg = parse_wfg(input).unwrap();
    let schemas = vec![make_login_schema()];

    let result = generate(&wfg, &schemas, &[]).unwrap();
    for window in result.events.windows(2) {
        assert!(window[0].timestamp <= window[1].timestamp);
    }
}

#[test]
fn test_multiple_streams_distribution() {
    let schema2 = WindowSchema {
        name: "DnsWindow".to_string(),
        streams: vec!["dns_events".to_string()],
        time_field: Some("timestamp".to_string()),
        over: Duration::from_secs(300),
        fields: vec![
            FieldDef {
                name: "timestamp".to_string(),
                field_type: FieldType::Base(BaseType::Time),
            },
            FieldDef {
                name: "query".to_string(),
                field_type: FieldType::Base(BaseType::Chars),
            },
        ],
    };

    let input = r#"
#[duration=10s]
scenario multi_stream<seed=42> {
    traffic {
        stream LoginWindow gen 20/s
        stream DnsWindow gen 10/s
    }
}
"#;
    let wfg = parse_wfg(input).unwrap();
    let schemas = vec![make_login_schema(), schema2];

    let result = generate(&wfg, &schemas, &[]).unwrap();
    assert_eq!(result.events.len(), 300);

    let login_count = result
        .events
        .iter()
        .filter(|e| e.window_name == "LoginWindow")
        .count();
    let dns_count = result
        .events
        .iter()
        .filter(|e| e.window_name == "DnsWindow")
        .count();

    // Rate ratio is 2:1, so login should get ~200, dns ~100
    assert_eq!(login_count + dns_count, 300);
    assert!(
        login_count > dns_count,
        "LoginWindow should have more events"
    );
}

#[test]
fn test_enum_named_values_arg() {
    let input = r#"
#[duration=5s]
scenario enum_values<seed=7> {
    traffic {
        stream LoginWindow gen 10/s
    }
}
"#;
    let wfg = parse_wfg(input).unwrap();
    let schemas = vec![make_login_schema()];

    let result = generate(&wfg, &schemas, &[]).unwrap();
    assert!(!result.events.is_empty());
    for event in &result.events {
        let user = event.fields["username"].as_str().unwrap();
        assert!(!user.is_empty());
    }
}
