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

use wf_lang::{BaseType, FieldType, WindowSchema};

use crate::datagen::stream_gen::GenEvent;

/// Write events as Arrow IPC file.
///
/// All fields are stored as UTF-8 strings (JSON-encoded for non-string values)
/// with metadata columns `_stream`, `_window`, `_timestamp`.
pub fn write_arrow_ipc(events: &[GenEvent], output_path: &Path) -> anyhow::Result<()> {
    if events.is_empty() {
        anyhow::bail!("no events to write");
    }

    // Create parent directories if needed
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
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

    let batch = RecordBatch::try_new(schema.clone(), columns)?;

    let file = File::create(output_path)?;
    let mut writer = FileWriter::try_new(file, &schema)?;
    writer.write(&batch)?;
    writer.finish()?;

    Ok(())
}

/// Group GenEvents by window, build typed Arrow RecordBatches keyed by stream name.
///
/// Each window group produces one `(stream_name, RecordBatch)` pair. Column types
/// are derived from the [`WindowSchema`] field definitions, matching the runtime's
/// expected schema exactly.
pub fn events_to_typed_batches(
    events: &[GenEvent],
    schemas: &[WindowSchema],
) -> anyhow::Result<Vec<(String, RecordBatch)>> {
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
        let schema = schema_by_window
            .get(window_name)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("schema not found for window '{window_name}'"))?;

        let stream_name = schema
            .streams
            .first()
            .ok_or_else(|| anyhow::anyhow!("no stream defined for window '{window_name}'"))?;

        let arrow_fields: Vec<Field> = schema
            .fields
            .iter()
            .map(|f| Field::new(&f.name, field_type_to_arrow(&f.field_type), true))
            .collect();
        let arrow_schema = Arc::new(Schema::new(arrow_fields));

        let mut builders: Vec<ColumnBuilder> = schema
            .fields
            .iter()
            .map(|f| ColumnBuilder::new(&f.field_type, group_events.len()))
            .collect();
        for event in group_events {
            let fallback_ts = event.timestamp.timestamp_nanos_opt();
            for (field_def, builder) in schema.fields.iter().zip(builders.iter_mut()) {
                builder.push(event.fields.get(field_def.name.as_str()), fallback_ts);
            }
        }
        let columns: Vec<ArrayRef> = builders.into_iter().map(ColumnBuilder::finish).collect();

        let batch = RecordBatch::try_new(arrow_schema, columns)?;
        batches.push((stream_name.clone(), batch));
    }

    Ok(batches)
}

/// Convert a wf-lang [`FieldType`] to the corresponding Arrow [`DataType`].
fn field_type_to_arrow(ft: &FieldType) -> DataType {
    let base = match ft {
        FieldType::Base(b) => b,
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
            Self::Utf8(col) => col.push(value.and_then(|v| v.as_str()).map(String::from)),
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
