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
    let errors = validate_wfg(&wfg, &schemas, &[wfl], false);
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
    let errors = validate_wfg(&wfg, &schemas, &[wfl], false);
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
    let errors = validate_wfg(&wfg, &schemas, &[wfl], false);
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
    let errors = validate_wfg(&wfg, &schemas, &[wfl], false);
    // No SC6 errors expected
    assert!(
        !errors.iter().any(|e| e.code == "SC6"),
        "unexpected SC6: {:?}",
        errors
    );
}

#[test]
fn test_sc5_inject_rule_not_found_skipped_when_skip_wfl() {
    // A legacy scenario with an injection block referencing a rule that is not
    // loaded: SC5 must be reported normally, but suppressed under skip_wfl
    // (--no-wfl), where the whole WFL pipeline is opted out.
    let wfg = minimal_wfg(
        vec![stream("s1", "LoginWindow")],
        vec![inject("my_rule", vec!["s1"])],
    );
    let schemas = vec![make_schema("LoginWindow", vec![])];

    let errs_normal = validate_wfg(&wfg, &schemas, &[], false);
    assert!(
        errs_normal.iter().any(|e| e.code == "SC5"),
        "SC5 must be reported when the WFL pipeline is active: {:?}",
        errs_normal
    );

    let errs_skipped = validate_wfg(&wfg, &schemas, &[], true);
    assert!(
        !errs_skipped.iter().any(|e| e.code == "SC5"),
        "SC5 must be skipped under skip_wfl: {:?}",
        errs_skipped
    );
}
