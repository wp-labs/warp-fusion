use std::path::PathBuf;

use anyhow::Context;

pub(crate) fn load_ws_files(paths: &[PathBuf]) -> anyhow::Result<Vec<wf_lang::WindowSchema>> {
    let mut schemas = Vec::new();
    for path in paths {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("reading .wfs file: {}", path.display()))?;
        let parsed = wf_lang::parse_wfs(&content)
            .with_context(|| format!("parsing .wfs file: {}", path.display()))?;
        schemas.extend(parsed);
    }
    Ok(schemas)
}

pub(crate) fn load_wfl_files(paths: &[PathBuf]) -> anyhow::Result<Vec<wf_lang::ast::WflFile>> {
    let mut files = Vec::new();
    for path in paths {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("reading .wfl file: {}", path.display()))?;
        let parsed = wf_lang::parse_wfl(&content)
            .with_context(|| format!("parsing .wfl file: {}", path.display()))?;
        files.push(parsed);
    }
    Ok(files)
}
