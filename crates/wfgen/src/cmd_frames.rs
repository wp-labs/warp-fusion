//! Pre-encoded Arrow frame dump + raw byte replay.
//!
//! P0 "方案 A" for breaking the `wfgen send` ~480k EPS client ceiling:
//!
//! - `wfgen dump-frames` encodes JSONL events into the **exact on-wire payload**
//!   that `wfgen send` would emit (Arrow IPC + RFC6587 framing, per-stream tag)
//!   and stores those frame bytes once, off the benchmark hot path.
//! - `wfgen send-arrow` replays the stored bytes over a single TCP connection
//!   with **zero JSON parsing / Arrow encoding**, so the measured EPS reflects
//!   the runtime's real ingest ceiling instead of the generator client.
//!
//! This mirrors wpgen's direct-mode lesson (warp-parse `wpgen`): generation
//! produces the final native format in memory and ships it straight to the sink,
//! no intermediate file re-parse on each run.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use orion_error::conversion::SourceErr;

use wf_lang::WindowSchema;

use crate::error::{WfgenReason, WfgenResult};
use crate::loader::load_from_uses;
use crate::output::arrow_ipc::events_to_typed_batches;
use crate::output::jsonl::parse_gen_event_line;
use crate::wfg_parser::parse_wfg;

use crate::cmd_helpers::load_ws_files;
use crate::tcp_send::connect_sender;

/// `wfgen dump-frames`: read JSONL once and write the pre-encoded Arrow frames
/// (the byte-identical payloads `wfgen send` produces) to `output`.
///
/// A connected `TcpArrowSink` is only borrowed for its `framed` encoding mode;
/// the payloads go to `output`, not the network. `--addr` defaults to the
/// benchmark port and is where the sink connects for the encode borrow.
pub async fn dump_frames(
    scenario: PathBuf,
    input: PathBuf,
    addr: String,
    ws: Vec<PathBuf>,
    output: PathBuf,
    chunk: Option<usize>,
    max_frame_bytes: usize,
    max_frame_rows: usize,
) -> WfgenResult<()> {
    let wfg_content = std::fs::read_to_string(&scenario).source_err(
        WfgenReason::Io,
        format!("reading .wfg file: {}", scenario.display()),
    )?;
    let wfg = parse_wfg(&wfg_content)?;

    let (mut schemas, _) = load_from_uses(&wfg, &scenario, &HashMap::new(), false)?;
    schemas.extend(load_ws_files(&ws)?);

    let sink = connect_sender(&addr).await?;

    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).source_err(
            WfgenReason::Io,
            format!("creating output directory: {}", parent.display()),
        )?;
    }
    let mut writer = BufWriter::new(File::create(&output).source_err(
        WfgenReason::Io,
        format!("creating {}", output.display()),
    )?);

    // `-` reads stdin, otherwise a file. Events are accumulated up to `chunk`
    // rows per Arrow batch (None = one-shot, matching `send` without --chunk);
    // a chunk bounds per-batch memory for very large event counts.
    let reader: Box<dyn BufRead> = if input == Path::new("-") {
        Box::new(BufReader::new(std::io::stdin()))
    } else {
        let file =
            File::open(&input).source_err(WfgenReason::Io, format!("opening {}", input.display()))?;
        Box::new(BufReader::new(file))
    };

    let mut events: Vec<crate::datagen::stream_gen::GenEvent> = Vec::new();
    let mut total_events = 0usize;
    let mut total_frames = 0usize;
    let mut total_bytes = 0usize;

    for line in reader.lines() {
        let line = line.source_err(WfgenReason::Io, format!("reading {}", input.display()))?;
        if let Some(ev) = parse_gen_event_line(&line, &input)? {
            events.push(ev);
        }

        if let Some(n) = chunk {
            if events.len() >= n {
                total_frames += write_frames(
                    &events,
                    &schemas,
                    &sink,
                    &mut writer,
                    &mut total_bytes,
                    max_frame_bytes,
                    max_frame_rows,
                )?;
                total_events += events.len();
                events.clear();
            }
        }
    }

    if !events.is_empty() {
        total_frames += write_frames(
            &events,
            &schemas,
            &sink,
            &mut writer,
            &mut total_bytes,
            max_frame_bytes,
            max_frame_rows,
        )?;
        total_events += events.len();
    }

    writer
        .flush()
        .source_err(WfgenReason::Io, format!("flushing {}", output.display()))?;

    println!(
        "Dumped {} events as {} frame(s) ({} bytes) -> {}",
        total_events,
        total_frames,
        total_bytes,
        output.display()
    );
    Ok(())
}

/// `wfgen send-arrow`: replay pre-encoded frame bytes over one TCP connection.
///
/// The frames file is a byte stream of RFC6587 length-prefixed Arrow messages,
/// so sending is a plain `copy` — the client does no JSON parsing and no Arrow
/// encoding, isolating the runtime's ingest capacity.
pub async fn send_arrow(input: PathBuf, addr: String) -> WfgenResult<()> {
    use tokio::io::AsyncWriteExt;

    let mut file = tokio::fs::File::open(&input)
        .await
        .source_err(WfgenReason::Io, format!("opening {}", input.display()))?;
    let stream = tokio::net::TcpStream::connect(&addr)
        .await
        .source_err(WfgenReason::Network, format!("connecting to runtime: {addr}"))?;
    stream
        .set_nodelay(true)
        .source_err(WfgenReason::Network, "set_nodelay")?;
    let mut sink = stream;

    let start = std::time::Instant::now();
    let copied = tokio::io::copy(&mut file, &mut sink)
        .await
        .source_err(WfgenReason::Network, "tcp replay write error")?;
    sink.shutdown()
        .await
        .source_err(WfgenReason::Network, "tcp replay shutdown")?;
    let elapsed = start.elapsed();

    println!(
        "Replayed {} bytes in {:.3}s -> {}",
        copied,
        elapsed.as_secs_f64(),
        addr
    );
    Ok(())
}

/// Encode `events` into typed Arrow batches and append each framed payload.
fn write_frames(
    events: &[crate::datagen::stream_gen::GenEvent],
    schemas: &[WindowSchema],
    sink: &wp_core_connectors::sinks::tcp::TcpArrowSink,
    writer: &mut impl Write,
    total_bytes: &mut usize,
    max_frame_bytes: usize,
    max_frame_rows: usize,
) -> WfgenResult<usize> {
    let batches = events_to_typed_batches(events, schemas, max_frame_bytes, max_frame_rows)?;
    let mut frames = 0usize;
    for (stream_name, batch) in &batches {
        let payload = sink
            .encode_batch_payload_with_tag(stream_name, batch)
            .source_err(WfgenReason::Serialization, "encode_batch_payload failed")?;
        writer
            .write_all(&payload)
            .source_err(WfgenReason::Io, "writing frame bytes")?;
        *total_bytes += payload.len();
        frames += 1;
    }
    Ok(frames)
}
