use std::io::Write;
use std::net::TcpStream;

use orion_error::conversion::{SourceErr, SourceRawErr};

use wf_lang::WindowSchema;
use wfgen::datagen::stream_gen::GenEvent;
use wfgen::error::{self, WfgenReason, WfgenResult};
use wfgen::output::arrow_ipc::events_to_typed_batches;

pub(crate) fn connect_sender(addr: &str) -> WfgenResult<TcpStream> {
    let stream = TcpStream::connect(addr).source_err(
        WfgenReason::Network,
        format!("connecting to runtime: {addr}"),
    )?;
    stream
        .set_nodelay(true)
        .source_err(WfgenReason::Network, "setting TCP_NODELAY on sender socket")?;
    Ok(stream)
}

pub(crate) fn send_events_with_stream(
    events: &[GenEvent],
    schemas: &[WindowSchema],
    stream: &mut TcpStream,
) -> WfgenResult<usize> {
    if events.is_empty() {
        return error::fail(WfgenReason::Network, "no events to send");
    }

    let batches = events_to_typed_batches(events, schemas)?;
    if batches.is_empty() {
        return error::fail(WfgenReason::Network, "no arrow batches built from events");
    }

    let mut sent_frames = 0usize;
    for (stream_name, batch) in &batches {
        let ipc_payload = wp_arrow::ipc::encode_ipc(stream_name, batch).source_raw_err(
            WfgenReason::Serialization,
            format!("encode_ipc failed for stream '{stream_name}'"),
        )?;
        let len = (ipc_payload.len() as u32).to_be_bytes();
        stream.write_all(&len).source_err(
            WfgenReason::Network,
            format!("sending frame header for stream '{stream_name}'"),
        )?;
        stream.write_all(&ipc_payload).source_err(
            WfgenReason::Network,
            format!("sending frame for stream '{stream_name}'"),
        )?;
        sent_frames += 1;
    }
    stream
        .flush()
        .source_err(WfgenReason::Network, "flushing sender socket")?;

    Ok(sent_frames)
}

pub(crate) fn send_events(
    events: &[GenEvent],
    schemas: &[WindowSchema],
    addr: &str,
) -> WfgenResult<usize> {
    let mut stream = connect_sender(addr)?;
    send_events_with_stream(events, schemas, &mut stream)
}
