use winnow::combinator::{alt, cut_err, opt};
use winnow::error::{AddContext, StrContext, StrContextValue};
use winnow::prelude::*;
use winnow::token::literal;

use wf_lang::parse_utils::ident;

use crate::wfg_ast::*;
use crate::wfg_parser::primitives::{percent, ws_skip};

use super::attrs::parse_attr_value;
pub(crate) fn parse_injection_block(input: &mut &str) -> ModalResult<SyntaxInjectionBlock> {
    ws_skip(input)?;
    cut_err(literal("{"))
        .context(StrContext::Expected(StrContextValue::Description(
            "opening brace for injection block",
        )))
        .parse_next(input)?;
    let mut cases = Vec::new();
    loop {
        ws_skip(input)?;
        if opt(literal("}")).parse_next(input)?.is_some() {
            break;
        }
        cases.push(parse_injection_case(input)?);
    }
    Ok(SyntaxInjectionBlock { cases })
}

fn parse_injection_case(input: &mut &str) -> ModalResult<SyntaxInjectCase> {
    let mode = alt((
        wf_lang::parse_utils::kw("hit").value(InjectCaseMode::Hit),
        wf_lang::parse_utils::kw("near_miss").value(InjectCaseMode::NearMiss),
        wf_lang::parse_utils::kw("miss").value(InjectCaseMode::Miss),
    ))
    .context(StrContext::Expected(StrContextValue::Description(
        "injection mode (hit, near_miss, miss)",
    )))
    .parse_next(input)?;
    ws_skip(input)?;
    cut_err(literal("<")).parse_next(input)?;
    let pct = cut_err(percent).parse_next(input)?;
    cut_err(literal(">")).parse_next(input)?;
    ws_skip(input)?;
    let target_rule = if opt(wf_lang::parse_utils::kw("for"))
        .parse_next(input)?
        .is_some()
    {
        ws_skip(input)?;
        Some(
            cut_err(ident)
                .context(StrContext::Expected(StrContextValue::Description(
                    "target rule name in injection case",
                )))
                .parse_next(input)?
                .to_string(),
        )
    } else {
        None
    };
    ws_skip(input)?;
    let stream = cut_err(ident)
        .context(StrContext::Expected(StrContextValue::Description(
            "stream name in injection case",
        )))
        .parse_next(input)?
        .to_string();
    ws_skip(input)?;
    cut_err(literal("{")).parse_next(input)?;
    ws_skip(input)?;
    let seq = cut_err(parse_seq_block).parse_next(input)?;
    ws_skip(input)?;
    cut_err(literal("}")).parse_next(input)?;
    Ok(SyntaxInjectCase {
        mode,
        percent: pct,
        target_rule,
        stream,
        seq,
    })
}

fn parse_seq_block(input: &mut &str) -> ModalResult<SeqBlock> {
    let entity = cut_err(ident)
        .context(StrContext::Expected(StrContextValue::Description(
            "entity key for seq",
        )))
        .parse_next(input)?
        .to_string();
    ws_skip(input)?;
    cut_err(wf_lang::parse_utils::kw("seq"))
        .context(StrContext::Expected(StrContextValue::Description(
            "'seq' keyword",
        )))
        .parse_next(input)?;
    ws_skip(input)?;
    cut_err(literal("{")).parse_next(input)?;
    let mut steps = Vec::new();
    loop {
        ws_skip(input)?;
        if opt(literal("}")).parse_next(input)?.is_some() {
            break;
        }
        steps.push(parse_seq_step(input)?);
    }
    Ok(SeqBlock { entity, steps })
}

fn parse_seq_step(input: &mut &str) -> ModalResult<SeqStep> {
    if opt(wf_lang::parse_utils::kw("then"))
        .parse_next(input)?
        .is_some()
    {
        ws_skip(input)?;
        cut_err(wf_lang::parse_utils::kw("use"))
            .context(StrContext::Expected(StrContextValue::Description(
                "'use' after 'then'",
            )))
            .parse_next(input)?;
        return parse_use_step_after_keyword(input);
    }

    if opt(wf_lang::parse_utils::kw("use"))
        .parse_next(input)?
        .is_some()
    {
        return parse_use_step_after_keyword(input);
    }

    if opt(wf_lang::parse_utils::kw("not"))
        .parse_next(input)?
        .is_some()
    {
        ws_skip(input)?;
        cut_err(literal("(")).parse_next(input)?;
        let predicates = parse_predicates(input)?;
        cut_err(literal(")")).parse_next(input)?;
        ws_skip(input)?;
        cut_err(wf_lang::parse_utils::kw("within")).parse_next(input)?;
        ws_skip(input)?;
        cut_err(literal("(")).parse_next(input)?;
        ws_skip(input)?;
        let within = cut_err(wf_lang::parse_utils::duration_value).parse_next(input)?;
        ws_skip(input)?;
        cut_err(literal(")")).parse_next(input)?;
        ws_skip(input)?;
        let _ = opt(literal(";")).parse_next(input)?;
        return Ok(SeqStep::Not { predicates, within });
    }

    Err(winnow::error::ErrMode::Cut(
        winnow::error::ContextError::new().add_context(
            input,
            &input.checkpoint(),
            StrContext::Expected(StrContextValue::Description(
                "use(...) or not(...) seq step",
            )),
        ),
    ))
}

fn parse_use_step_after_keyword(input: &mut &str) -> ModalResult<SeqStep> {
    ws_skip(input)?;
    cut_err(literal("(")).parse_next(input)?;
    let predicates = parse_predicates(input)?;
    cut_err(literal(")")).parse_next(input)?;
    ws_skip(input)?;
    cut_err(wf_lang::parse_utils::kw("with")).parse_next(input)?;
    ws_skip(input)?;
    cut_err(literal("(")).parse_next(input)?;
    ws_skip(input)?;
    let count = cut_err(wf_lang::parse_utils::nonneg_integer).parse_next(input)? as u64;
    ws_skip(input)?;
    cut_err(literal(")")).parse_next(input)?;
    ws_skip(input)?;
    let _ = opt(literal(";")).parse_next(input)?;
    Ok(SeqStep::Use { predicates, count })
}

fn parse_predicates(input: &mut &str) -> ModalResult<Vec<FieldPredicate>> {
    let mut predicates = Vec::new();
    predicates.push(parse_predicate(input)?);
    loop {
        ws_skip(input)?;
        if opt(literal(",")).parse_next(input)?.is_some() {
            ws_skip(input)?;
            predicates.push(parse_predicate(input)?);
        } else {
            break;
        }
    }
    Ok(predicates)
}

fn parse_predicate(input: &mut &str) -> ModalResult<FieldPredicate> {
    let field = cut_err(ident)
        .context(StrContext::Expected(StrContextValue::Description(
            "predicate field",
        )))
        .parse_next(input)?
        .to_string();
    ws_skip(input)?;
    cut_err(literal("=")).parse_next(input)?;
    ws_skip(input)?;
    let value = parse_attr_value(input)?;
    Ok(FieldPredicate { field, value })
}
