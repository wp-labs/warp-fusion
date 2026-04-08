use rand::Rng;
use rand::rngs::StdRng;
use wf_lang::FieldType;

use crate::wfg_ast::{GenArg, GenExpr};

/// Generate a value for a field based on its type and optional override.
pub fn generate_field_value(
    field_type: &FieldType,
    override_expr: Option<&GenExpr>,
    rng: &mut StdRng,
) -> serde_json::Value {
    match override_expr {
        Some(expr) => generate_from_expr(expr, rng),
        None => generate_default(field_type, rng),
    }
}

/// Generate a default random value for a field type.
fn generate_default(field_type: &FieldType, rng: &mut StdRng) -> serde_json::Value {
    let base = match field_type {
        FieldType::Base(b) => b,
        FieldType::Array(b) => {
            // Generate an array of 1-5 elements
            let len = rng.random_range(1..=5);
            let arr: Vec<serde_json::Value> =
                (0..len).map(|_| generate_default_base(b, rng)).collect();
            return serde_json::Value::Array(arr);
        }
    };
    generate_default_base(base, rng)
}

fn generate_default_base(base: &wf_lang::BaseType, rng: &mut StdRng) -> serde_json::Value {
    use wf_lang::BaseType;
    match base {
        BaseType::Chars => {
            let len = rng.random_range(6..=16);
            let s: String = (0..len)
                .map(|_| {
                    let idx = rng.random_range(0..36u8);
                    if idx < 26 {
                        (b'a' + idx) as char
                    } else {
                        (b'0' + idx - 26) as char
                    }
                })
                .collect();
            serde_json::Value::String(s)
        }
        BaseType::Digit => {
            let n = rng.random_range(0..100_000i64);
            serde_json::Value::Number(serde_json::Number::from(n))
        }
        BaseType::Float => {
            let n: f64 = rng.random_range(0.0..1000.0);
            serde_json::json!(n)
        }
        BaseType::Bool => {
            let b: bool = rng.random();
            serde_json::Value::Bool(b)
        }
        BaseType::Time => {
            // Placeholder — actual timestamp is controlled by stream_gen
            serde_json::json!(0_i64)
        }
        BaseType::Ip => {
            // Random IPv4
            let a = rng.random_range(1..=254u8);
            let b = rng.random_range(0..=255u8);
            let c = rng.random_range(0..=255u8);
            let d = rng.random_range(1..=254u8);
            serde_json::Value::String(format!("{a}.{b}.{c}.{d}"))
        }
        BaseType::Hex => {
            let hex: String = (0..32)
                .map(|_| {
                    let idx = rng.random_range(0..16u8);
                    if idx < 10 {
                        (b'0' + idx) as char
                    } else {
                        (b'a' + idx - 10) as char
                    }
                })
                .collect();
            serde_json::Value::String(hex)
        }
    }
}

/// Generate a value from a GenExpr.
fn generate_from_expr(expr: &GenExpr, rng: &mut StdRng) -> serde_json::Value {
    match expr {
        GenExpr::StringLit(s) => serde_json::Value::String(s.clone()),
        GenExpr::NumberLit(n) => serde_json::json!(n),
        GenExpr::BoolLit(b) => serde_json::Value::Bool(*b),
        GenExpr::GenFunc { name, args } => dispatch_gen_func(name, args, rng),
    }
}

/// Resolve a gen function argument by name (preferred) or positional index.
fn resolve_arg<'a>(args: &'a [GenArg], name: &str, index: usize) -> Option<&'a GenExpr> {
    // First try by name
    for arg in args {
        if arg.name.as_deref() == Some(name) {
            return Some(&arg.value);
        }
    }
    // Fall back to positional
    args.get(index).map(|a| &a.value)
}

/// Dispatch a gen function call.
fn dispatch_gen_func(name: &str, args: &[GenArg], rng: &mut StdRng) -> serde_json::Value {
    match name {
        "ipv4" => {
            let pool = match resolve_arg(args, "pool", 0) {
                Some(GenExpr::NumberLit(n)) => *n as u32,
                _ => 1000,
            };
            // Generate from a pool of IPs
            let idx = rng.random_range(0..pool);
            let a = ((idx >> 16) & 0xFF) as u8;
            let b = ((idx >> 8) & 0xFF) as u8;
            let c = (idx & 0xFF) as u8;
            serde_json::Value::String(format!("10.{a}.{b}.{c}"))
        }
        "pattern" => {
            let format_str = match resolve_arg(args, "format", 0) {
                Some(GenExpr::StringLit(s)) => s.as_str(),
                _ => "val_{}",
            };
            let n = rng.random_range(0..100_000u64);
            let result = format_str.replace("{}", &n.to_string());
            serde_json::Value::String(result)
        }
        "enum" => {
            if let Some(GenExpr::StringLit(values)) = resolve_arg(args, "values", 0) {
                let options: Vec<&str> = values
                    .split(',')
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .collect();
                if !options.is_empty() {
                    let idx = rng.random_range(0..options.len());
                    return serde_json::Value::String(options[idx].to_string());
                }
            }
            if args.is_empty() {
                return serde_json::Value::Null;
            }
            let idx = rng.random_range(0..args.len());
            generate_from_expr(&args[idx].value, rng)
        }
        "range" => {
            let min = match resolve_arg(args, "min", 0) {
                Some(GenExpr::NumberLit(n)) => *n,
                _ => 0.0,
            };
            let max = match resolve_arg(args, "max", 1) {
                Some(GenExpr::NumberLit(n)) => *n,
                _ => 100.0,
            };
            let val = rng.random_range(min..max);
            serde_json::json!(val)
        }
        "timestamp" => {
            // Placeholder — actual timestamp is controlled by stream_gen
            serde_json::json!(0_i64)
        }
        _ => {
            // Unknown gen function — return null
            serde_json::Value::Null
        }
    }
}
