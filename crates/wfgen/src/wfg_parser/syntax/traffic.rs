use winnow::combinator::{alt, cut_err, opt};
use winnow::error::{StrContext, StrContextValue};
use winnow::prelude::*;
use winnow::token::literal;

use wf_lang::parse_utils::ident;

use crate::wfg_ast::*;
use crate::wfg_parser::primitives::{rate, ws_skip};
pub(crate) fn parse_traffic_block(input: &mut &str) -> ModalResult<TrafficBlock> {
    ws_skip(input)?;
    cut_err(literal("{"))
        .context(StrContext::Expected(StrContextValue::Description(
            "opening brace for traffic block",
        )))
        .parse_next(input)?;

    let mut streams = Vec::new();
    loop {
        ws_skip(input)?;
        if opt(literal("}")).parse_next(input)?.is_some() {
            break;
        }
        cut_err(wf_lang::parse_utils::kw("stream"))
            .context(StrContext::Expected(StrContextValue::Description(
                "'stream' in traffic block",
            )))
            .parse_next(input)?;
        ws_skip(input)?;
        let stream = cut_err(ident)
            .context(StrContext::Expected(StrContextValue::Description(
                "stream name",
            )))
            .parse_next(input)?
            .to_string();
        ws_skip(input)?;
        cut_err(wf_lang::parse_utils::kw("gen"))
            .context(StrContext::Expected(StrContextValue::Description(
                "'gen' keyword",
            )))
            .parse_next(input)?;
        ws_skip(input)?;
        let rate_expr = cut_err(parse_rate_expr)
            .context(StrContext::Expected(StrContextValue::Description(
                "rate expression",
            )))
            .parse_next(input)?;
        ws_skip(input)?;
        let _ = opt(literal(";")).parse_next(input)?;

        streams.push(SyntaxStreamDecl {
            stream,
            rate: rate_expr,
        });
    }

    Ok(TrafficBlock { streams })
}

fn parse_rate_expr(input: &mut &str) -> ModalResult<RateExpr> {
    if opt(wf_lang::parse_utils::kw("wave"))
        .parse_next(input)?
        .is_some()
    {
        return parse_wave(input);
    }
    if opt(wf_lang::parse_utils::kw("burst"))
        .parse_next(input)?
        .is_some()
    {
        return parse_burst(input);
    }
    if opt(wf_lang::parse_utils::kw("timeline"))
        .parse_next(input)?
        .is_some()
    {
        return parse_timeline(input);
    }
    Ok(RateExpr::Constant(rate(input)?))
}

fn parse_wave(input: &mut &str) -> ModalResult<RateExpr> {
    ws_skip(input)?;
    cut_err(literal("(")).parse_next(input)?;
    ws_skip(input)?;
    cut_err(wf_lang::parse_utils::kw("base")).parse_next(input)?;
    cut_err(literal("=")).parse_next(input)?;
    let base = cut_err(rate).parse_next(input)?;
    ws_skip(input)?;
    cut_err(literal(",")).parse_next(input)?;
    ws_skip(input)?;
    cut_err(wf_lang::parse_utils::kw("amp")).parse_next(input)?;
    cut_err(literal("=")).parse_next(input)?;
    let amp = cut_err(rate).parse_next(input)?;
    ws_skip(input)?;
    cut_err(literal(",")).parse_next(input)?;
    ws_skip(input)?;
    cut_err(wf_lang::parse_utils::kw("period")).parse_next(input)?;
    cut_err(literal("=")).parse_next(input)?;
    let period = cut_err(wf_lang::parse_utils::duration_value).parse_next(input)?;

    let mut shape = WaveShape::Sine;
    ws_skip(input)?;
    if opt(literal(",")).parse_next(input)?.is_some() {
        ws_skip(input)?;
        cut_err(wf_lang::parse_utils::kw("shape")).parse_next(input)?;
        cut_err(literal("=")).parse_next(input)?;
        shape = cut_err(alt((
            wf_lang::parse_utils::kw("sine").value(WaveShape::Sine),
            wf_lang::parse_utils::kw("triangle").value(WaveShape::Triangle),
            wf_lang::parse_utils::kw("square").value(WaveShape::Square),
        )))
        .parse_next(input)?;
    }
    ws_skip(input)?;
    cut_err(literal(")")).parse_next(input)?;

    Ok(RateExpr::Wave {
        base,
        amp,
        period,
        shape,
    })
}

fn parse_burst(input: &mut &str) -> ModalResult<RateExpr> {
    ws_skip(input)?;
    cut_err(literal("(")).parse_next(input)?;
    ws_skip(input)?;
    cut_err(wf_lang::parse_utils::kw("base")).parse_next(input)?;
    cut_err(literal("=")).parse_next(input)?;
    let base = cut_err(rate).parse_next(input)?;
    ws_skip(input)?;
    cut_err(literal(",")).parse_next(input)?;
    ws_skip(input)?;
    cut_err(wf_lang::parse_utils::kw("peak")).parse_next(input)?;
    cut_err(literal("=")).parse_next(input)?;
    let peak = cut_err(rate).parse_next(input)?;
    ws_skip(input)?;
    cut_err(literal(",")).parse_next(input)?;
    ws_skip(input)?;
    cut_err(wf_lang::parse_utils::kw("every")).parse_next(input)?;
    cut_err(literal("=")).parse_next(input)?;
    let every = cut_err(wf_lang::parse_utils::duration_value).parse_next(input)?;
    ws_skip(input)?;
    cut_err(literal(",")).parse_next(input)?;
    ws_skip(input)?;
    cut_err(wf_lang::parse_utils::kw("hold")).parse_next(input)?;
    cut_err(literal("=")).parse_next(input)?;
    let hold = cut_err(wf_lang::parse_utils::duration_value).parse_next(input)?;
    ws_skip(input)?;
    cut_err(literal(")")).parse_next(input)?;

    Ok(RateExpr::Burst {
        base,
        peak,
        every,
        hold,
    })
}

fn parse_timeline(input: &mut &str) -> ModalResult<RateExpr> {
    ws_skip(input)?;
    cut_err(literal("{")).parse_next(input)?;
    let mut segments = Vec::new();
    loop {
        ws_skip(input)?;
        if opt(literal("}")).parse_next(input)?.is_some() {
            break;
        }
        let start = cut_err(wf_lang::parse_utils::duration_value).parse_next(input)?;
        ws_skip(input)?;
        cut_err(literal("..")).parse_next(input)?;
        ws_skip(input)?;
        let end = cut_err(wf_lang::parse_utils::duration_value).parse_next(input)?;
        ws_skip(input)?;
        cut_err(literal("=")).parse_next(input)?;
        ws_skip(input)?;
        let seg_rate = cut_err(rate).parse_next(input)?;
        ws_skip(input)?;
        let _ = opt(literal(";")).parse_next(input)?;
        segments.push(TimelineSegment {
            start,
            end,
            rate: seg_rate,
        });
    }
    Ok(RateExpr::Timeline(segments))
}
