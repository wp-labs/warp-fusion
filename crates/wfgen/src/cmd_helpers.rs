use std::path::PathBuf;

use orion_error::conversion::SourceErr;

use crate::error::{WfgenReason, WfgenResult, WfgenStructExt};
use crate::prelude;

pub fn load_ws_files(paths: &[PathBuf]) -> WfgenResult<Vec<wf_lang::WindowSchema>> {
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

pub fn load_wfl_files(paths: &[PathBuf]) -> WfgenResult<Vec<wf_lang::ast::WflFile>> {
    let mut files = Vec::new();
    for path in paths {
        let content = std::fs::read_to_string(path).source_err(
            WfgenReason::Io,
            format!("reading .wfl file: {}", path.display()),
        )?;
        let mut parsed = wf_lang::parse_wfl(&content).wfgen()?;
        // Merge `_global.wfl` yield presets next to the rule file, matching
        // wf-runtime's project prelude convention. Skip when the file being
        // loaded is the prelude itself.
        if let Some(prelude_path) = prelude::prelude_path_for(path)
            && !prelude::is_prelude_file(path, &prelude_path)
        {
            let prelude_source = std::fs::read_to_string(&prelude_path).source_err(
                WfgenReason::Io,
                format!("reading rule prelude: {}", prelude_path.display()),
            )?;
            let prelude = prelude::parse_rule_prelude(&prelude_source, &prelude_path)?;
            prelude::validate_rule_prelude_conflicts(&parsed, path, &prelude)?;
            prelude::apply_rule_prelude(&mut parsed, &prelude);
        }
        // issue #73: `use "file.wfl"` 导入顶层列表（include 语义, 递归/循环/重名报错）。
        parsed = wf_lang::compiler::lists::resolve_imports(&parsed, path, &mut |import_path| {
            std::fs::read_to_string(import_path)
                .source_err(
                    WfgenReason::Io,
                    format!("reading imported wfl: {}", import_path.display()),
                )
                .map_err(|e| {
                    wf_lang::error::error(
                        wf_lang::LangReason::Compile,
                        e.detail().clone().unwrap_or_else(|| e.to_string()),
                    )
                })
        })
        .wfgen()?;
        files.push(parsed);
    }
    Ok(files)
}
