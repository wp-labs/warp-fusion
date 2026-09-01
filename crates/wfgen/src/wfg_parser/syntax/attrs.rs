use winnow::combinator::{cut_err, opt};
use winnow::error::{StrContext, StrContextValue};
use winnow::prelude::*;
use winnow::token::literal;

use wf_lang::parse_utils::{ident, number_literal};

use crate::wfg_ast::*;
use crate::wfg_parser::primitives::ws_skip;
pub(crate) fn scenario_attrs(input: &mut &str) -> ModalResult<Vec<ScenarioAttr>> {
    ws_skip(input)?;
    cut_err(literal("#["))
        .context(StrContext::Expected(StrContextValue::Description(
            "scenario annotation '#['",
        )))
        .parse_next(input)?;
    let attrs = parse_attr_list(input, "]")?;
    cut_err(literal("]"))
        .context(StrContext::Expected(StrContextValue::Description(
            "closing ']' for scenario annotation",
        )))
        .parse_next(input)?;
    Ok(attrs)
}

pub(crate) fn inline_annos(input: &mut &str) -> ModalResult<Vec<ScenarioAttr>> {
    ws_skip(input)?;
    cut_err(literal("<"))
        .context(StrContext::Expected(StrContextValue::Description(
            "opening '<' for inline annotations",
        )))
        .parse_next(input)?;
    let attrs = parse_attr_list(input, ">")?;
    cut_err(literal(">"))
        .context(StrContext::Expected(StrContextValue::Description(
            "closing '>' for inline annotations",
        )))
        .parse_next(input)?;
    Ok(attrs)
}

fn parse_attr_list(input: &mut &str, end_delim: &str) -> ModalResult<Vec<ScenarioAttr>> {
    let mut attrs = Vec::new();
    ws_skip(input)?;
    if input.starts_with(end_delim) {
        return Ok(attrs);
    }

    attrs.push(parse_attr(input)?);
    loop {
        ws_skip(input)?;
        if opt(literal(",")).parse_next(input)?.is_some() {
            ws_skip(input)?;
            attrs.push(parse_attr(input)?);
        } else {
            break;
        }
    }
    Ok(attrs)
}

fn parse_attr(input: &mut &str) -> ModalResult<ScenarioAttr> {
    let key = ident(input)?.to_string();
    ws_skip(input)?;
    cut_err(literal("="))
        .context(StrContext::Expected(StrContextValue::Description(
            "'=' in annotation",
        )))
        .parse_next(input)?;
    ws_skip(input)?;
    let value = parse_attr_value(input)?;
    Ok(ScenarioAttr { key, value })
}

pub(crate) fn parse_attr_value(input: &mut &str) -> ModalResult<AttrValue> {
    if let Some(s) = opt(wf_lang::parse_utils::quoted_string).parse_next(input)? {
        return Ok(AttrValue::String(s));
    }

    // Duration is parsed before bare number to avoid consuming `10m` as `10`.
    let duration_saved = *input;
    if let Ok(d) = wf_lang::parse_utils::duration_value.parse_next(input) {
        return Ok(AttrValue::Duration(d));
    }
    *input = duration_saved;

    let number_saved = *input;
    if let Ok(n) = number_literal.parse_next(input) {
        return Ok(AttrValue::Number(n));
    }
    *input = number_saved;

    let word = ident(input)?.to_string();
    match word.as_str() {
        "true" => Ok(AttrValue::Bool(true)),
        "false" => Ok(AttrValue::Bool(false)),
        _ => Ok(AttrValue::String(word)),
    }
}
