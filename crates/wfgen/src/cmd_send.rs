use std::collections::HashMap;
use std::path::PathBuf;

use orion_error::conversion::SourceErr;

use wfgen::error::{WfgenReason, WfgenResult};
use wfgen::loader::load_from_uses;
use wfgen::output::jsonl::read_events_jsonl;
use wfgen::wfg_parser::parse_wfg;

use crate::cmd_helpers::load_ws_files;
use crate::tcp_send::send_events;

pub(crate) fn run(
    scenario: PathBuf,
    input: PathBuf,
    addr: String,
    ws: Vec<PathBuf>,
) -> WfgenResult<()> {
    let wfg_content = std::fs::read_to_string(&scenario).source_err(
        WfgenReason::Io,
        format!("reading .wfg file: {}", scenario.display()),
    )?;
    let wfg = parse_wfg(&wfg_content)?;

    let (mut schemas, _) = load_from_uses(&wfg, &scenario, &HashMap::new())?;
    schemas.extend(load_ws_files(&ws)?);

    let events = read_events_jsonl(&input)?;
    let sent_frames = send_events(&events, &schemas, &addr)?;

    println!(
        "Sent {} events as {} frame(s) -> {}",
        events.len(),
        sent_frames,
        addr
    );
    Ok(())
}
