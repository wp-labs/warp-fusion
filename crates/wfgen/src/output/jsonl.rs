use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use chrono::{DateTime, SecondsFormat, Utc};
use orion_error::conversion::SourceErr;

use crate::datagen::stream_gen::GenEvent;
use crate::error::{self, WfgenReason, WfgenResult};
use crate::oracle::OracleAlert;
use crate::verify::ActualAlert;

/// Write events as JSONL (one JSON object per line).
pub fn write_jsonl(events: &[GenEvent], output_path: &Path) -> WfgenResult<()> {
    // Create parent directories if needed
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).source_err(
            WfgenReason::Io,
            format!("creating output directory: {}", parent.display()),
        )?;
    }

    let file = File::create(output_path).source_err(
        WfgenReason::Io,
        format!("creating {}", output_path.display()),
    )?;
    let mut writer = BufWriter::new(file);

    for event in events {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "_stream".to_string(),
            serde_json::Value::String(event.stream_name.clone()),
        );
        obj.insert(
            "_window".to_string(),
            serde_json::Value::String(event.window_name.clone()),
        );
        obj.insert(
            "_timestamp".to_string(),
            serde_json::Value::String(event.timestamp.to_rfc3339_opts(SecondsFormat::Millis, true)),
        );

        // Merge event fields
        for (k, v) in &event.fields {
            obj.insert(k.clone(), v.clone());
        }

        let line = serde_json::to_string(&obj).source_err(
            WfgenReason::Serialization,
            "serializing generated event JSON",
        )?;
        writeln!(writer, "{}", line).source_err(
            WfgenReason::Io,
            format!("writing {}", output_path.display()),
        )?;
    }

    writer.flush().source_err(
        WfgenReason::Io,
        format!("flushing {}", output_path.display()),
    )?;
    Ok(())
}

/// Write oracle alerts as JSONL.
pub fn write_oracle_jsonl(alerts: &[OracleAlert], output_path: &Path) -> WfgenResult<()> {
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).source_err(
            WfgenReason::Io,
            format!("creating output directory: {}", parent.display()),
        )?;
    }

    let file = File::create(output_path).source_err(
        WfgenReason::Io,
        format!("creating {}", output_path.display()),
    )?;
    let mut writer = BufWriter::new(file);

    for alert in alerts {
        let line = serde_json::to_string(alert)
            .source_err(WfgenReason::Serialization, "serializing oracle alert JSON")?;
        writeln!(writer, "{}", line).source_err(
            WfgenReason::Io,
            format!("writing {}", output_path.display()),
        )?;
    }

    writer.flush().source_err(
        WfgenReason::Io,
        format!("flushing {}", output_path.display()),
    )?;
    Ok(())
}

/// Read events from a JSONL file.
///
/// Expects each line to contain `_stream`, `_window`, `_timestamp` metadata
/// fields plus the event payload fields.
pub fn read_events_jsonl(path: &Path) -> WfgenResult<Vec<GenEvent>> {
    let file =
        File::open(path).source_err(WfgenReason::Io, format!("opening {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut events = Vec::new();

    for line in reader.lines() {
        let line = line.source_err(WfgenReason::Io, format!("reading {}", path.display()))?;
        if line.trim().is_empty() {
            continue;
        }

        let obj: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&line)
            .source_err(
                WfgenReason::Serialization,
                format!("parsing {}", path.display()),
            )?;

        let stream_name = obj
            .get("_stream")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let window_name = obj
            .get("_window")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let timestamp: DateTime<Utc> = obj
            .get("_timestamp")
            .and_then(|v| v.as_str())
            .unwrap_or("1970-01-01T00:00:00Z")
            .parse()
            .unwrap_or_default();

        // Remaining fields (exclude metadata)
        let mut fields = serde_json::Map::new();
        for (k, v) in &obj {
            if !k.starts_with('_') {
                fields.insert(k.clone(), v.clone());
            }
        }

        events.push(GenEvent {
            stream_name,
            window_name,
            timestamp,
            fields,
        });
    }

    Ok(events)
}

/// Read actual alerts from a JSONL file.
pub fn read_alerts_jsonl(path: &Path) -> WfgenResult<Vec<ActualAlert>> {
    let file =
        File::open(path).source_err(WfgenReason::Io, format!("opening {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut alerts = Vec::new();

    for (line_no, line) in reader.lines().enumerate() {
        let line = line.source_err(WfgenReason::Io, format!("reading {}", path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        let alert = parse_actual_alert_line(&line).map_err(|err| {
            err.with_context(
                orion_error::OperationContext::doing("parsing actual alert JSONL")
                    .with_field("line", line_no + 1),
            )
        })?;
        alerts.push(alert);
    }

    Ok(alerts)
}

/// Read oracle alerts from a JSONL file.
pub fn read_oracle_jsonl(path: &Path) -> WfgenResult<Vec<OracleAlert>> {
    let file =
        File::open(path).source_err(WfgenReason::Io, format!("opening {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut alerts = Vec::new();

    for line in reader.lines() {
        let line = line.source_err(WfgenReason::Io, format!("reading {}", path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        let alert: OracleAlert = serde_json::from_str(&line).source_err(
            WfgenReason::Serialization,
            format!("parsing {}", path.display()),
        )?;
        alerts.push(alert);
    }

    Ok(alerts)
}

fn parse_actual_alert_line(line: &str) -> WfgenResult<ActualAlert> {
    let obj: serde_json::Map<String, serde_json::Value> = serde_json::from_str(line)
        .source_err(WfgenReason::Serialization, "parsing actual alert JSON")?;

    Ok(ActualAlert {
        rule_name: read_string(&obj, &["rule_name", "__wfu_rule_name"])?,
        score: read_f64(&obj, &["score", "__wfu_score"])?,
        entity_type: read_string(&obj, &["entity_type", "__wfu_entity_type"])?,
        entity_id: read_string(&obj, &["entity_id", "__wfu_entity_id"])?,
        origin: read_string(&obj, &["origin", "__wfu_origin"])?,
        fired_at: read_string(
            &obj,
            &["fired_at", "__wfu_fired_at", "emit_time", "__wfu_emit_time"],
        )?,
    })
}

fn read_string(
    obj: &serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> WfgenResult<String> {
    for key in keys {
        if let Some(value) = obj.get(*key) {
            return match value {
                serde_json::Value::String(s) => Ok(s.clone()),
                serde_json::Value::Number(n) => Ok(n.to_string()),
                serde_json::Value::Bool(b) => Ok(b.to_string()),
                _ => error::fail(
                    WfgenReason::Serialization,
                    format!("field `{key}` is not a scalar string-compatible value"),
                ),
            };
        }
    }

    error::fail(
        WfgenReason::Serialization,
        format!("missing required field, tried {:?}", keys),
    )
}

fn read_f64(obj: &serde_json::Map<String, serde_json::Value>, keys: &[&str]) -> WfgenResult<f64> {
    for key in keys {
        if let Some(value) = obj.get(*key) {
            return match value {
                serde_json::Value::Number(n) => n.as_f64().ok_or_else(|| {
                    error::error(
                        WfgenReason::Serialization,
                        format!("field `{key}` is not a finite f64-compatible number"),
                    )
                }),
                serde_json::Value::String(s) => s.parse().map_err(|e| {
                    error::error(
                        WfgenReason::Serialization,
                        format!("field `{key}` is not a valid f64 string: {e}"),
                    )
                }),
                _ => error::fail(
                    WfgenReason::Serialization,
                    format!("field `{key}` is not numeric"),
                ),
            };
        }
    }

    error::fail(
        WfgenReason::Serialization,
        format!("missing required field, tried {:?}", keys),
    )
}

#[cfg(test)]
mod tests {
    use super::parse_actual_alert_line;

    #[test]
    fn parses_legacy_actual_alert_shape() {
        let alert = parse_actual_alert_line(
            r#"{
                "rule_name":"r1",
                "score":42.5,
                "entity_type":"ip",
                "entity_id":"10.0.0.1",
                "origin":"close:timeout",
                "fired_at":"2024-01-01T00:00:00Z"
            }"#,
        )
        .unwrap();

        assert_eq!(alert.rule_name, "r1");
        assert_eq!(alert.score, 42.5);
        assert_eq!(alert.entity_type, "ip");
        assert_eq!(alert.entity_id, "10.0.0.1");
        assert_eq!(alert.origin, "close:timeout");
        assert_eq!(alert.fired_at, "2024-01-01T00:00:00Z");
    }

    #[test]
    fn parses_structured_runtime_alert_shape() {
        let alert = parse_actual_alert_line(
            r#"{
                "__wfu_rule_name":"r2",
                "__wfu_score":70.0,
                "__wfu_entity_type":"ip",
                "__wfu_entity_id":"10.0.18.77",
                "__wfu_origin":"close:timeout",
                "__wfu_fired_at":"1970-01-01T00:05:00.207Z",
                "__wfu_emit_time":"2026-03-11T01:52:20.501Z",
                "sip":"10.0.18.77"
            }"#,
        )
        .unwrap();

        assert_eq!(alert.rule_name, "r2");
        assert_eq!(alert.score, 70.0);
        assert_eq!(alert.entity_type, "ip");
        assert_eq!(alert.entity_id, "10.0.18.77");
        assert_eq!(alert.origin, "close:timeout");
        assert_eq!(alert.fired_at, "1970-01-01T00:05:00.207Z");
    }
}
