use std::collections::HashMap;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rand::rngs::StdRng;
use serde_json::Value;
use wf_lang::{BaseType, FieldType, WindowSchema};

use crate::wfg_ast::{GenExpr, StreamBlock};

use super::field_gen::generate_field_value;

/// A single generated event.
#[derive(Debug, Clone)]
pub struct GenEvent {
    /// The actual stream name from schema (e.g., "syslog"), used for `_stream` in output.
    pub stream_name: String,
    /// The window name (e.g., "auth_events").
    pub window_name: String,
    pub timestamp: DateTime<Utc>,
    pub fields: serde_json::Map<String, Value>,
}

/// Generate events for a single stream.
pub fn generate_stream_events(
    stream: &StreamBlock,
    schema: &WindowSchema,
    event_count: u64,
    start: &DateTime<Utc>,
    duration: &std::time::Duration,
    rng: &mut StdRng,
) -> Vec<GenEvent> {
    let mut events = Vec::with_capacity(event_count as usize);

    // Get the actual stream name from schema (e.g., "syslog")
    let stream_name = schema
        .streams
        .first()
        .cloned()
        .unwrap_or_else(|| schema.name.clone());

    // Build field override lookup
    let overrides: HashMap<&str, &GenExpr> = stream
        .overrides
        .iter()
        .map(|o| (o.field_name.as_str(), &o.gen_expr))
        .collect();

    let duration_nanos = duration.as_nanos() as i64;
    let interval = if event_count > 1 {
        duration_nanos / (event_count as i64)
    } else {
        0
    };

    for i in 0..event_count {
        let ts = *start + ChronoDuration::nanoseconds(interval * i as i64);

        let mut fields = serde_json::Map::new();

        for field_def in &schema.fields {
            let override_expr = overrides.get(field_def.name.as_str()).copied();

            // For Time fields, set the timestamp
            if matches!(&field_def.field_type, FieldType::Base(BaseType::Time))
                && (override_expr.is_none()
                    || matches!(override_expr, Some(GenExpr::GenFunc { name, .. }) if name == "timestamp"))
            {
                fields.insert(
                    field_def.name.clone(),
                    serde_json::json!(ts.timestamp_nanos_opt().unwrap_or(0)),
                );
                continue;
            }

            let value = generate_field_value(&field_def.field_type, override_expr, rng);
            fields.insert(field_def.name.clone(), value);
        }

        events.push(GenEvent {
            stream_name: stream_name.clone(),
            window_name: stream.window.clone(),
            timestamp: ts,
            fields,
        });
    }

    events
}
