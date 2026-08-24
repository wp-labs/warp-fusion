use winnow::combinator::{alt, cut_err, opt};
use winnow::error::{StrContext, StrContextValue};
use winnow::prelude::*;
use winnow::token::literal;

use wf_lang::parse_utils::{ident, number_literal};

use crate::wfg_ast::*;
use crate::wfg_parser::primitives::{percent, ws_skip};
pub(crate) fn parse_expect_block(input: &mut &str) -> ModalResult<ExpectBlock> {
    ws_skip(input)?;
    cut_err(literal("{")).parse_next(input)?;
    let mut checks = Vec::new();
    loop {
        ws_skip(input)?;
        if opt(literal("}")).parse_next(input)?.is_some() {
            break;
        }
        checks.push(parse_expect_stmt(input)?);
    }
    Ok(ExpectBlock { checks })
}

fn parse_expect_stmt(input: &mut &str) -> ModalResult<ExpectCheck> {
    let metric = alt((
        wf_lang::parse_utils::kw("hit").value(ExpectMetric::Hit),
        wf_lang::parse_utils::kw("near_miss").value(ExpectMetric::NearMiss),
        wf_lang::parse_utils::kw("miss").value(ExpectMetric::Miss),
        wf_lang::parse_utils::kw("precision").value(ExpectMetric::Precision),
        wf_lang::parse_utils::kw("recall").value(ExpectMetric::Recall),
        wf_lang::parse_utils::kw("fpr").value(ExpectMetric::Fpr),
        wf_lang::parse_utils::kw("latency_p95").value(ExpectMetric::LatencyP95),
    ))
    .parse_next(input)?;
    ws_skip(input)?;
    cut_err(literal("(")).parse_next(input)?;
    ws_skip(input)?;
    let rule = cut_err(ident)
        .context(StrContext::Expected(StrContextValue::Description(
            "rule name in expect expression",
        )))
        .parse_next(input)?
        .to_string();
    ws_skip(input)?;
    cut_err(literal(")")).parse_next(input)?;
    ws_skip(input)?;
    let op = parse_compare_op(input)?;
    ws_skip(input)?;
    let value = parse_expect_value(input)?;
    ws_skip(input)?;
    let _ = opt(literal(";")).parse_next(input)?;
    Ok(ExpectCheck {
        metric,
        rule,
        op,
        value,
    })
}

fn parse_compare_op(input: &mut &str) -> ModalResult<CompareOp> {
    alt((
        literal(">=").value(CompareOp::Gte),
        literal("<=").value(CompareOp::Lte),
        literal("==").value(CompareOp::Eq),
        literal(">").value(CompareOp::Gt),
        literal("<").value(CompareOp::Lt),
    ))
    .parse_next(input)
}

fn parse_expect_value(input: &mut &str) -> ModalResult<ExpectValue> {
    let percent_saved = *input;
    if let Ok(v) = percent.parse_next(input) {
        return Ok(ExpectValue::Percent(v));
    }
    *input = percent_saved;

    let duration_saved = *input;
    if let Ok(d) = wf_lang::parse_utils::duration_value.parse_next(input) {
        return Ok(ExpectValue::Duration(d));
    }
    *input = duration_saved;

    let n = number_literal(input)?;
    Ok(ExpectValue::Number(n))
}
