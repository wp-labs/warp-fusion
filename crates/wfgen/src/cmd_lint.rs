use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Context;

use wfgen::loader::load_from_uses;
use wfgen::validate::validate_wfg;
use wfgen::wfg_parser::parse_wfg;

use crate::cmd_helpers::{load_wfl_files, load_ws_files};

pub(crate) fn run(scenario: PathBuf, ws: Vec<PathBuf>, wfl: Vec<PathBuf>) -> anyhow::Result<()> {
    let wfg_content = std::fs::read_to_string(&scenario).context("reading .wfg file")?;
    let wfg = parse_wfg(&wfg_content).context("parsing .wfg file")?;

    let (mut schemas, mut wfl_files) = load_from_uses(&wfg, &scenario, &HashMap::new())?;
    schemas.extend(load_ws_files(&ws)?);
    wfl_files.extend(load_wfl_files(&wfl)?);

    let errors = validate_wfg(&wfg, &schemas, &wfl_files);
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
