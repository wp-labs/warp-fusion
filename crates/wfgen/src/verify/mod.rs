mod matching;
mod types;

#[cfg(test)]
mod tests;

use std::collections::HashMap;

use crate::oracle::OracleAlert;

// Re-export public types so external API is unchanged.
pub use types::{ActualAlert, AlertDetail, MismatchDetail, VerifyReport, VerifySummary};

use matching::greedy_match;

/// Match key for grouping alerts.
type MatchKey = (String, String, String, String);

/// Compare actual alerts against oracle (expected) alerts.
///
/// Algorithm:
/// 1. Group both sides by match key `(rule_name, entity_type, entity_id, origin)`.
/// 2. Within each group, greedily pair by nearest time.
/// 3. Paired alerts with `|time_diff| > time_tolerance` or `|score_diff| > score_tolerance`
///    count as field_mismatch.
/// 4. Unpaired expected → missing, unpaired actual → unexpected.
/// 5. Status = "pass" iff missing == 0 && unexpected == 0 && field_mismatch == 0.
pub fn verify(
    expected: &[OracleAlert],
    actual: &[ActualAlert],
    score_tolerance: f64,
    time_tolerance_secs: f64,
) -> VerifyReport {
    let expected_groups = group_expected(expected);
    let actual_groups = group_actual(actual);

    let mut matched = 0usize;
    let mut missing = 0usize;
    let mut unexpected = 0usize;
    let mut field_mismatch = 0usize;

    let mut missing_details = Vec::new();
    let mut unexpected_details = Vec::new();
    let mut mismatch_details = Vec::new();

    // Collect all keys from both sides
    let mut all_keys: Vec<MatchKey> = Vec::new();
    for k in expected_groups.keys() {
        all_keys.push(k.clone());
    }
    for k in actual_groups.keys() {
        if !expected_groups.contains_key(k) {
            all_keys.push(k.clone());
        }
    }

    for key in &all_keys {
        let exp_list = expected_groups.get(key);
        let act_list = actual_groups.get(key);

        match (exp_list, act_list) {
            (Some(exp), Some(act)) => {
                let result = greedy_match(exp, act, score_tolerance, time_tolerance_secs);
                matched += result.matched;
                field_mismatch += result.mismatches.len();
                missing += result.missing_indices.len();
                unexpected += result.unexpected_indices.len();

                for &idx in &result.missing_indices {
                    missing_details.push(AlertDetail {
                        rule_name: exp[idx].rule_name.clone(),
                        entity_type: exp[idx].entity_type.clone(),
                        entity_id: exp[idx].entity_id.clone(),
                        score: exp[idx].score,
                        time: exp[idx].emit_time.clone(),
                    });
                }
                for &idx in &result.unexpected_indices {
                    unexpected_details.push(AlertDetail {
                        rule_name: act[idx].rule_name.clone(),
                        entity_type: act[idx].entity_type.clone(),
                        entity_id: act[idx].entity_id.clone(),
                        score: act[idx].score,
                        time: act[idx].fired_at.clone(),
                    });
                }
                for &(exp_idx, act_idx) in &result.mismatches {
                    mismatch_details.push(MismatchDetail {
                        rule_name: exp[exp_idx].rule_name.clone(),
                        entity_type: exp[exp_idx].entity_type.clone(),
                        entity_id: exp[exp_idx].entity_id.clone(),
                        expected_score: exp[exp_idx].score,
                        actual_score: act[act_idx].score,
                        expected_time: exp[exp_idx].emit_time.clone(),
                        actual_time: act[act_idx].fired_at.clone(),
                    });
                }
            }
            (Some(exp), None) => {
                missing += exp.len();
                for e in exp {
                    missing_details.push(AlertDetail {
                        rule_name: e.rule_name.clone(),
                        entity_type: e.entity_type.clone(),
                        entity_id: e.entity_id.clone(),
                        score: e.score,
                        time: e.emit_time.clone(),
                    });
                }
            }
            (None, Some(act)) => {
                unexpected += act.len();
                for a in act {
                    unexpected_details.push(AlertDetail {
                        rule_name: a.rule_name.clone(),
                        entity_type: a.entity_type.clone(),
                        entity_id: a.entity_id.clone(),
                        score: a.score,
                        time: a.fired_at.clone(),
                    });
                }
            }
            (None, None) => {}
        }
    }

    let status = if missing == 0 && unexpected == 0 && field_mismatch == 0 {
        "pass".to_string()
    } else {
        "fail".to_string()
    };

    VerifyReport {
        status,
        summary: VerifySummary {
            oracle_total: expected.len(),
            actual_total: actual.len(),
            matched,
            missing,
            unexpected,
            field_mismatch,
        },
        missing_details,
        unexpected_details,
        mismatch_details,
    }
}

// ---------------------------------------------------------------------------
// Grouping
// ---------------------------------------------------------------------------

fn match_key_expected(a: &OracleAlert) -> MatchKey {
    (
        a.rule_name.clone(),
        a.entity_type.clone(),
        a.entity_id.clone(),
        a.origin.clone(),
    )
}

fn match_key_actual(a: &ActualAlert) -> MatchKey {
    (
        a.rule_name.clone(),
        a.entity_type.clone(),
        a.entity_id.clone(),
        a.origin.clone(),
    )
}

fn group_expected(alerts: &[OracleAlert]) -> HashMap<MatchKey, Vec<&OracleAlert>> {
    let mut map: HashMap<MatchKey, Vec<&OracleAlert>> = HashMap::new();
    for a in alerts {
        map.entry(match_key_expected(a)).or_default().push(a);
    }
    map
}

fn group_actual(alerts: &[ActualAlert]) -> HashMap<MatchKey, Vec<&ActualAlert>> {
    let mut map: HashMap<MatchKey, Vec<&ActualAlert>> = HashMap::new();
    for a in alerts {
        map.entry(match_key_actual(a)).or_default().push(a);
    }
    map
}
