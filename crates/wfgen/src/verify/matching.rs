use crate::oracle::OracleAlert;

use super::types::ActualAlert;

/// Result of greedy matching within a single key group.
pub(super) struct MatchResult {
    pub(super) matched: usize,
    /// (expected_idx, actual_idx) pairs with score mismatch.
    pub(super) mismatches: Vec<(usize, usize)>,
    /// Indices into the expected slice that were not paired.
    pub(super) missing_indices: Vec<usize>,
    /// Indices into the actual slice that were not paired.
    pub(super) unexpected_indices: Vec<usize>,
}

/// Greedily pair expected and actual alerts within a group by nearest time.
pub(super) fn greedy_match(
    expected: &[&OracleAlert],
    actual: &[&ActualAlert],
    score_tolerance: f64,
    time_tolerance_secs: f64,
) -> MatchResult {
    let mut used_actual = vec![false; actual.len()];
    let mut matched = 0usize;
    let mut mismatches = Vec::new();
    let mut paired_expected = vec![false; expected.len()];

    // For each expected alert, find the nearest unused actual by time
    for (ei, exp) in expected.iter().enumerate() {
        let exp_time = parse_time_approx(&exp.emit_time);
        let mut best_idx: Option<usize> = None;
        let mut best_dist = f64::MAX;

        for (j, act) in actual.iter().enumerate() {
            if used_actual[j] {
                continue;
            }
            let act_time = parse_time_approx(&act.fired_at);
            let dist = (exp_time - act_time).abs();
            if dist < best_dist {
                best_dist = dist;
                best_idx = Some(j);
            }
        }

        if let Some(j) = best_idx {
            used_actual[j] = true;
            paired_expected[ei] = true;
            let score_diff = (exp.score - actual[j].score).abs();
            let time_diff = best_dist; // abs time diff in seconds
            if score_diff <= score_tolerance && time_diff <= time_tolerance_secs {
                matched += 1;
            } else {
                mismatches.push((ei, j));
            }
        }
    }

    let missing_indices: Vec<usize> = paired_expected
        .iter()
        .enumerate()
        .filter(|(_, paired)| !**paired)
        .map(|(i, _)| i)
        .collect();

    let unexpected_indices: Vec<usize> = used_actual
        .iter()
        .enumerate()
        .filter(|(_, used)| !**used)
        .map(|(i, _)| i)
        .collect();

    MatchResult {
        matched,
        mismatches,
        missing_indices,
        unexpected_indices,
    }
}

/// Parse an ISO 8601 timestamp to seconds-since-epoch (approximate, for ordering).
pub(super) fn parse_time_approx(s: &str) -> f64 {
    s.parse::<chrono::DateTime<chrono::Utc>>()
        .map(|dt| dt.timestamp() as f64 + dt.timestamp_subsec_millis() as f64 / 1000.0)
        .unwrap_or(0.0)
}
