use std::io::BufReader;
use std::time::Duration;

use wf_lang::{BaseType, FieldDef, FieldType, WindowSchema};
use wfl::cmd_replay::{replay_events, replay_events_for_verify};

fn make_auth_events_schema() -> WindowSchema {
    WindowSchema {
        name: "auth_events".to_string(),
        streams: vec!["auth_stream".to_string()],
        time_field: Some("event_time".to_string()),
        over: Duration::from_secs(3600),
        fields: vec![
            FieldDef {
                name: "sip".to_string(),
                field_type: FieldType::Base(BaseType::Ip),
            },
            FieldDef {
                name: "action".to_string(),
                field_type: FieldType::Base(BaseType::Chars),
            },
            FieldDef {
                name: "user".to_string(),
                field_type: FieldType::Base(BaseType::Chars),
            },
            FieldDef {
                name: "event_time".to_string(),
                field_type: FieldType::Base(BaseType::Time),
            },
        ],
    }
}

fn make_security_alerts_schema() -> WindowSchema {
    WindowSchema {
        name: "security_alerts".to_string(),
        streams: vec![],
        time_field: None,
        over: Duration::from_secs(3600),
        fields: vec![
            FieldDef {
                name: "sip".to_string(),
                field_type: FieldType::Base(BaseType::Ip),
            },
            FieldDef {
                name: "fail_count".to_string(),
                field_type: FieldType::Base(BaseType::Digit),
            },
        ],
    }
}

const WFL_RULE: &str = r#"
rule brute_force {
    events { e : auth_events }
    match<sip:5m> {
        on event { e | count >= 5; }
    } -> score(70.0)
    entity(ip, e.sip)
    yield security_alerts (sip = e.sip, fail_count = 5)
}
"#;

fn make_ndjson_events(count: usize) -> String {
    let mut lines = Vec::with_capacity(count);
    for i in 0..count {
        lines.push(format!(
            r#"{{"_stream":"auth_stream","sip":"10.0.0.1","action":"failed","user":"admin","event_time":{}}}"#,
            1_700_000_000_000_000_000i64 + (i as i64) * 1_000_000_000
        ));
    }
    lines.join("\n")
}

#[test]
fn replay_five_events_one_match() {
    let schemas = vec![make_auth_events_schema(), make_security_alerts_schema()];
    let ndjson = make_ndjson_events(5);
    let reader = BufReader::new(ndjson.as_bytes());

    let result = replay_events(WFL_RULE, &schemas, reader, false).expect("replay should succeed");
    assert_eq!(result.event_count, 5);
    assert_eq!(result.match_count, 1);
    assert_eq!(result.error_count, 0);
    assert_eq!(result.alerts.len(), 1);

    let alert = &result.alerts[0];
    assert_eq!(alert.rule_name, "brute_force");
    assert!((alert.score - 70.0).abs() < f64::EPSILON);
    assert_eq!(alert.entity_type, "ip");
    assert_eq!(alert.entity_id, "10.0.0.1");
}

#[test]
fn replay_below_threshold_no_match() {
    let schemas = vec![make_auth_events_schema(), make_security_alerts_schema()];
    let ndjson = make_ndjson_events(3);
    let reader = BufReader::new(ndjson.as_bytes());

    let result = replay_events(WFL_RULE, &schemas, reader, false).expect("replay should succeed");
    assert_eq!(result.event_count, 3);
    assert_eq!(result.match_count, 0);
    assert_eq!(result.error_count, 0);
    assert!(result.alerts.is_empty());
}

// ===========================================================================
// EOF close_all(Eos) with on_close steps
// ===========================================================================

/// Rule with on_close: the event step is satisfied, then EOF triggers close_all(Eos)
/// which evaluates on_close steps and produces an alert.
const WFL_CLOSE_RULE: &str = r#"
rule eos_close {
    events { e : auth_events }
    match<sip:5m> {
        on event { e | count >= 1; }
        and close { close_count: e | count >= 1; }
    } -> score(80.0)
    entity(ip, e.sip)
    yield security_alerts (sip = e.sip, fail_count = 1)
}
"#;

#[test]
fn replay_eof_close_all_fires_alert() {
    let schemas = vec![make_auth_events_schema(), make_security_alerts_schema()];
    // Send 2 events: enough to satisfy on_event (count >= 1) and on_close (count >= 1).
    // No on-event match is produced (close steps present → deferred to close path).
    // EOF close_all(Eos) evaluates close steps and emits the alert.
    let ndjson = make_ndjson_events(2);
    let reader = BufReader::new(ndjson.as_bytes());

    let result =
        replay_events(WFL_CLOSE_RULE, &schemas, reader, false).expect("replay should succeed");

    assert_eq!(result.event_count, 2);
    assert_eq!(result.match_count, 1, "expected one alert from EOF close");
    assert_eq!(result.error_count, 0);
    assert_eq!(result.alerts.len(), 1);

    let alert = &result.alerts[0];
    assert_eq!(alert.rule_name, "eos_close");
    assert!((alert.score - 80.0).abs() < f64::EPSILON);
    assert_eq!(alert.entity_type, "ip");
    assert_eq!(alert.entity_id, "10.0.0.1");
}

// ===========================================================================
// Multi-source rule: time_field resolves from the alias-specific schema
// ===========================================================================

fn make_b_win_schema() -> WindowSchema {
    WindowSchema {
        name: "b_win".to_string(),
        streams: vec!["b_stream".to_string()],
        time_field: Some("tb".to_string()),
        over: Duration::from_secs(3600),
        fields: vec![
            FieldDef {
                name: "sip".to_string(),
                field_type: FieldType::Base(BaseType::Ip),
            },
            FieldDef {
                name: "tb".to_string(),
                field_type: FieldType::Base(BaseType::Time),
            },
        ],
    }
}

/// In a multi-source rule, events are routed by _stream. The engine should
/// use the time_field from the appropriate schema. This test verifies fired_at
/// is in the expected range, not 1970.
#[test]
fn replay_multi_source_time_field() {
    let schemas = vec![
        make_auth_events_schema(),
        make_b_win_schema(),
        make_security_alerts_schema(),
    ];

    // Rule binds two sources; b_stream events go to b_win.
    // b_win's time_field is "tb".
    let wfl = r#"
rule multi_src {
    events {
        a : auth_events
        b : b_win
    }
    match<sip:5m> {
        on event { b | count >= 2; }
    } -> score(60.0)
    entity(ip, b.sip)
    yield security_alerts (sip = b.sip, fail_count = 2)
}
"#;

    let base_nanos = 1_700_000_000_000_000_000i64;
    // Events with _stream="b_stream" routed to b_win
    let ndjson = format!(
        r#"{{"_stream":"b_stream","sip":"10.0.0.1","tb":{}}}"#,
        base_nanos
    ) + "\n"
        + &format!(
            r#"{{"_stream":"b_stream","sip":"10.0.0.1","tb":{}}}"#,
            base_nanos + 1_000_000_000
        );
    let reader = BufReader::new(ndjson.as_bytes());

    let result = replay_events(wfl, &schemas, reader, false).expect("replay should succeed");

    assert_eq!(result.event_count, 2);
    assert_eq!(result.match_count, 1);
    assert_eq!(result.alerts.len(), 1);

    let alert = &result.alerts[0];
    assert_eq!(alert.rule_name, "multi_src");
    // fired_at must be derived from the event time (tb), not default to 0 (1970).
    // The nanosecond timestamp 1_700_000_000_000_000_000 is ~2023-11-14.
    // Convert fired_at (ISO string) year to verify it's not 1970.
    assert!(
        !alert.fired_at.starts_with("1970"),
        "fired_at should not be 1970 (got {}); time_field was not resolved from schema",
        alert.fired_at
    );
}

// ===========================================================================
// Conv + mixed qualifying/non-qualifying: cross-layer e2e
// ===========================================================================

fn make_conn_events_schema() -> WindowSchema {
    WindowSchema {
        name: "conn_events".to_string(),
        streams: vec!["netflow".to_string()],
        time_field: Some("event_time".to_string()),
        over: Duration::from_secs(1800),
        fields: vec![
            FieldDef {
                name: "sip".to_string(),
                field_type: FieldType::Base(BaseType::Ip),
            },
            FieldDef {
                name: "dport".to_string(),
                field_type: FieldType::Base(BaseType::Digit),
            },
            FieldDef {
                name: "action".to_string(),
                field_type: FieldType::Base(BaseType::Chars),
            },
            FieldDef {
                name: "event_time".to_string(),
                field_type: FieldType::Base(BaseType::Time),
            },
        ],
    }
}

fn make_network_alerts_schema() -> WindowSchema {
    WindowSchema {
        name: "network_alerts".to_string(),
        streams: vec![],
        time_field: None,
        over: Duration::ZERO,
        fields: vec![
            FieldDef {
                name: "sip".to_string(),
                field_type: FieldType::Base(BaseType::Ip),
            },
            FieldDef {
                name: "alert_type".to_string(),
                field_type: FieldType::Base(BaseType::Chars),
            },
        ],
    }
}

/// Conv with mixed qualifying/non-qualifying outputs in the replay path.
///
/// 4 IPs feed into a fixed-window rule with `on close { scan >= 3 }` and
/// `conv { sort(-scan) | top(2) }`. Three IPs qualify (scan ≥ 3), one does
/// not (scan = 2). Conv must operate only on qualifying outputs, keeping
/// the top 2 by scan count. The non-qualifying IP must not steal a top(2)
/// slot or produce a spurious alert.
#[test]
fn replay_conv_top_with_mixed_qualifying() {
    let schemas = vec![make_conn_events_schema(), make_network_alerts_schema()];

    let wfl = r#"
rule conv_mixed {
    events { c : conn_events && action == "syn" }
    match<sip:1h:fixed> {
        on event { c | count >= 1; }
        and close { scan: c.dport | distinct | count >= 3; }
    } -> score(80.0)
    entity(ip, c.sip)
    yield network_alerts (sip = c.sip, alert_type = "scan")
    conv { sort(-scan) | top(2) ; }
}
"#;

    let base = 1_700_000_000_000_000_000i64;
    let sec = 1_000_000_000i64;
    let mut lines = Vec::new();
    let mut t = 0i64;

    // IP-A: 5 distinct ports → qualifying (scan=5)
    for port in [80, 443, 8080, 22, 3306] {
        t += 1;
        lines.push(format!(
            r#"{{"_stream":"netflow","sip":"10.0.0.1","dport":{},"action":"syn","event_time":{}}}"#,
            port,
            base + t * sec
        ));
    }

    // IP-B: 4 distinct ports → qualifying (scan=4)
    for port in [80, 443, 8080, 22] {
        t += 1;
        lines.push(format!(
            r#"{{"_stream":"netflow","sip":"10.0.0.2","dport":{},"action":"syn","event_time":{}}}"#,
            port,
            base + t * sec
        ));
    }

    // IP-C: 3 distinct ports → qualifying (scan=3)
    for port in [80, 443, 8080] {
        t += 1;
        lines.push(format!(
            r#"{{"_stream":"netflow","sip":"10.0.0.3","dport":{},"action":"syn","event_time":{}}}"#,
            port,
            base + t * sec
        ));
    }

    // IP-D: 2 distinct ports → NON-qualifying (scan=2 < 3)
    for port in [80, 443] {
        t += 1;
        lines.push(format!(
            r#"{{"_stream":"netflow","sip":"10.0.0.4","dport":{},"action":"syn","event_time":{}}}"#,
            port,
            base + t * sec
        ));
    }

    let ndjson = lines.join("\n");
    let reader = BufReader::new(ndjson.as_bytes());

    let result = replay_events(wfl, &schemas, reader, false).expect("replay should succeed");

    // 3 qualifying outputs, conv top(2) keeps 2; IP-D non-qualifying → no alert
    assert_eq!(result.match_count, 2, "expected 2 alerts after conv top(2)");
    assert_eq!(result.alerts.len(), 2);

    // Alerts should be for IP-A (scan=5) and IP-B (scan=4) after sort(-scan)
    let mut entity_ids: Vec<&str> = result.alerts.iter().map(|a| a.entity_id.as_str()).collect();
    entity_ids.sort();
    assert_eq!(entity_ids, vec!["10.0.0.1", "10.0.0.2"]);
}

#[test]
fn replay_pipeline_emits_only_final_rule_alerts() {
    let schemas = vec![make_auth_events_schema(), make_security_alerts_schema()];

    let wfl = r#"
rule pipe_replay {
    events { e : auth_events }
    match<sip:5m> {
        on event { s1: e | count >= 1; }
    }
    |> match<sip:5m> {
        on event { s2: _in | count >= 2; }
    } -> score(70.0)
    entity(ip, _in.sip)
    yield security_alerts (sip = _in.sip, fail_count = 2)
}
"#;

    let base_nanos = 1_700_000_000_000_000_000i64;
    let ndjson = format!(
        r#"{{"_stream":"auth_stream","sip":"10.0.0.1","action":"failed","user":"admin","event_time":{}}}"#,
        base_nanos
    ) + "\n"
        + &format!(
            r#"{{"_stream":"auth_stream","sip":"10.0.0.1","action":"failed","user":"admin","event_time":{}}}"#,
            base_nanos + 1_000_000_000
        );
    let reader = BufReader::new(ndjson.as_bytes());

    let result = replay_events(wfl, &schemas, reader, false).expect("replay should succeed");
    assert_eq!(result.event_count, 2);
    assert_eq!(result.match_count, 1);
    assert_eq!(result.error_count, 0);
    assert_eq!(result.alerts.len(), 1);
    assert_eq!(result.alerts[0].rule_name, "pipe_replay");
    assert!(
        !result.alerts[0].rule_name.starts_with("__wf_pipe_"),
        "replay must not output internal pipeline stage alerts"
    );
}

#[test]
fn replay_verify_mode_uses_timeout_not_eos() {
    let schemas = vec![make_auth_events_schema(), make_security_alerts_schema()];
    let wfl = r#"
rule timeout_only {
    events { e : auth_events }
    match<sip:5s> {
        on event { e | count >= 1; }
        and close { e | count >= 1; }
    } -> score(70.0)
    entity(ip, e.sip)
    yield security_alerts (sip = e.sip, fail_count = count(e))
}
"#;

    // Event times are strings (wfgen JSONL style). Verify mode should parse
    // them as event time and emit timeout close when watermark passes 5s.
    let ndjson = r#"{"_stream":"auth_stream","_timestamp":"1970-01-01T00:00:00Z","sip":"10.0.0.1","action":"failed","user":"u1","event_time":"1970-01-01T00:00:00Z"}
{"_stream":"auth_stream","_timestamp":"1970-01-01T00:00:07Z","sip":"10.0.0.2","action":"failed","user":"u2","event_time":"1970-01-01T00:00:07Z"}"#;
    let reader = BufReader::new(ndjson.as_bytes());

    let result =
        replay_events_for_verify(wfl, &schemas, reader, false).expect("replay should succeed");
    assert_eq!(result.event_count, 2);
    assert_eq!(result.error_count, 0);
    assert_eq!(result.match_count, 1, "only key 10.0.0.1 should timeout");
    assert_eq!(result.alerts.len(), 1);
    assert_eq!(result.alerts[0].entity_id, "10.0.0.1");
    assert_eq!(result.alerts[0].origin.as_str(), "close:timeout");
}
