use super::*;
use crate::datagen::fault_gen::apply_faults;
use crate::wfg_ast::{FaultLine, FaultType, FaultsBlock};
use rand::SeedableRng;
use rand::rngs::StdRng;

fn make_clean_events(count: usize) -> Vec<super::super::stream_gen::GenEvent> {
    let duration_secs = (count as u64 / 10).max(1);
    let input = format!(
        r#"
#[duration={}s]
scenario fault_helper<seed=42> {{
    traffic {{
        stream LoginWindow gen 10/s
    }}
}}
"#,
        duration_secs
    );
    let wfg = parse_wfg(&input).unwrap();
    let schemas = vec![make_login_schema()];
    generate(&wfg, &schemas, &[]).unwrap().events
}

fn faults_block(faults: Vec<(FaultType, f64)>) -> FaultsBlock {
    FaultsBlock {
        faults: faults
            .into_iter()
            .map(|(ft, pct)| FaultLine {
                fault_type: ft,
                percent: pct,
            })
            .collect(),
    }
}

#[test]
fn test_fault_drop_removes_events() {
    let events = make_clean_events(100);
    let faults = faults_block(vec![(FaultType::Drop, 10.0)]);
    let mut rng = StdRng::seed_from_u64(42);
    let result = apply_faults(events, &faults, &mut rng);

    assert!(
        result.events.len() < 100,
        "drop should reduce event count, got {}",
        result.events.len()
    );
    assert!(result.stats.dropped > 0);
    assert_eq!(
        result.stats.dropped + result.stats.clean,
        100,
        "dropped + clean should equal original count"
    );
}

#[test]
fn test_fault_duplicate_adds_events() {
    let events = make_clean_events(100);
    let faults = faults_block(vec![(FaultType::Duplicate, 10.0)]);
    let mut rng = StdRng::seed_from_u64(42);
    let result = apply_faults(events, &faults, &mut rng);

    assert!(
        result.events.len() > 100,
        "duplicate should increase event count, got {}",
        result.events.len()
    );
    assert!(result.stats.duplicate > 0);
    // Each duplicate adds 1 extra event
    assert_eq!(result.events.len(), 100 + result.stats.duplicate);
}

#[test]
fn test_fault_out_of_order_preserves_count() {
    let events = make_clean_events(100);
    let faults = faults_block(vec![(FaultType::OutOfOrder, 20.0)]);
    let mut rng = StdRng::seed_from_u64(42);
    let result = apply_faults(events, &faults, &mut rng);

    // OutOfOrder swaps pairs but doesn't change count
    assert_eq!(result.events.len(), 100);
    assert!(result.stats.out_of_order > 0);

    // Verify some timestamps are out of order
    let mut has_disorder = false;
    for w in result.events.windows(2) {
        if w[0].timestamp > w[1].timestamp {
            has_disorder = true;
            break;
        }
    }
    assert!(
        has_disorder,
        "out_of_order should produce timestamp disorder"
    );
}

#[test]
fn test_fault_late_preserves_count() {
    let events = make_clean_events(100);
    let faults = faults_block(vec![(FaultType::Late, 10.0)]);
    let mut rng = StdRng::seed_from_u64(42);
    let result = apply_faults(events, &faults, &mut rng);

    // Late moves events later in the output but doesn't change count
    assert_eq!(result.events.len(), 100);
    assert!(result.stats.late > 0);
}

#[test]
fn test_fault_deterministic() {
    let events1 = make_clean_events(100);
    let events2 = make_clean_events(100);
    let faults = faults_block(vec![
        (FaultType::OutOfOrder, 10.0),
        (FaultType::Late, 5.0),
        (FaultType::Duplicate, 3.0),
        (FaultType::Drop, 2.0),
    ]);
    let mut rng1 = StdRng::seed_from_u64(99);
    let mut rng2 = StdRng::seed_from_u64(99);

    let result1 = apply_faults(events1, &faults, &mut rng1);
    let result2 = apply_faults(events2, &faults, &mut rng2);

    assert_eq!(result1.events.len(), result2.events.len());
    for (e1, e2) in result1.events.iter().zip(result2.events.iter()) {
        assert_eq!(e1.timestamp, e2.timestamp);
        assert_eq!(e1.fields, e2.fields);
    }
}

#[test]
fn test_fault_combined_stats() {
    let events = make_clean_events(200);
    let faults = faults_block(vec![
        (FaultType::OutOfOrder, 10.0),
        (FaultType::Late, 5.0),
        (FaultType::Duplicate, 3.0),
        (FaultType::Drop, 2.0),
    ]);
    let mut rng = StdRng::seed_from_u64(42);
    let result = apply_faults(events, &faults, &mut rng);
    let s = &result.stats;

    // Every input event is accounted for exactly once
    // Note: out_of_order consumes 1 event but also counts the partner as clean
    // So total assignments = out_of_order + late + duplicate + dropped + clean
    // But out_of_order also increments clean for the partner
    // Total input events = out_of_order + late + duplicate + dropped + clean - out_of_order
    //                    = late + duplicate + dropped + clean
    // Actually: each out_of_order event contributes (1 out_of_order + 1 clean for partner)
    // But the partner was already in the input. So:
    // input_count = out_of_order + clean_from_partner + late + duplicate + dropped + other_clean
    //            = out_of_order + late + duplicate + dropped + clean
    // where clean includes the partners
    assert_eq!(
        s.out_of_order + s.late + s.duplicate + s.dropped + s.clean,
        200,
        "stats should account for all input events (including out_of_order partners)"
    );
}

#[test]
fn test_empty_faults_passthrough() {
    let events = make_clean_events(50);
    let faults = FaultsBlock { faults: vec![] };
    let mut rng = StdRng::seed_from_u64(1);
    let result = apply_faults(events, &faults, &mut rng);

    assert_eq!(result.events.len(), 50);
    assert_eq!(result.stats.clean, 50);
    assert_eq!(result.stats.dropped, 0);
    assert_eq!(result.stats.duplicate, 0);
    assert_eq!(result.stats.out_of_order, 0);
    assert_eq!(result.stats.late, 0);
}
