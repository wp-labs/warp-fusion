use super::*;

// -----------------------------------------------------------------------
// SV7 tests
// -----------------------------------------------------------------------

#[test]
fn test_sv7_string_lit_on_digit_field() {
    let wfg = minimal_wfg(
        vec![stream_with_override(
            "s1",
            "W",
            "count",
            GenExpr::StringLit("hello".into()),
        )],
        vec![],
    );
    let schemas = vec![make_schema("W", vec![("count", BaseType::Digit)])];
    let errors = validate_wfg(&wfg, &schemas, &[]);
    assert!(
        errors
            .iter()
            .any(|e| e.code == "SV7" && e.message.contains("count"))
    );
}

#[test]
fn test_sv7_number_lit_on_bool_field() {
    let wfg = minimal_wfg(
        vec![stream_with_override(
            "s1",
            "W",
            "flag",
            GenExpr::NumberLit(42.0),
        )],
        vec![],
    );
    let schemas = vec![make_schema("W", vec![("flag", BaseType::Bool)])];
    let errors = validate_wfg(&wfg, &schemas, &[]);
    assert!(
        errors
            .iter()
            .any(|e| e.code == "SV7" && e.message.contains("flag"))
    );
}

#[test]
fn test_sv7_bool_lit_on_chars_field() {
    let wfg = minimal_wfg(
        vec![stream_with_override(
            "s1",
            "W",
            "name",
            GenExpr::BoolLit(true),
        )],
        vec![],
    );
    let schemas = vec![make_schema("W", vec![("name", BaseType::Chars)])];
    let errors = validate_wfg(&wfg, &schemas, &[]);
    assert!(
        errors
            .iter()
            .any(|e| e.code == "SV7" && e.message.contains("name"))
    );
}

#[test]
fn test_sv7_ipv4_on_digit_field() {
    let wfg = minimal_wfg(
        vec![stream_with_override(
            "s1",
            "W",
            "port",
            GenExpr::GenFunc {
                name: "ipv4".into(),
                args: vec![],
            },
        )],
        vec![],
    );
    let schemas = vec![make_schema("W", vec![("port", BaseType::Digit)])];
    let errors = validate_wfg(&wfg, &schemas, &[]);
    assert!(
        errors
            .iter()
            .any(|e| e.code == "SV7" && e.message.contains("port"))
    );
}

#[test]
fn test_sv7_range_on_ip_field() {
    let wfg = minimal_wfg(
        vec![stream_with_override(
            "s1",
            "W",
            "addr",
            GenExpr::GenFunc {
                name: "range".into(),
                args: vec![],
            },
        )],
        vec![],
    );
    let schemas = vec![make_schema("W", vec![("addr", BaseType::Ip)])];
    let errors = validate_wfg(&wfg, &schemas, &[]);
    assert!(
        errors
            .iter()
            .any(|e| e.code == "SV7" && e.message.contains("addr"))
    );
}

#[test]
fn test_sv7_enum_compatible_with_any_type() {
    let wfg = minimal_wfg(
        vec![stream_with_override(
            "s1",
            "W",
            "val",
            GenExpr::GenFunc {
                name: "enum".into(),
                args: vec![],
            },
        )],
        vec![],
    );
    let schemas = vec![make_schema("W", vec![("val", BaseType::Digit)])];
    let errors = validate_wfg(&wfg, &schemas, &[]);
    assert!(
        !errors.iter().any(|e| e.code == "SV7"),
        "enum should be compatible with any type"
    );
}

#[test]
fn test_sv7_valid_combinations() {
    // String on Chars, Number on Float, ipv4 on Ip, range on Digit -- all valid
    let wfg = minimal_wfg(
        vec![StreamBlock {
            alias: "s1".into(),
            window: "W".into(),
            rate: Rate {
                count: 10,
                unit: RateUnit::PerSecond,
            },
            overrides: vec![
                FieldOverride {
                    field_name: "name".into(),
                    gen_expr: GenExpr::StringLit("test".into()),
                },
                FieldOverride {
                    field_name: "score".into(),
                    gen_expr: GenExpr::NumberLit(3.21),
                },
                FieldOverride {
                    field_name: "addr".into(),
                    gen_expr: GenExpr::GenFunc {
                        name: "ipv4".into(),
                        args: vec![],
                    },
                },
                FieldOverride {
                    field_name: "count".into(),
                    gen_expr: GenExpr::GenFunc {
                        name: "range".into(),
                        args: vec![],
                    },
                },
            ],
        }],
        vec![],
    );
    let schemas = vec![make_schema(
        "W",
        vec![
            ("name", BaseType::Chars),
            ("score", BaseType::Float),
            ("addr", BaseType::Ip),
            ("count", BaseType::Digit),
        ],
    )];
    let errors = validate_wfg(&wfg, &schemas, &[]);
    assert!(
        !errors.iter().any(|e| e.code == "SV7"),
        "all valid: {:?}",
        errors
    );
}
