use std::collections::HashMap;
use std::path::PathBuf;

use orion_error::conversion::SourceErr;

use crate::error::{WfgenReason, WfgenResult};
use crate::loader::load_from_uses;
use crate::validate::validate_wfg;
use crate::wfg_parser::parse_wfg;

use crate::cmd_helpers::{load_wfl_files, load_ws_files};

/// `wfgen lint` 参数：校验 .wfg scenario 文件。
#[derive(clap::Args)]
pub struct Args {
    /// Path to the .wfg scenario file
    pub scenario: PathBuf,

    /// Additional .wfs schema files (beyond those in `use` declarations)
    #[arg(long)]
    pub ws: Vec<PathBuf>,

    /// Additional .wfl rule files (beyond those in `use` declarations)
    #[arg(long)]
    pub wfl: Vec<PathBuf>,
}

pub fn run(args: Args) -> WfgenResult<()> {
    let scenario = args.scenario;
    let ws = args.ws;
    let wfl = args.wfl;
    let wfg_content = std::fs::read_to_string(&scenario).source_err(
        WfgenReason::Io,
        format!("reading .wfg file: {}", scenario.display()),
    )?;
    let wfg = parse_wfg(&wfg_content)?;

    let (mut schemas, mut wfl_files) = load_from_uses(&wfg, &scenario, &HashMap::new(), false)?;
    schemas.extend(load_ws_files(&ws)?);
    wfl_files.extend(load_wfl_files(&wfl)?);

    let errors = validate_wfg(&wfg, &schemas, &wfl_files, false);
    if errors.is_empty() {
        println!("OK");
    } else {
        for e in &errors {
            eprintln!("{}", e);
        }
        std::process::exit(1);
    }
    Ok(())
}
