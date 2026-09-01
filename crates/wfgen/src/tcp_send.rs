use std::collections::BTreeMap;

use orion_error::conversion::SourceErr;
use wp_connector_api::SinkSpec;
use wp_core_connectors::sinks::tcp::TcpArrowSink;

use crate::datagen::stream_gen::GenEvent;
use crate::error::{self, WfgenReason, WfgenResult};
use crate::output::arrow_ipc::events_to_typed_batches;
use wf_lang::WindowSchema;

/// Connect to the wfusion runtime via wp-core-connectors' TcpArrowSink.
///
/// This uses the exact same Arrow IPC encoding + RFC6587 framing pipeline
/// as wparse's tcp_sink connector, ensuring byte-level compatibility with
/// wfusion's tcp_src.
pub async fn connect_sender(addr: &str) -> WfgenResult<TcpArrowSink> {
    let (host, port_str) = addr.rsplit_once(':').ok_or_else(|| {
        error::error(
            WfgenReason::Network,
            format!("invalid address '{addr}': expected host:port"),
        )
    })?;
    let port: i64 = port_str
        .parse()
        .map_err(|_| error::error(WfgenReason::Network, format!("invalid port in '{addr}'")))?;

    let mut params: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    params.insert("addr".into(), host.into());
    params.insert("port".into(), port.into());
    params.insert("data_format".into(), "arrow_framed".into());
    params.insert("tag".into(), "".into());

    let spec = SinkSpec {
        group: "wfgen".into(),
        name: "wfgen_tcp".into(),
        kind: "tcp".into(),
        connector_id: "tcp_sink".into(),
        params,
        filter: None,
    };

    let sink = TcpArrowSink::connect(&spec, 0).await.source_err(
        WfgenReason::Network,
        format!("connecting to runtime: {addr}"),
    )?;
    Ok(sink)
}

/// Send generated events via wp-core-connectors' TcpArrowSink.
///
/// Each window's events are grouped into a RecordBatch, then passed through
/// TcpArrowSink's standard Arrow IPC encoding → RFC6587 framing → NetWriter
/// pipeline — the same code path used by wparse's tcp_sink connector.
pub async fn send_events_with_stream(
    events: &[GenEvent],
    schemas: &[WindowSchema],
    sink: &mut TcpArrowSink,
) -> WfgenResult<usize> {
    if events.is_empty() {
        return error::fail(WfgenReason::Network, "no events to send");
    }

    let batches = events_to_typed_batches(
        events,
        schemas,
        crate::output::arrow_ipc::DEFAULT_MAX_FRAME_BYTES,
        crate::output::arrow_ipc::DEFAULT_MAX_FRAME_ROWS,
    )?;
    if batches.is_empty() {
        return error::fail(WfgenReason::Network, "no arrow batches built from events");
    }

    let mut sent_frames = 0usize;
    for (stream_name, batch) in &batches {
        let payload = sink
            .encode_batch_payload_with_tag(stream_name, batch)
            .source_err(WfgenReason::Serialization, "encode_batch_payload failed")?;
        sink.send_payload(&payload)
            .await
            .source_err(WfgenReason::Network, "tcp send error")?;
        sent_frames += 1;
    }

    Ok(sent_frames)
}

/// Connect and send a single batch of events (fire-and-forget).
pub async fn send_events(
    events: &[GenEvent],
    schemas: &[WindowSchema],
    addr: &str,
) -> WfgenResult<usize> {
    let mut sink = connect_sender(addr).await?;
    send_events_with_stream(events, schemas, &mut sink).await
}
