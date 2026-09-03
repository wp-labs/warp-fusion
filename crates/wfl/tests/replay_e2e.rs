use std::io::BufReader;
use std::time::Duration;

use wf_engine::match_engine::Value;
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
    assert_eq!(alert.rule_name.as_ref(), "brute_force");
    assert!((alert.score - 70.0).abs() < f64::EPSILON);
    assert_eq!(alert.entity_type.as_ref(), "ip");
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
    assert_eq!(alert.rule_name.as_ref(), "eos_close");
    assert!((alert.score - 80.0).abs() < f64::EPSILON);
    assert_eq!(alert.entity_type.as_ref(), "ip");
    assert_eq!(alert.entity_id, "10.0.0.1");
}

// ===========================================================================
// let 派生字段（issue #79）：match 路径 apply_lets，entity/yield 按裸名引用
// ===========================================================================

fn make_alert_out_schema() -> WindowSchema {
    WindowSchema {
        name: "alert_out".to_string(),
        streams: vec![],
        time_field: None,
        over: Duration::from_secs(3600),
        fields: vec![
            FieldDef {
                name: "tenant".to_string(),
                field_type: FieldType::Base(BaseType::Chars),
            },
            FieldDef {
                name: "dedup".to_string(),
                field_type: FieldType::Base(BaseType::Chars),
            },
        ],
    }
}

/// match 规则 + `let` 派生字段（issue #79）：`tenant = first(e.user)`、
/// `dedup = join_by("|", tenant, first(e.action))`（链式引用），entity 与 yield
/// 都按裸名引用派生值——验证解析 → 编译 → match 路径 apply_lets 的完整链路。
#[test]
fn replay_match_rule_with_lets() {
    let schemas = vec![
        make_auth_events_schema(),
        make_security_alerts_schema(),
        make_alert_out_schema(),
    ];
    let wfl = r#"
rule let_derive {
    events { e : auth_events }
    let tenant = e.user
    let dedup = join_by("|", tenant, e.action)
    match<sip:5m> {
        on event { e | count >= 2; }
    } -> score(70.0)
    entity(chars, tenant)
    yield alert_out (tenant = tenant, dedup = dedup)
}
"#;
    let ndjson = make_ndjson_events(2); // user=admin, action=failed
    let reader = BufReader::new(ndjson.as_bytes());

    let result = replay_events(wfl, &schemas, reader, false).expect("replay should succeed");

    assert_eq!(result.event_count, 2);
    assert_eq!(result.match_count, 1);
    assert_eq!(result.error_count, 0);
    assert_eq!(result.alerts.len(), 1);
    let alert = &result.alerts[0];
    assert_eq!(
        alert.entity_id, "admin",
        "entity(chars, tenant) → let 派生值"
    );
    assert_eq!(alert.yield_fields.len(), 2);
    assert_eq!(
        alert.yield_fields[0].1,
        Value::Str("admin".into()),
        "tenant 派生值"
    );
    assert_eq!(
        alert.yield_fields[1].1,
        Value::Str("admin|failed".into()),
        "dedup 派生值（链式引用 tenant）"
    );
}

// ===========================================================================
// match 表达式（issue #79 Issue 2）：枚举归一化 + 多模式 `|` + 默认 `_`
// ===========================================================================

/// match 表达式在 yield 中做枚举归一化：`failed` → 5、`locked`/`disabled`
/// → 9（多模式 `|`）、其余 → 1（默认 `_`）。验证解析 → 编译 → 引擎求值链路。
#[test]
fn replay_match_expr_severity() {
    let schemas = vec![make_auth_events_schema(), make_security_alerts_schema()];
    let wfl = r#"
rule sev_map {
    events { e : auth_events }
    match<sip:5m> {
        on event { e | count >= 1; }
    } -> score(50.0)
    entity(ip, e.sip)
    yield security_alerts (sip = e.sip, fail_count = case e.action {
        "failed" => 5,
        "locked" | "disabled" => 9,
        _ => 1,
    })
}
"#;
    let ndjson = make_ndjson_events(1); // action=failed
    let reader = BufReader::new(ndjson.as_bytes());
    let result = replay_events(wfl, &schemas, reader, false).expect("replay should succeed");
    assert_eq!(result.error_count, 0);
    assert_eq!(result.alerts.len(), 1);
    assert_eq!(
        result.alerts[0].yield_fields[1].1,
        Value::Number(5.0),
        "failed → 5"
    );

    // locked → 9（多模式第二个命中）；other → 1（默认分支）。
    for (action, expected) in [("locked", 9.0), ("info", 1.0)] {
        let ndjson = format!(
            r#"{{"_stream":"auth_stream","sip":"10.0.0.1","action":"{action}","user":"admin","event_time":1700000000000000000}}"#
        );
        let reader = BufReader::new(ndjson.as_bytes());
        let result = replay_events(wfl, &schemas, reader, false).expect("replay should succeed");
        assert_eq!(result.error_count, 0);
        assert_eq!(result.alerts.len(), 1);
        assert_eq!(
            result.alerts[0].yield_fields[1].1,
            Value::Number(expected),
            "{action} → {expected}"
        );
    }
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
    assert_eq!(alert.rule_name.as_ref(), "multi_src");
    // fired_at must be derived from the event time (tb), not default to 0 (1970).
    // The nanosecond timestamp 1_700_000_000_000_000_000 is ~2023-11-14.
    // Convert fired_at (ISO string) year to verify it's not 1970.
    assert!(
        !alert.fired_at.starts_with("1970"),
        "fired_at should not be 1970 (got {}); time_field was not resolved from schema",
        alert.fired_at
    );
}

#[test]
fn replay_time_field_accepts_millis_timestamp() {
    let schemas = vec![make_auth_events_schema(), make_security_alerts_schema()];

    let base_millis = 1_700_000_000_000i64;
    let ndjson = (0..5)
        .map(|i| {
            format!(
                r#"{{"_stream":"auth_stream","sip":"10.0.0.1","action":"failed","user":"admin","event_time":{}}}"#,
                base_millis + i * 1_000
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let reader = BufReader::new(ndjson.as_bytes());

    let result = replay_events(WFL_RULE, &schemas, reader, false).expect("replay should succeed");

    assert_eq!(result.event_count, 5);
    assert_eq!(result.match_count, 1);
    assert_eq!(result.error_count, 0);
    assert_eq!(result.alerts.len(), 1);
    assert!(
        result.alerts[0].fired_at.starts_with("2023-11-14"),
        "millis timestamp should be normalized to event-time nanos, got {}",
        result.alerts[0].fired_at
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

    // 水印推进事件（2026-08-24 修: q5 close_all 对齐 oracle 后, HOP/Fixed 窗口
    // 只收口**完整窗口**——尾部未完整窗口（w_end > 最终 watermark）释放但不
    // 发射。1h:fixed 窗口 + 14s 数据永不完整 → EOF 不产出 qualifying → conv 无
    // 输入。此 dummy 事件把 watermark 推到窗口终点之后（w_end = base + 2800s,
    // base mod 1h = 800s）→ 窗口完整 close; dummy IP scan=1 不 qualify, 且落入
    // 下一窗口 EOF 不发射, 不影响断言）。
    lines.push(format!(
        r#"{{"_stream":"netflow","sip":"10.0.0.9","dport":9999,"action":"syn","event_time":{}}}"#,
        base + 2_800_000_000_001i64
    ));

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
    assert_eq!(result.alerts[0].rule_name.as_ref(), "pipe_replay");
    assert!(
        !result.alerts[0].rule_name.starts_with("__wf_pipe_"),
        "replay must not output internal pipeline stage alerts"
    );
}

#[test]
fn replay_verify_mode_timeout_and_eof_close() {
    let schemas = vec![make_auth_events_schema(), make_security_alerts_schema()];
    let wfl = r#"
rule timeout_and_eof {
    events { e : auth_events }
    match<sip:5s> {
        on event { e | count >= 1; }
        and close { e | count >= 1; }
    } -> score(70.0)
    entity(ip, e.sip)
    yield security_alerts (sip = e.sip, fail_count = count(e))
}
"#;

    // 10.0.0.1 的窗口在 5s 处被第二条事件的 watermark（7s）扫过期 → 中途
    // timeout 发射；10.0.0.2 的窗口（起点 7s，未过期）在 EOF 时 close_all
    // 收口发射（issue #23：verify 必须补尾部未过期窗口，与 replay/test 及
    // 引擎 flush 收口语义一致，否则 span 短于窗口的数据恒 0 匹配）。
    let ndjson = r#"{"_stream":"auth_stream","_timestamp":"1970-01-01T00:00:00Z","sip":"10.0.0.1","action":"failed","user":"u1","event_time":"1970-01-01T00:00:00Z"}
{"_stream":"auth_stream","_timestamp":"1970-01-01T00:00:07Z","sip":"10.0.0.2","action":"failed","user":"u2","event_time":"1970-01-01T00:00:07Z"}"#;
    let reader = BufReader::new(ndjson.as_bytes());

    let result =
        replay_events_for_verify(wfl, &schemas, reader, false).expect("replay should succeed");
    assert_eq!(result.event_count, 2);
    assert_eq!(result.error_count, 0);
    assert_eq!(
        result.match_count, 2,
        "timeout (10.0.0.1) + EOF close (10.0.0.2)"
    );
    assert_eq!(result.alerts.len(), 2);

    let mut by_entity: Vec<(&str, &str)> = result
        .alerts
        .iter()
        .map(|a| (a.entity_id.as_str(), a.origin.as_str()))
        .collect();
    by_entity.sort();
    assert_eq!(by_entity[0], ("10.0.0.1", "close:timeout"));
    assert_eq!(by_entity[1], ("10.0.0.2", "close:eos"));

    // 对照：replay（无逐事件扫描）在 EOF 统一 close_all——同输入也覆盖到
    // 10.0.0.1（origin 为 eos 而非 timeout，语义差异见 run_timeout_scan 注释）。
    let reader = BufReader::new(ndjson.as_bytes());
    let replay_result = replay_events(wfl, &schemas, reader, false).expect("replay should succeed");
    assert_eq!(
        replay_result.match_count, 2,
        "replay closes both keys at EOF"
    );
    let mut replay_origins: Vec<&str> = replay_result
        .alerts
        .iter()
        .map(|a| a.origin.as_str())
        .collect();
    replay_origins.sort();
    assert_eq!(
        replay_origins,
        vec!["close:eos", "close:eos"],
        "replay (no mid-stream scan) closes everything at EOF"
    );
}

// ===========================================================================
// Bind filter on NDJSON replay（issue #23）
// ===========================================================================
//
// `events { c : conn_events && dport == 4444 }` 的 bind filter 必须在 replay
// 驱动里逐事件应用（与 `wfl test` / 生产 rule_task 的 alias 过滤一致）。此前
// replay 漏掉该前置过滤：被 filter 排除的事件仍进入状态机，close 步累积把
// 良性事件计入 count → 误触发 / count 虚高。digit（dport）与 chars（action）
// 两条路径都验证。

/// 只有良性事件（dport=443/80，均非 4444）→ 规则必须 0 触发。
/// 修复前：dport==4444 过滤被忽略，两行良性事件各触发 1 条 close 告警。
#[test]
fn replay_bind_filter_excludes_benign_only_input() {
    let schemas = vec![make_conn_events_schema(), make_network_alerts_schema()];
    let wfl = r#"
rule port_filter {
    events { c : conn_events && dport == 4444 }
    match<sip:5m> {
        on event { c | count >= 1; }
        and close { c | count >= 1; }
    } -> score(80.0)
    entity(ip, c.sip)
    yield network_alerts (sip = c.sip, alert_type = "x")
}
"#;
    let base = 1_700_000_000_000_000_000i64;
    let sec = 1_000_000_000i64;
    let ndjson = format!(
        r#"{{"_stream":"netflow","sip":"10.0.0.1","dport":443,"action":"syn","event_time":{}}}"#,
        base
    ) + "\n"
        + &format!(
            r#"{{"_stream":"netflow","sip":"10.0.0.2","dport":80,"action":"syn","event_time":{}}}"#,
            base + sec
        );
    let reader = BufReader::new(ndjson.as_bytes());
    let result = replay_events(wfl, &schemas, reader, false).expect("replay should succeed");
    assert_eq!(result.event_count, 2);
    assert_eq!(
        result.match_count, 0,
        "bind filter dport==4444 must reject benign-only events"
    );
    assert_eq!(result.error_count, 0);
    assert!(result.alerts.is_empty());
}

/// 恶意(dport=4444) + 良性(dport=80) 同 sip → 恰好 1 触发。close 步用
/// `count == 1` 精确断言：修复前良性事件被计入 close 累积使 count=2，
/// `== 1` 不满足 → 漏报；修复后只有命中 filter 的事件计入 → count=1 触发。
#[test]
fn replay_bind_filter_does_not_inflate_close_count() {
    let schemas = vec![make_conn_events_schema(), make_network_alerts_schema()];
    let wfl = r#"
rule port_filter_exact {
    events { c : conn_events && dport == 4444 }
    match<sip:5m> {
        on event { c | count >= 1; }
        and close { exact: c | count == 1; }
    } -> score(80.0)
    entity(ip, c.sip)
    yield network_alerts (sip = c.sip, alert_type = "x")
}
"#;
    let base = 1_700_000_000_000_000_000i64;
    let sec = 1_000_000_000i64;
    let ndjson = format!(
        r#"{{"_stream":"netflow","sip":"10.0.0.1","dport":4444,"action":"syn","event_time":{}}}"#,
        base
    ) + "\n"
        + &format!(
            r#"{{"_stream":"netflow","sip":"10.0.0.1","dport":80,"action":"syn","event_time":{}}}"#,
            base + sec
        );
    let reader = BufReader::new(ndjson.as_bytes());
    let result = replay_events(wfl, &schemas, reader, false).expect("replay should succeed");
    assert_eq!(result.event_count, 2);
    assert_eq!(result.error_count, 0);
    assert_eq!(
        result.match_count, 1,
        "benign event must not inflate close count; expected exactly 1 alert"
    );
    assert_eq!(result.alerts.len(), 1);
    assert_eq!(result.alerts[0].entity_id, "10.0.0.1");
}

/// chars bind filter（对照，issue #23 报告中 chars 正常的论断）：
/// `action == "syn"` 过滤下，非 syn 事件不得进入机器。
#[test]
fn replay_chars_bind_filter_excludes_non_matching() {
    let schemas = vec![make_conn_events_schema(), make_network_alerts_schema()];
    let wfl = r#"
rule action_filter {
    events { c : conn_events && action == "syn" }
    match<sip:5m> {
        on event { c | count >= 1; }
        and close { exact: c | count == 1; }
    } -> score(80.0)
    entity(ip, c.sip)
    yield network_alerts (sip = c.sip, alert_type = "x")
}
"#;
    let base = 1_700_000_000_000_000_000i64;
    let sec = 1_000_000_000i64;
    // 1 条命中(action=syn) + 1 条不命中(action=fin)：close 精确计数必须只含
    // syn 事件。
    let ndjson = format!(
        r#"{{"_stream":"netflow","sip":"10.0.0.1","dport":4444,"action":"syn","event_time":{}}}"#,
        base
    ) + "\n"
        + &format!(
            r#"{{"_stream":"netflow","sip":"10.0.0.1","dport":80,"action":"fin","event_time":{}}}"#,
            base + sec
        );
    let reader = BufReader::new(ndjson.as_bytes());
    let result = replay_events(wfl, &schemas, reader, false).expect("replay should succeed");
    assert_eq!(result.event_count, 2);
    assert_eq!(result.error_count, 0);
    assert_eq!(
        result.match_count, 1,
        "non-syn event must not inflate close count; expected exactly 1 alert"
    );
    assert_eq!(result.alerts.len(), 1);
    assert_eq!(result.alerts[0].entity_id, "10.0.0.1");
}
