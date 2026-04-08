use super::*;

#[test]
fn test_no_inject_backward_compat() {
    // Without injection blocks, generate() behaves identically with or without rule_plans.
    let input = r#"
#[duration=10s]
scenario compat<seed=42> {
    traffic {
        stream LoginWindow gen 10/s
    }
}
"#;
    let wfg = parse_wfg(input).unwrap();
    let schemas = vec![make_login_schema()];
    let plans = vec![make_brute_force_plan()];

    let result_with_plans = generate(&wfg, &schemas, &plans).unwrap();
    let result_without = generate(&wfg, &schemas, &[]).unwrap();

    assert_eq!(result_with_plans.events.len(), result_without.events.len());
    for (e1, e2) in result_with_plans
        .events
        .iter()
        .zip(result_without.events.iter())
    {
        assert_eq!(e1.timestamp, e2.timestamp);
        assert_eq!(e1.fields, e2.fields);
    }
}
