use std::fmt::Display;

use orion_error::conversion::{ConvErr, ToStructError};
use orion_error::{OperationContext, OrionError, StructError, UnifiedReason};
use wf_engine::error::CoreReason;

#[derive(Debug, Clone, PartialEq, OrionError)]
pub enum WfgenReason {
    #[orion_error(message = "scenario parse error", identity = "logic.wfgen.parse")]
    Parse,
    #[orion_error(
        message = "scenario validation error",
        identity = "logic.wfgen.validation"
    )]
    Validation,
    #[orion_error(message = "generation error", identity = "logic.wfgen.generation")]
    Generation,
    #[orion_error(message = "oracle error", identity = "logic.wfgen.oracle")]
    Oracle,
    #[orion_error(message = "input/output error", identity = "sys.wfgen.io")]
    Io,
    #[orion_error(message = "serialization error", identity = "sys.wfgen.serialization")]
    Serialization,
    #[orion_error(message = "network send error", identity = "sys.wfgen.network")]
    Network,
    #[orion_error(transparent)]
    Lang(wf_lang::LangReason),
    #[orion_error(transparent)]
    Config(wf_config::ConfigReason),
    #[orion_error(transparent)]
    Core(CoreReason),
    #[orion_error(transparent)]
    Vars(wf_config::VarsReason),
    #[orion_error(transparent)]
    General(UnifiedReason),
}

impl From<wf_lang::LangReason> for WfgenReason {
    fn from(reason: wf_lang::LangReason) -> Self {
        Self::Lang(reason)
    }
}

impl From<wf_config::ConfigReason> for WfgenReason {
    fn from(reason: wf_config::ConfigReason) -> Self {
        Self::Config(reason)
    }
}

impl From<CoreReason> for WfgenReason {
    fn from(reason: CoreReason) -> Self {
        Self::Core(reason)
    }
}

impl From<wf_config::VarsReason> for WfgenReason {
    fn from(reason: wf_config::VarsReason) -> Self {
        Self::Vars(reason)
    }
}

pub type WfgenError = StructError<WfgenReason>;
pub type WfgenResult<T> = Result<T, WfgenError>;

pub trait WfgenStructExt<T> {
    fn wfgen(self) -> WfgenResult<T>;
}

impl<T, R> WfgenStructExt<T> for Result<T, StructError<R>>
where
    R: orion_error::reason::DomainReason,
    WfgenReason: From<R>,
{
    fn wfgen(self) -> WfgenResult<T> {
        self.conv_err()
    }
}

pub fn fail<T>(reason: WfgenReason, detail: impl Into<String>) -> WfgenResult<T> {
    Err(reason.to_err().with_detail(detail))
}

pub fn error(reason: WfgenReason, detail: impl Into<String>) -> WfgenError {
    reason.to_err().with_detail(detail)
}

pub fn error_at(
    reason: WfgenReason,
    detail: impl Into<String>,
    action: impl Into<String>,
    path: impl Display,
) -> WfgenError {
    reason
        .to_err()
        .with_detail(detail)
        .with_context(OperationContext::doing(action).with_field("path", path))
}
