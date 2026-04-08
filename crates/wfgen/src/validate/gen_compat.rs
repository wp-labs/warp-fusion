use wf_lang::BaseType;

use crate::wfg_ast::GenExpr;

/// Check whether a `GenExpr` is type-compatible with a field's `BaseType`.
///
/// Returns `None` if compatible, or `Some(reason)` if not.
pub(super) fn check_gen_expr_compat(expr: &GenExpr, base: &BaseType) -> Option<String> {
    match expr {
        GenExpr::StringLit(_) => match base {
            BaseType::Chars | BaseType::Ip | BaseType::Hex | BaseType::Time => None,
            _ => Some(format!("string literal not compatible with {:?}", base)),
        },
        GenExpr::NumberLit(_) => match base {
            BaseType::Digit | BaseType::Float => None,
            _ => Some(format!("number literal not compatible with {:?}", base)),
        },
        GenExpr::BoolLit(_) => match base {
            BaseType::Bool => None,
            _ => Some(format!("boolean literal not compatible with {:?}", base)),
        },
        GenExpr::GenFunc { name, .. } => check_gen_func_compat(name, base),
    }
}

/// Check whether a known gen function is compatible with a field's `BaseType`.
pub(super) fn check_gen_func_compat(func_name: &str, base: &BaseType) -> Option<String> {
    match func_name {
        "ipv4" => match base {
            BaseType::Ip | BaseType::Chars => None,
            _ => Some(format!(
                "ipv4() produces IP addresses, not compatible with {:?}",
                base
            )),
        },
        "pattern" => match base {
            BaseType::Chars | BaseType::Ip | BaseType::Hex => None,
            _ => Some(format!(
                "pattern() produces strings, not compatible with {:?}",
                base
            )),
        },
        "range" => match base {
            BaseType::Digit | BaseType::Float => None,
            _ => Some(format!(
                "range() produces numbers, not compatible with {:?}",
                base
            )),
        },
        "timestamp" => match base {
            BaseType::Time | BaseType::Chars => None,
            _ => Some(format!(
                "timestamp() produces time values, not compatible with {:?}",
                base
            )),
        },
        // `enum` is a generic selector -- compatible with any type
        "enum" => None,
        // Unknown gen functions -- skip validation (user-extensible)
        _ => None,
    }
}
