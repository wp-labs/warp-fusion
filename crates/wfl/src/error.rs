use std::fmt::Display;

use orion_error::conversion::{ConvErr, ToStructError};
use orion_error::{OperationContext, OrionError, StructError, UnifiedReason};
use wf_engine::error::CoreReason;

#[derive(Debug, Clone, PartialEq, OrionError)]
pub enum WflReason {
    #[orion_error(message = "formatting error", identity = "logic.wfl.format")]
    Format,
    #[orion_error(message = "rule parse error", identity = "logic.wfl.parse")]
    Parse,
    #[orion_error(message = "rule validation error", identity = "logic.wfl.validation")]
    Validation,
    #[orion_error(message = "rule replay error", identity = "logic.wfl.replay")]
    Replay,
    #[orion_error(message = "verification error", identity = "logic.wfl.verify")]
    Verify,
    #[orion_error(message = "input/output error", identity = "sys.wfl.io")]
    Io,
    #[orion_error(message = "serialization error", identity = "sys.wfl.serialization")]
    Serialization,
    #[orion_error(transparent)]
    Lang(wf_lang::LangReason),
    #[orion_error(transparent)]
    Config(wf_config::ConfigReason),
    #[orion_error(transparent)]
    Core(CoreReason),
    #[orion_error(transparent)]
    Wfgen(wfgen::error::WfgenReason),
    #[orion_error(transparent)]
    General(UnifiedReason),
}

impl From<wf_lang::LangReason> for WflReason {
    fn from(reason: wf_lang::LangReason) -> Self {
        Self::Lang(reason)
    }
}

impl From<wf_config::ConfigReason> for WflReason {
    fn from(reason: wf_config::ConfigReason) -> Self {
        Self::Config(reason)
    }
}

impl From<CoreReason> for WflReason {
    fn from(reason: CoreReason) -> Self {
        Self::Core(reason)
    }
}

impl From<wfgen::error::WfgenReason> for WflReason {
    fn from(reason: wfgen::error::WfgenReason) -> Self {
        Self::Wfgen(reason)
    }
}

pub type WflError = StructError<WflReason>;
pub type WflResult<T> = Result<T, WflError>;

pub trait WflStructExt<T> {
    fn wfl(self) -> WflResult<T>;
}

impl<T, R> WflStructExt<T> for Result<T, StructError<R>>
where
    R: orion_error::reason::DomainReason,
    WflReason: From<R>,
{
    fn wfl(self) -> WflResult<T> {
        self.conv_err()
    }
}

pub fn fail<T>(reason: WflReason, detail: impl Into<String>) -> WflResult<T> {
    Err(reason.to_err().with_detail(detail))
}

pub fn error(reason: WflReason, detail: impl Into<String>) -> WflError {
    reason.to_err().with_detail(detail)
}

pub fn error_at(
    reason: WflReason,
    detail: impl Into<String>,
    action: impl Into<String>,
    path: impl Display,
) -> WflError {
    reason
        .to_err()
        .with_detail(detail)
        .with_context(OperationContext::doing(action).with_field("path", path))
}
