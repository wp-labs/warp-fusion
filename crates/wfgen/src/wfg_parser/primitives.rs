use winnow::ascii::multispace0;
use winnow::combinator::opt;
use winnow::prelude::*;
use winnow::token::{literal, take_while};

use crate::wfg_ast::{Rate, RateUnit};

// ---------------------------------------------------------------------------
// Whitespace & comments (// style for .wfg)
// ---------------------------------------------------------------------------

/// Skip whitespace and `// ...` line comments.
pub fn ws_skip(input: &mut &str) -> ModalResult<()> {
    loop {
        let _ = multispace0.parse_next(input)?;
        if opt(literal("//")).parse_next(input)?.is_some() {
            let _ = take_while(0.., |c: char| c != '\n').parse_next(input)?;
        } else {
            break;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Rate: NUMBER "/" ("s"|"m"|"h")
// ---------------------------------------------------------------------------

pub fn rate(input: &mut &str) -> ModalResult<Rate> {
    let num = wf_lang::parse_utils::number_literal(input)?;
    let count = num as u64;
    literal("/").parse_next(input)?;
    let unit = winnow::combinator::alt((
        literal("s").value(RateUnit::PerSecond),
        literal("m").value(RateUnit::PerMinute),
        literal("h").value(RateUnit::PerHour),
    ))
    .parse_next(input)?;
    Ok(Rate { count, unit })
}

// ---------------------------------------------------------------------------
// Percent: NUMBER "%"
// ---------------------------------------------------------------------------

pub fn percent(input: &mut &str) -> ModalResult<f64> {
    let num = wf_lang::parse_utils::number_literal(input)?;
    literal("%").parse_next(input)?;
    Ok(num)
}
