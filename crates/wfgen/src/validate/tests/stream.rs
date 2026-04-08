use super::*;

// -----------------------------------------------------------------------
// SC2 / SC2a tests
// -----------------------------------------------------------------------

#[test]
fn test_sc2_stream_alias_not_in_any_rule() {
    let wfg = minimal_wfg(vec![stream("s_missing", "LoginWindow")], vec![]);
    let schemas = vec![make_schema("LoginWindow", vec![])];
    let wfl = make_wfl("my_rule", vec![("e", "LoginWindow")]);
    let errors = validate_wfg(&wfg, &schemas, &[wfl]);
    assert!(
        errors
            .iter()
            .any(|e| e.code == "SC2" && e.message.contains("s_missing"))
    );
}

#[test]
fn test_sc2a_stream_alias_window_mismatch() {
    let wfg = minimal_wfg(vec![stream("e", "DnsWindow")], vec![]);
    let schemas = vec![
        make_schema("DnsWindow", vec![]),
        make_schema("LoginWindow", vec![]),
    ];
    let wfl = make_wfl("my_rule", vec![("e", "LoginWindow")]);
    let errors = validate_wfg(&wfg, &schemas, &[wfl]);
    assert!(
        errors
            .iter()
            .any(|e| e.code == "SC2a" && e.message.contains("DnsWindow"))
    );
}
