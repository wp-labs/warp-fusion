use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{
    ArrayRef, BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray,
    TimestampNanosecondArray,
};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::ipc::writer::FileWriter;
use chrono::{DateTime, SecondsFormat, Utc};
use orion_error::conversion::SourceRawErr;

use wf_lang::{BaseType, FieldType, WindowSchema};

use crate::datagen::stream_gen::GenEvent;
use crate::error::{self, WfgenReason, WfgenResult};

/// Write events as Arrow IPC file.
///
/// All fields are stored as UTF-8 strings (JSON-encoded for non-string values)
/// with metadata columns `_stream`, `_window`, `_timestamp`.
pub fn write_arrow_ipc(events: &[GenEvent], output_path: &Path) -> WfgenResult<()> {
    if events.is_empty() {
        return error::fail(WfgenReason::Generation, "no events to write");
    }

    // Create parent directories if needed
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).source_raw_err(
            WfgenReason::Io,
            format!("creating output directory: {}", parent.display()),
        )?;
    }

    // Collect all field names from events (preserving order from first event)
    let mut field_names: Vec<String> = Vec::new();
    // Always include metadata columns first
    field_names.push("_stream".to_string());
    field_names.push("_window".to_string());
    field_names.push("_timestamp".to_string());

    // Collect data field names from all events to avoid dropping sparse fields.
    for event in events {
        for key in event.fields.keys() {
            if !field_names.contains(key) {
                field_names.push(key.clone());
            }
        }
    }

    // Build Arrow schema — all fields as Utf8 for simplicity
    let arrow_fields: Vec<Field> = field_names
        .iter()
        .map(|name| Field::new(name, DataType::Utf8, true))
        .collect();
    let schema = Arc::new(Schema::new(arrow_fields));

    // Build columns
    let mut columns: Vec<ArrayRef> = Vec::new();

    for field_name in &field_names {
        let values: Vec<Option<String>> = events
            .iter()
            .map(|event| match field_name.as_str() {
                "_stream" => Some(event.stream_name.clone()),
                "_window" => Some(event.window_name.clone()),
                "_timestamp" => Some(event.timestamp.to_rfc3339_opts(SecondsFormat::Millis, true)),
                name => event.fields.get(name).map(|v| match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                }),
            })
            .collect();

        let array = StringArray::from(values);
        columns.push(Arc::new(array) as ArrayRef);
    }

    let batch = RecordBatch::try_new(schema.clone(), columns)
        .source_raw_err(WfgenReason::Serialization, "building Arrow record batch")?;

    let file = File::create(output_path).source_raw_err(
        WfgenReason::Io,
        format!("creating {}", output_path.display()),
    )?;
    let mut writer = FileWriter::try_new(file, &schema)
        .source_raw_err(WfgenReason::Serialization, "creating Arrow IPC writer")?;
    writer
        .write(&batch)
        .source_raw_err(WfgenReason::Serialization, "writing Arrow IPC batch")?;
    writer
        .finish()
        .source_raw_err(WfgenReason::Serialization, "finishing Arrow IPC writer")?;

    Ok(())
}

/// Upper bound on the encoded size of a single Arrow frame sent to the runtime.
///
/// A frame is appended to a window as *one* batch, and window memory eviction
/// operates on whole batches — a single oversized frame that exceeds the
/// window's `max_window_bytes` is dropped entirely (wp-labs/wp-reactor#18/#20).
/// Keeping frames at a small fraction of the window cap (default 256MB) avoids
/// that, and keeps the ordered commit worker from ever holding one giant
/// RecordBatch. Overcounting the per-event estimate only splits a frame a
/// little earlier — the safe direction.
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024; // 8 MiB

/// Secondary per-frame row cap: protects builder memory even when the byte
/// estimate is tiny (e.g. mostly-null or narrow rows).
const MAX_FRAME_ROWS: usize = 100_000;

/// Group GenEvents by window, build typed Arrow RecordBatches keyed by stream
/// name, splitting each window's events into multiple frames once a frame
/// exceeds [`MAX_FRAME_BYTES`] or [`MAX_FRAME_ROWS`].
///
/// Column types are derived from the [`WindowSchema`] field definitions,
/// matching the runtime's expected schema exactly. Frame splitting preserves
/// event order and never drops events.
pub fn events_to_typed_batches(
    events: &[GenEvent],
    schemas: &[WindowSchema],
) -> WfgenResult<Vec<(String, RecordBatch)>> {
    let schema_by_window: HashMap<&str, &WindowSchema> =
        schemas.iter().map(|s| (s.name.as_str(), s)).collect();
    let mut groups: HashMap<&str, Vec<&GenEvent>> = HashMap::new();
    for event in events {
        groups
            .entry(event.window_name.as_str())
            .or_default()
            .push(event);
    }

    let mut batches = Vec::new();

    for (window_name, group_events) in groups {
        let schema = schema_by_window.get(window_name).copied().ok_or_else(|| {
            error::error(
                WfgenReason::Validation,
                format!("schema not found for window '{window_name}'"),
            )
        })?;
        let stream_name = schema.streams.first().ok_or_else(|| {
            error::error(
                WfgenReason::Validation,
                format!("no stream defined for window '{window_name}'"),
            )
        })?;

        let mut frame: Vec<&GenEvent> = Vec::new();
        let mut frame_bytes = 0usize;
        for event in group_events {
            let est = event_frame_bytes(event, schema);
            if !frame.is_empty()
                && (frame_bytes + est > MAX_FRAME_BYTES || frame.len() + 1 > MAX_FRAME_ROWS)
            {
                build_frame(&mut batches, stream_name, schema, &frame)?;
                frame.clear();
                frame_bytes = 0;
            }
            frame.push(event);
            frame_bytes += est;
        }
        if !frame.is_empty() {
            build_frame(&mut batches, stream_name, schema, &frame)?;
        }
    }

    Ok(batches)
}

/// Build one typed Arrow RecordBatch from a frame of events and push it.
fn build_frame(
    batches: &mut Vec<(String, RecordBatch)>,
    stream_name: &str,
    schema: &WindowSchema,
    frame_events: &[&GenEvent],
) -> WfgenResult<()> {
    let arrow_fields: Vec<Field> = schema
        .fields
        .iter()
        .map(|f| Field::new(&f.name, field_type_to_arrow(&f.field_type), true))
        .collect();
    let arrow_schema = Arc::new(Schema::new(arrow_fields));

    let mut builders: Vec<ColumnBuilder> = schema
        .fields
        .iter()
        .map(|f| ColumnBuilder::new(&f.field_type, frame_events.len()))
        .collect();
    for event in frame_events {
        let fallback_ts = event.timestamp.timestamp_nanos_opt();
        for (field_def, builder) in schema.fields.iter().zip(builders.iter_mut()) {
            builder.push(event.fields.get(field_def.name.as_str()), fallback_ts);
        }
    }
    let columns: Vec<ArrayRef> = builders.into_iter().map(ColumnBuilder::finish).collect();

    let batch = RecordBatch::try_new(arrow_schema, columns).source_raw_err(
        WfgenReason::Serialization,
        "building typed Arrow record batch",
    )?;
    batches.push((stream_name.to_string(), batch));
    Ok(())
}

/// Conservative byte estimate of one event within a frame.
///
/// The runtime's window accounting charges *both* the Arrow content and the
/// parsed-event `HashMap` footprint (`content_bytes + events_bytes`). Object and
/// array fields decode into nested maps/vecs ~2-4× the JSON string, so they are
/// weighted accordingly; every other field is `4B offset + payload`, which
/// overcounts fixed-width primitives. Overestimating only splits frames a little
/// earlier — the safe direction.
fn event_frame_bytes(event: &GenEvent, schema: &WindowSchema) -> usize {
    let mut bytes = 16usize; // per-event row overhead
    for field in &schema.fields {
        let Some(value) = event.fields.get(field.name.as_str()) else {
            continue;
        };
        let blowup = match field.field_type {
            FieldType::Object | FieldType::ArrayAny | FieldType::Array(_) => 3,
            _ => 1,
        };
        bytes += 4 + json_value_len(value) * blowup;
    }
    bytes
}

/// Approximate serialized length of a JSON value (strings as-is, everything
/// else JSON-encoded — matching what the UTF-8 columns actually store).
fn json_value_len(v: &serde_json::Value) -> usize {
    match v {
        serde_json::Value::String(s) => s.len(),
        other => other.to_string().len(),
    }
}

/// Convert a wf-lang [`FieldType`] to the corresponding Arrow [`DataType`].
fn field_type_to_arrow(ft: &FieldType) -> DataType {
    let base = match ft {
        FieldType::Base(b) => b,
        FieldType::ArrayAny | FieldType::Object => return DataType::Utf8,
        FieldType::Array(b) => b,
    };
    match base {
        BaseType::Chars | BaseType::Ip | BaseType::Hex => DataType::Utf8,
        BaseType::Digit => DataType::Int64,
        BaseType::Float => DataType::Float64,
        BaseType::Bool => DataType::Boolean,
        BaseType::Time => DataType::Timestamp(TimeUnit::Nanosecond, None),
    }
}

enum ColumnBuilder {
    Utf8(Vec<Option<String>>),
    Int64(Vec<Option<i64>>),
    Float64(Vec<Option<f64>>),
    Bool(Vec<Option<bool>>),
    TimeNanos(Vec<Option<i64>>),
}

impl ColumnBuilder {
    fn new(field_type: &FieldType, cap: usize) -> Self {
        let base = match field_type {
            FieldType::Base(b) => b,
            FieldType::ArrayAny | FieldType::Object => return Self::Utf8(Vec::with_capacity(cap)),
            FieldType::Array(b) => b,
        };
        match base {
            BaseType::Chars | BaseType::Ip | BaseType::Hex => Self::Utf8(Vec::with_capacity(cap)),
            BaseType::Digit => Self::Int64(Vec::with_capacity(cap)),
            BaseType::Float => Self::Float64(Vec::with_capacity(cap)),
            BaseType::Bool => Self::Bool(Vec::with_capacity(cap)),
            BaseType::Time => Self::TimeNanos(Vec::with_capacity(cap)),
        }
    }

    fn push(&mut self, value: Option<&serde_json::Value>, fallback_time: Option<i64>) {
        match self {
            Self::Utf8(col) => col.push(value.map(|v| match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            })),
            Self::Int64(col) => col.push(value.and_then(|v| v.as_i64())),
            Self::Float64(col) => col.push(value.and_then(|v| v.as_f64())),
            Self::Bool(col) => col.push(value.and_then(|v| v.as_bool())),
            Self::TimeNanos(col) => {
                let parsed = value.and_then(|v| {
                    if let Some(n) = v.as_i64() {
                        return Some(n);
                    }
                    if let Some(s) = v.as_str()
                        && let Ok(dt) = s.parse::<DateTime<Utc>>()
                    {
                        return dt.timestamp_nanos_opt();
                    }
                    None
                });
                col.push(parsed.or(fallback_time));
            }
        }
    }

    fn finish(self) -> ArrayRef {
        match self {
            Self::Utf8(col) => Arc::new(StringArray::from(col)),
            Self::Int64(col) => Arc::new(Int64Array::from(col)),
            Self::Float64(col) => Arc::new(Float64Array::from(col)),
            Self::Bool(col) => Arc::new(BooleanArray::from(col)),
            Self::TimeNanos(col) => Arc::new(TimestampNanosecondArray::from(col)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;
    use std::time::Duration;
    use wf_lang::{BaseType, FieldDef, FieldType, WindowSchema};

    fn schema() -> WindowSchema {
        WindowSchema {
            name: "conn_events".into(),
            streams: vec!["conn_events".into()],
            time_field: Some("event_time".into()),
            over: Duration::from_secs(120),
            fields: vec![
                FieldDef {
                    name: "sip".into(),
                    field_type: FieldType::Base(BaseType::Ip),
                },
                FieldDef {
                    name: "event_time".into(),
                    field_type: FieldType::Base(BaseType::Time),
                },
                FieldDef {
                    name: "conn_info".into(),
                    field_type: FieldType::Object,
                },
            ],
        }
    }

    fn event(i: usize, conn_info: Option<&serde_json::Value>) -> GenEvent {
        let mut fields = serde_json::Map::new();
        fields.insert("sip".into(), json!("10.0.0.1"));
        fields.insert("event_time".into(), json!("2026-08-13T00:00:00Z"));
        if let Some(v) = conn_info {
            fields.insert("conn_info".into(), v.clone());
        }
        GenEvent {
            stream_name: "conn_events".into(),
            window_name: "conn_events".into(),
            timestamp: Utc::now(),
            fields,
        }
    }

    /// Object-heavy events must split into multiple byte-bounded frames instead
    /// of one giant frame per window (which would exceed a window's
    /// `max_window_bytes` and be dropped whole — wp-labs/wp-reactor#20).
    #[test]
    fn object_heavy_events_split_into_byte_bounded_frames() {
        let big = json!({"data": "x".repeat(2000)}); // ~2KB object field per row
        let events: Vec<GenEvent> = (0..10_000).map(|i| event(i, Some(&big))).collect();
        let per_event = event_frame_bytes(&events[0], &schema());

        let batches = events_to_typed_batches(&events, &[schema()]).unwrap();

        assert!(
            batches.len() >= 2,
            "10k × ~2KB events must split into multiple frames ({}), not one giant frame",
            batches.len()
        );
        let total_rows: usize = batches.iter().map(|(_, b)| b.num_rows()).sum();
        assert_eq!(total_rows, events.len(), "no event may be dropped or duplicated");
        for (_, b) in &batches {
            assert!(
                b.num_rows() * per_event <= MAX_FRAME_BYTES,
                "frame of {} rows × {per_event}B must stay under the byte cap",
                b.num_rows()
            );
            assert!(b.num_rows() <= MAX_FRAME_ROWS);
        }
    }

    /// Narrow (mostly-null) events must still split at the row cap so a single
    /// RecordBatch never pins the commit worker with one huge vector.
    #[test]
    fn narrow_events_split_at_row_cap() {
        // ~52B/event → 100k rows ≈ 5.2MB < MAX_FRAME_BYTES, so the row cap is
        // the binding constraint.
        let events: Vec<GenEvent> = (0..(MAX_FRAME_ROWS + MAX_FRAME_ROWS / 2))
            .map(|i| event(i, None))
            .collect();

        let batches = events_to_typed_batches(&events, &[schema()]).unwrap();

        assert!(
            batches.len() >= 2,
            "150k narrow events must split at the row cap ({} frames)",
            batches.len()
        );
        let total_rows: usize = batches.iter().map(|(_, b)| b.num_rows()).sum();
        assert_eq!(total_rows, events.len());
        for (_, b) in &batches {
            assert!(
                b.num_rows() <= MAX_FRAME_ROWS,
                "frame must not exceed the row cap ({} rows)",
                b.num_rows()
            );
        }
    }
}
