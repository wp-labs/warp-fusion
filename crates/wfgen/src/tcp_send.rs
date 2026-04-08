use std::io::Write;
use std::net::TcpStream;

use anyhow::Context;

use wf_lang::WindowSchema;
use wfgen::datagen::stream_gen::GenEvent;
use wfgen::output::arrow_ipc::events_to_typed_batches;

pub(crate) fn connect_sender(addr: &str) -> anyhow::Result<TcpStream> {
    let stream =
        TcpStream::connect(addr).with_context(|| format!("connecting to runtime: {addr}"))?;
    stream
        .set_nodelay(true)
        .context("setting TCP_NODELAY on sender socket")?;
    Ok(stream)
}

pub(crate) fn send_events_with_stream(
    events: &[GenEvent],
    schemas: &[WindowSchema],
    stream: &mut TcpStream,
) -> anyhow::Result<usize> {
    if events.is_empty() {
        anyhow::bail!("no events to send");
    }

    let batches = events_to_typed_batches(events, schemas)?;
    if batches.is_empty() {
        anyhow::bail!("no arrow batches built from events");
    }

    let mut sent_frames = 0usize;
    for (stream_name, batch) in &batches {
        let ipc_payload = wp_arrow::ipc::encode_ipc(stream_name, batch)
            .with_context(|| format!("encode_ipc failed for stream '{stream_name}'"))?;
        let len = (ipc_payload.len() as u32).to_be_bytes();
        stream
            .write_all(&len)
            .with_context(|| format!("sending frame header for stream '{stream_name}'"))?;
        stream
            .write_all(&ipc_payload)
            .with_context(|| format!("sending frame for stream '{stream_name}'"))?;
        sent_frames += 1;
    }
    stream.flush().context("flushing sender socket")?;

    Ok(sent_frames)
}

pub(crate) fn send_events(
    events: &[GenEvent],
    schemas: &[WindowSchema],
    addr: &str,
) -> anyhow::Result<usize> {
    let mut stream = connect_sender(addr)?;
    send_events_with_stream(events, schemas, &mut stream)
}
