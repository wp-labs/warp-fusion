use super::*;

// -----------------------------------------------------------------------
// SV8 tests (oracle param validation)
// -----------------------------------------------------------------------

fn wfg_with_oracle(oracle: OracleBlock) -> WfgFile {
    WfgFile {
        uses: vec![],
        scenario: ScenarioDecl {
            name: "test".into(),
            seed: 1,
            time_clause: TimeClause {
                start: "2024-01-01T00:00:00Z".into(),
                duration: Duration::from_secs(3600),
            },
            total: 100,
            streams: vec![],
            injects: vec![],
            faults: None,
            oracle: Some(oracle),
        },
        syntax: None,
    }
}

#[test]
fn test_sv8_time_tolerance_must_be_duration() {
    let oracle = OracleBlock {
        params: vec![ParamAssign {
            name: "time_tolerance".into(),
            value: ParamValue::Number(42.0), // wrong: should be Duration
        }],
    };
    let wfg = wfg_with_oracle(oracle);
    let errors = validate_wfg(&wfg, &[], &[]);
    assert!(
        errors
            .iter()
            .any(|e| e.code == "SV8" && e.message.contains("time_tolerance")),
        "errors: {:?}",
        errors
    );
}

#[test]
fn test_sv8_score_tolerance_must_be_nonneg_number() {
    let oracle = OracleBlock {
        params: vec![ParamAssign {
            name: "score_tolerance".into(),
            value: ParamValue::Number(-0.5), // negative
        }],
    };
    let wfg = wfg_with_oracle(oracle);
    let errors = validate_wfg(&wfg, &[], &[]);
    assert!(
        errors
            .iter()
            .any(|e| e.code == "SV8" && e.message.contains("score_tolerance")),
        "errors: {:?}",
        errors
    );
}

#[test]
fn test_sv8_score_tolerance_string_rejected() {
    let oracle = OracleBlock {
        params: vec![ParamAssign {
            name: "score_tolerance".into(),
            value: ParamValue::String("hello".into()),
        }],
    };
    let wfg = wfg_with_oracle(oracle);
    let errors = validate_wfg(&wfg, &[], &[]);
    assert!(
        errors.iter().any(|e| e.code == "SV8"),
        "errors: {:?}",
        errors
    );
}

#[test]
fn test_sv8_valid_oracle_params() {
    let oracle = OracleBlock {
        params: vec![
            ParamAssign {
                name: "time_tolerance".into(),
                value: ParamValue::Duration(Duration::from_secs(2)),
            },
            ParamAssign {
                name: "score_tolerance".into(),
                value: ParamValue::Number(0.05),
            },
        ],
    };
    let wfg = wfg_with_oracle(oracle);
    let errors = validate_wfg(&wfg, &[], &[]);
    assert!(
        !errors.iter().any(|e| e.code == "SV8"),
        "should pass: {:?}",
        errors
    );
}
