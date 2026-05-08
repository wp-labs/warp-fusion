use std::path::PathBuf;

use orion_error::conversion::SourceErr;

use wfgen::error::{WfgenReason, WfgenResult, WfgenStructExt};

pub(crate) fn load_ws_files(paths: &[PathBuf]) -> WfgenResult<Vec<wf_lang::WindowSchema>> {
    let mut schemas = Vec::new();
    for path in paths {
        let content = std::fs::read_to_string(path).source_err(
            WfgenReason::Io,
            format!("reading .wfs file: {}", path.display()),
        )?;
        let parsed = wf_lang::parse_wfs(&content).wfgen()?;
        schemas.extend(parsed);
    }
    Ok(schemas)
}

pub(crate) fn load_wfl_files(paths: &[PathBuf]) -> WfgenResult<Vec<wf_lang::ast::WflFile>> {
    let mut files = Vec::new();
    for path in paths {
        let content = std::fs::read_to_string(path).source_err(
            WfgenReason::Io,
            format!("reading .wfl file: {}", path.display()),
        )?;
        let parsed = wf_lang::parse_wfl(&content).wfgen()?;
        files.push(parsed);
    }
    Ok(files)
}
