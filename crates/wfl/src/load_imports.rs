//! wfl crate 公共加载辅助: `use "file.wfl"` 导入顶层列表（issue #73）。

use std::path::Path;

use orion_error::conversion::ConvErr;

use crate::error::{WflResult, WflStructExt};

/// 加载 + 预处理 + 解析规则文件, 并解析 `use "file.wfl"` 导入（include 语义:
/// 目标文件全部顶层列表并入当前作用域; 递归传播; 循环/缺失/重名报错）。
///
/// 四个命令（lint/test/replay/explain）共用——use 解析与 compile 同一条路径,
/// 不绕过未知名/循环/重名错误。
pub fn load_wfl_with_imports(
    file: &Path,
    ctx: &wf_config::ConfigVarContext,
    cwd: &Path,
) -> WflResult<wf_lang::ast::WflFile> {
    let source = wf_config::project::load_wfl_with_context(file, ctx, Some(cwd)).wfl()?;
    let parsed = wf_lang::parse_wfl(&source).wfl()?;
    wf_lang::compiler::lists::resolve_imports(&parsed, file, &mut |import_path| {
        wf_config::project::load_wfl_with_context(import_path, ctx, Some(cwd)).map_err(|e| {
            wf_lang::error::error(
                wf_lang::LangReason::Compile,
                e.detail().clone().unwrap_or_else(|| e.to_string()),
            )
        })
    })
    .conv_err()
}
