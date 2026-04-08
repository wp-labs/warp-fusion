use winnow::combinator::cut_err;
use winnow::error::{StrContext, StrContextValue};
use winnow::prelude::*;

use wf_lang::parse_utils::ident;

use crate::wfg_ast::*;

use super::primitives::ws_skip;
use super::syntax::{inline_annos, parse_syntax_body, scenario_attrs};

pub(super) struct ParsedScenario {
    pub scenario: ScenarioDecl,
    pub syntax: Option<SyntaxScenario>,
}

// ---------------------------------------------------------------------------
// Scenario (new syntax only)
// ---------------------------------------------------------------------------

pub(super) fn scenario_decl(input: &mut &str) -> ModalResult<ParsedScenario> {
    ws_skip(input)?;
    let attrs = if input.starts_with("#[") {
        scenario_attrs(input)?
    } else {
        Vec::new()
    };

    ws_skip(input)?;
    wf_lang::parse_utils::kw("scenario").parse_next(input)?;
    ws_skip(input)?;
    let name = cut_err(ident)
        .context(StrContext::Expected(StrContextValue::Description(
            "scenario name",
        )))
        .parse_next(input)?
        .to_string();
    ws_skip(input)?;

    let inline = if input.starts_with('<') {
        inline_annos(input)?
    } else {
        Vec::new()
    };

    let (scenario, syntax) = parse_syntax_body(input, name, attrs, inline)?;
    Ok(ParsedScenario {
        scenario,
        syntax: Some(syntax),
    })
}
