use super::*;

// -----------------------------------------------------------------------
// SC6 / SC2a tests
// -----------------------------------------------------------------------

#[test]
fn test_sc6_inject_stream_not_in_scenario() {
    let wfg = minimal_wfg(
        vec![stream("s1", "LoginWindow")],
        vec![inject("my_rule", vec!["s1", "s_missing"])],
    );
    let schemas = vec![make_schema("LoginWindow", vec![])];
    let wfl = make_wfl("my_rule", vec![("s1", "LoginWindow")]);
    let errors = validate_wfg(&wfg, &schemas, &[wfl]);
    assert!(
        errors
            .iter()
            .any(|e| e.code == "SC6" && e.message.contains("s_missing"))
    );
}

#[test]
fn test_sc6_sc2a_stream_window_not_in_rule_events() {
    // Stream s1 uses DnsWindow, but the rule only references LoginWindow.
    let wfg = minimal_wfg(
        vec![stream("s1", "DnsWindow")],
        vec![inject("my_rule", vec!["s1"])],
    );
    let schemas = vec![
        make_schema("DnsWindow", vec![]),
        make_schema("LoginWindow", vec![]),
    ];
    let wfl = make_wfl("my_rule", vec![("s1", "LoginWindow")]);
    let errors = validate_wfg(&wfg, &schemas, &[wfl]);
    assert!(errors.iter().any(|e| {
        e.code == "SC6" && e.message.contains("DnsWindow") && e.message.contains("LoginWindow")
    }));
}

#[test]
fn test_sc6_inject_alias_not_in_target_rule_events() {
    let wfg = minimal_wfg(
        vec![stream("s1", "LoginWindow")],
        vec![inject("my_rule", vec!["s1"])],
    );
    let schemas = vec![make_schema("LoginWindow", vec![])];
    let wfl = make_wfl("my_rule", vec![("other_alias", "LoginWindow")]);
    let errors = validate_wfg(&wfg, &schemas, &[wfl]);
    assert!(
        errors
            .iter()
            .any(|e| e.code == "SC6" && e.message.contains("alias"))
    );
}

#[test]
fn test_sc6_sc2a_valid_stream_window_matches_rule() {
    let wfg = minimal_wfg(
        vec![stream("s1", "LoginWindow")],
        vec![inject("my_rule", vec!["s1"])],
    );
    let schemas = vec![make_schema("LoginWindow", vec![])];
    let wfl = make_wfl("my_rule", vec![("s1", "LoginWindow")]);
    let errors = validate_wfg(&wfg, &schemas, &[wfl]);
    // No SC6 errors expected
    assert!(
        !errors.iter().any(|e| e.code == "SC6"),
        "unexpected SC6: {:?}",
        errors
    );
}
