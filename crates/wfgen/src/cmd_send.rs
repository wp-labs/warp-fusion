use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Duration;

use orion_error::conversion::SourceErr;

use crate::error::{WfgenReason, WfgenResult};
use crate::loader::load_from_uses;
use crate::output::jsonl::parse_gen_event_line;
use crate::wfg_parser::parse_wfg;

use crate::cmd_helpers::load_ws_files;
use crate::tcp_send::{connect_sender, send_events_with_stream};

pub async fn run(
    scenario: PathBuf,
    input: PathBuf,
    addr: String,
    ws: Vec<PathBuf>,
    chunk: Option<usize>,
    rate_ms: Option<u64>,
) -> WfgenResult<()> {
    let wfg_content = std::fs::read_to_string(&scenario).source_err(
        WfgenReason::Io,
        format!("reading .wfg file: {}", scenario.display()),
    )?;
    let wfg = parse_wfg(&wfg_content)?;

    let (mut schemas, _) = load_from_uses(&wfg, &scenario, &HashMap::new(), false)?;
    schemas.extend(load_ws_files(&ws)?);

    // `-` reads stdin, otherwise a file. Streamed in `chunk`-sized batches over
    // ONE persistent connection, optionally paced by `--rate-ms`.
    let reader: Box<dyn BufRead> = if input == Path::new("-") {
        Box::new(BufReader::new(std::io::stdin()))
    } else {
        let file =
            File::open(&input).source_err(WfgenReason::Io, format!("opening {}", input.display()))?;
        Box::new(BufReader::new(file))
    };

    let mut sink = connect_sender(&addr).await?;
    let mut events: Vec<crate::datagen::stream_gen::GenEvent> = Vec::new();
    let mut total_events = 0usize;
    let mut total_frames = 0usize;

    for line in reader.lines() {
        let line = line.source_err(WfgenReason::Io, format!("reading {}", input.display()))?;
        if let Some(ev) = parse_gen_event_line(&line, &input)? {
            events.push(ev);
        }

        if let Some(n) = chunk {
            if events.len() >= n {
                total_frames += send_events_with_stream(&events, &schemas, &mut sink).await?;
                total_events += events.len();
                events.clear();
                if let Some(ms) = rate_ms {
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                }
            }
        }
    }

    if !events.is_empty() {
        total_frames += send_events_with_stream(&events, &schemas, &mut sink).await?;
        total_events += events.len();
    }

    println!(
        "Sent {} events as {} frame(s) -> {}",
        total_events, total_frames, addr
    );
    Ok(())
}
