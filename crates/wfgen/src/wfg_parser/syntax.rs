use std::time::Duration;

use winnow::combinator::{alt, cut_err, opt};
use winnow::error::{AddContext, StrContext, StrContextValue};
use winnow::prelude::*;
use winnow::token::literal;

use wf_lang::parse_utils::{ident, number_literal};

use crate::wfg_ast::*;

use super::primitives::{percent, rate, ws_skip};

pub(super) fn scenario_attrs(input: &mut &str) -> ModalResult<Vec<ScenarioAttr>> {
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

pub(super) fn inline_annos(input: &mut &str) -> ModalResult<Vec<ScenarioAttr>> {
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

fn parse_attr_value(input: &mut &str) -> ModalResult<AttrValue> {
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

pub(super) fn parse_syntax_body(
    input: &mut &str,
    name: String,
    attrs: Vec<ScenarioAttr>,
    inline_annos: Vec<ScenarioAttr>,
) -> ModalResult<(ScenarioDecl, SyntaxScenario)> {
    ws_skip(input)?;
    cut_err(literal("{"))
        .context(StrContext::Expected(StrContextValue::Description(
            "opening brace for scenario body",
        )))
        .parse_next(input)?;

    let mut traffic: Option<TrafficBlock> = None;
    let mut injection: Option<SyntaxInjectionBlock> = None;
    let mut expect: Option<ExpectBlock> = None;

    loop {
        ws_skip(input)?;
        if opt(literal("}")).parse_next(input)?.is_some() {
            break;
        }

        if opt(wf_lang::parse_utils::kw("traffic"))
            .parse_next(input)?
            .is_some()
        {
            traffic = Some(parse_traffic_block(input)?);
            continue;
        }
        if opt(wf_lang::parse_utils::kw("injection"))
            .parse_next(input)?
            .is_some()
        {
            injection = Some(parse_injection_block(input)?);
            continue;
        }
        if opt(wf_lang::parse_utils::kw("expect"))
            .parse_next(input)?
            .is_some()
        {
            expect = Some(parse_expect_block(input)?);
            continue;
        }

        return Err(winnow::error::ErrMode::Cut(
            winnow::error::ContextError::new().add_context(
                input,
                &input.checkpoint(),
                StrContext::Expected(StrContextValue::Description(
                    "traffic, injection, expect, or closing brace",
                )),
            ),
        ));
    }

    let Some(traffic) = traffic else {
        return Err(winnow::error::ErrMode::Cut(
            winnow::error::ContextError::new().add_context(
                input,
                &input.checkpoint(),
                StrContext::Expected(StrContextValue::Description("traffic block")),
            ),
        ));
    };

    let seed = extract_seed(&inline_annos).unwrap_or(0);
    let duration = extract_duration(&attrs).unwrap_or_else(|| Duration::from_secs(60));
    let total = derive_total(&traffic, duration);
    let streams = derive_legacy_streams(&traffic);
    let injects = derive_legacy_injects(injection.as_ref(), expect.as_ref());

    let scenario = ScenarioDecl {
        name,
        seed,
        time_clause: TimeClause {
            start: "1970-01-01T00:00:00Z".to_string(),
            duration,
        },
        total,
        streams,
        injects,
        faults: None,
        oracle: None,
    };

    let syntax = SyntaxScenario {
        attrs,
        inline_annos,
        traffic,
        injection,
        expect,
    };

    Ok((scenario, syntax))
}

fn parse_traffic_block(input: &mut &str) -> ModalResult<TrafficBlock> {
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

fn parse_injection_block(input: &mut &str) -> ModalResult<SyntaxInjectionBlock> {
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
    let mut then_from_prev = false;
    if opt(wf_lang::parse_utils::kw("then"))
        .parse_next(input)?
        .is_some()
    {
        then_from_prev = true;
        ws_skip(input)?;
    }

    if opt(wf_lang::parse_utils::kw("use"))
        .parse_next(input)?
        .is_some()
    {
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
        cut_err(literal(",")).parse_next(input)?;
        ws_skip(input)?;
        let within = cut_err(wf_lang::parse_utils::duration_value).parse_next(input)?;
        ws_skip(input)?;
        cut_err(literal(")")).parse_next(input)?;
        ws_skip(input)?;
        let _ = opt(literal(";")).parse_next(input)?;
        return Ok(SeqStep::Use {
            then_from_prev,
            predicates,
            count,
            within,
        });
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

fn parse_expect_block(input: &mut &str) -> ModalResult<ExpectBlock> {
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

fn extract_seed(inline_annos: &[ScenarioAttr]) -> Option<u64> {
    inline_annos
        .iter()
        .find(|a| a.key == "seed")
        .and_then(|a| match a.value {
            AttrValue::Number(n) if n >= 0.0 => Some(n as u64),
            _ => None,
        })
}

fn extract_duration(attrs: &[ScenarioAttr]) -> Option<Duration> {
    attrs
        .iter()
        .find(|a| a.key == "duration")
        .and_then(|a| match a.value {
            AttrValue::Duration(d) => Some(d),
            _ => None,
        })
}

fn derive_legacy_streams(traffic: &TrafficBlock) -> Vec<StreamBlock> {
    traffic
        .streams
        .iter()
        .map(|s| StreamBlock {
            alias: s.stream.clone(),
            window: s.stream.clone(),
            rate: rate_from_expr(&s.rate),
            overrides: Vec::new(),
        })
        .collect()
}

fn derive_legacy_injects(
    injection: Option<&SyntaxInjectionBlock>,
    expect: Option<&ExpectBlock>,
) -> Vec<InjectBlock> {
    let Some(inj) = injection else {
        return Vec::new();
    };

    let mut streams = Vec::new();
    let mut lines = Vec::new();

    for case in &inj.cases {
        if !streams.iter().any(|s| s == &case.stream) {
            streams.push(case.stream.clone());
        }

        let mut params = vec![ParamAssign {
            name: "count_per_entity".to_string(),
            value: ParamValue::Number(first_use_count(case.seq.steps.as_slice()) as f64),
        }];
        if let Some(within) = first_use_within(case.seq.steps.as_slice()) {
            params.push(ParamAssign {
                name: "within".to_string(),
                value: ParamValue::Duration(within),
            });
        }
        if matches!(case.mode, InjectCaseMode::NearMiss)
            && let Some(steps_completed) = completed_use_steps(case.seq.steps.as_slice())
        {
            params.push(ParamAssign {
                name: "steps_completed".to_string(),
                value: ParamValue::Number(steps_completed as f64),
            });
        }
        let use_steps = case
            .seq
            .steps
            .iter()
            .filter_map(|step| match step {
                SeqStep::Use {
                    predicates, count, ..
                } => Some(crate::wfg_ast::InjectUseStep {
                    count: *count,
                    predicates: predicates.clone(),
                }),
                SeqStep::Not { .. } => None,
            })
            .collect();

        lines.push(InjectLine {
            mode: match case.mode {
                InjectCaseMode::Hit => InjectMode::Hit,
                InjectCaseMode::NearMiss => InjectMode::NearMiss,
                InjectCaseMode::Miss => InjectMode::NonHit,
            },
            percent: case.percent,
            params,
            use_steps,
        });
    }

    vec![InjectBlock {
        rule: derive_rule_name(expect),
        streams,
        lines,
    }]
}

fn derive_rule_name(expect: Option<&ExpectBlock>) -> String {
    expect
        .and_then(|e| e.checks.first().map(|c| c.rule.clone()))
        .unwrap_or_default()
}

fn first_use_count(steps: &[SeqStep]) -> u64 {
    steps
        .iter()
        .find_map(|s| match s {
            SeqStep::Use { count, .. } => Some(*count),
            SeqStep::Not { .. } => None,
        })
        .unwrap_or(1)
}

fn first_use_within(steps: &[SeqStep]) -> Option<Duration> {
    steps.iter().find_map(|s| match s {
        SeqStep::Use { within, .. } => Some(*within),
        SeqStep::Not { .. } => None,
    })
}

fn completed_use_steps(steps: &[SeqStep]) -> Option<u64> {
    let count = steps
        .iter()
        .filter(|s| matches!(s, SeqStep::Use { .. }))
        .count();
    if count == 0 {
        None
    } else {
        Some(count.saturating_sub(1) as u64)
    }
}

fn rate_from_expr(rate_expr: &RateExpr) -> Rate {
    match rate_expr {
        RateExpr::Constant(r) => r.clone(),
        RateExpr::Wave { base, .. } => base.clone(),
        RateExpr::Burst { base, .. } => base.clone(),
        RateExpr::Timeline(segments) => segments.first().map(|s| s.rate.clone()).unwrap_or(Rate {
            count: 1,
            unit: RateUnit::PerSecond,
        }),
    }
}

fn derive_total(traffic: &TrafficBlock, duration: Duration) -> u64 {
    let eps_sum: f64 = traffic.streams.iter().map(|s| s.rate.approx_eps()).sum();
    if eps_sum <= 0.0 {
        return 1;
    }
    let total = (eps_sum * duration.as_secs_f64()).round() as u64;
    total.max(1)
}
