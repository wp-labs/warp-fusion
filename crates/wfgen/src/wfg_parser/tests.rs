use super::parse_wfg;
use crate::wfg_ast::*;

#[test]
fn test_parse_minimal_syntax_scenario() {
    let input = r#"
#[duration=10m]
scenario brute_force_detect<seed=42> {
  traffic {
    stream auth_events gen 100/s
  }
}
"#;

    let wfg = parse_wfg(input).unwrap();
    assert_eq!(wfg.scenario.name, "brute_force_detect");
    assert_eq!(wfg.scenario.seed, 42);
    assert!(wfg.syntax.is_some());
    let syntax = wfg.syntax.as_ref().unwrap();
    assert_eq!(syntax.traffic.streams.len(), 1);
    assert_eq!(syntax.traffic.streams[0].stream, "auth_events");
    assert!(matches!(
        syntax.traffic.streams[0].rate,
        RateExpr::Constant(_)
    ));
}

#[test]
fn test_parse_use_declarations() {
    let input = r#"
use "../schemas/security.wfs"
use "../rules/brute_force.wfl"

#[duration=10m]
scenario s<seed=1> {
  traffic { stream auth_events gen 50/s }
}
"#;
    let wfg = parse_wfg(input).unwrap();
    assert_eq!(wfg.uses.len(), 2);
    assert_eq!(wfg.uses[0].path, "../schemas/security.wfs");
    assert_eq!(wfg.uses[1].path, "../rules/brute_force.wfl");
}

#[test]
fn test_parse_rate_expressions_wave_burst_timeline() {
    let input = r#"
#[duration=10m]
scenario rates<seed=2> {
  traffic {
    stream s1 gen wave(base=80/s, amp=20/s, period=2m, shape=triangle)
    stream s2 gen burst(base=40/s, peak=300/s, every=3m, hold=20s)
    stream s3 gen timeline {
      0m..2m=20/s
      2m..4m=60/s
    }
  }
}
"#;
    let wfg = parse_wfg(input).unwrap();
    let t = &wfg.syntax.as_ref().unwrap().traffic.streams;
    assert!(matches!(t[0].rate, RateExpr::Wave { .. }));
    assert!(matches!(t[1].rate, RateExpr::Burst { .. }));
    assert!(matches!(t[2].rate, RateExpr::Timeline(_)));
}

#[test]
fn test_parse_injection_and_expect_extensions() {
    let input = r#"
#[duration=30m]
scenario brute_force_detect<seed=7> {
  traffic {
    stream auth_events gen 200/s
  }

  injection {
    hit<30%> auth_events {
      user seq {
        use(login="failed") with(3,2m)
        then use(action="port_scan") with(1,1m)
      }
    }
    near_miss<10%> auth_events {
      user seq {
        use(login="failed") with(2,2m)
        not(action="port_scan") within(1m)
      }
    }
    miss<60%> auth_events {
      user seq {
        use(login="success") with(1,30s)
      }
    }
  }

  expect {
    hit(brute_force_then_scan) >= 95%
    near_miss(brute_force_then_scan) <= 1%
    miss(brute_force_then_scan) <= 0.1%
    precision(brute_force_then_scan) >= 99%
    recall(brute_force_then_scan) >= 95%
    fpr(brute_force_then_scan) <= 0.5%
    latency_p95(brute_force_then_scan) <= 2s
  }
}
"#;

    let wfg = parse_wfg(input).unwrap();
    let syntax = wfg.syntax.as_ref().unwrap();
    let inj = syntax.injection.as_ref().unwrap();
    assert_eq!(inj.cases.len(), 3);
    assert_eq!(inj.cases[0].mode, InjectCaseMode::Hit);
    assert_eq!(inj.cases[1].mode, InjectCaseMode::NearMiss);
    assert_eq!(inj.cases[2].mode, InjectCaseMode::Miss);
    assert_eq!(inj.cases[0].percent, 30.0);

    let steps = &inj.cases[0].seq.steps;
    assert!(matches!(
        steps[0],
        SeqStep::Use {
            then_from_prev: false,
            ..
        }
    ));
    assert!(matches!(
        steps[1],
        SeqStep::Use {
            then_from_prev: true,
            ..
        }
    ));
    assert!(matches!(inj.cases[1].seq.steps[1], SeqStep::Not { .. }));

    let expect = syntax.expect.as_ref().unwrap();
    assert_eq!(expect.checks.len(), 7);
    assert!(matches!(expect.checks[6].metric, ExpectMetric::LatencyP95));
    assert!(matches!(expect.checks[6].value, ExpectValue::Duration(_)));
}

#[test]
fn test_parse_comments_and_optional_semicolon() {
    let input = r#"
// header
#[duration=10m]
scenario s<seed=1> {
  traffic {
    stream auth_events gen 100/s; // optional semicolon
  }
}
"#;
    let wfg = parse_wfg(input).unwrap();
    assert_eq!(wfg.scenario.name, "s");
}

#[test]
fn test_legacy_syntax_rejected() {
    let input = r#"
scenario legacy seed 1 {
  time "2024-01-01T00:00:00Z" duration 1h
  total 100
  stream s1 : W 10/s
}
"#;
    assert!(parse_wfg(input).is_err());
}
