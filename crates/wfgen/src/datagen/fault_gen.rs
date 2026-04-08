use rand::Rng;
use rand::rngs::StdRng;

use crate::datagen::stream_gen::GenEvent;
use crate::wfg_ast::{FaultType, FaultsBlock};

/// Result of applying faults to a clean event stream.
pub struct FaultResult {
    /// Events after fault injection (arrival order, NOT necessarily timestamp-sorted).
    pub events: Vec<GenEvent>,
    pub stats: FaultStats,
}

/// Statistics about applied faults.
#[derive(Debug, Clone, Default)]
pub struct FaultStats {
    pub out_of_order: usize,
    pub late: usize,
    pub duplicate: usize,
    pub dropped: usize,
    pub clean: usize,
}

impl std::fmt::Display for FaultStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} out_of_order, {} late, {} duplicate, {} dropped, {} clean",
            self.out_of_order, self.late, self.duplicate, self.dropped, self.clean
        )
    }
}

/// Fault assignment for a single event.
#[derive(Clone, Copy, PartialEq)]
enum Assignment {
    Clean,
    Fault(FaultType),
}

/// Apply temporal faults to a clean, timestamp-sorted event stream.
///
/// The algorithm is two-phase:
/// 1. **Assign**: Each event is independently assigned a fault type (or clean)
///    based on cumulative percent thresholds.
/// 2. **Transform**: Events are reordered/duplicated/dropped according to their
///    assignment.
///
/// The output represents **arrival order** — it is NOT necessarily sorted by
/// timestamp.
pub fn apply_faults(events: Vec<GenEvent>, faults: &FaultsBlock, rng: &mut StdRng) -> FaultResult {
    if events.is_empty() || faults.faults.is_empty() {
        let clean = events.len();
        return FaultResult {
            events,
            stats: FaultStats {
                clean,
                ..Default::default()
            },
        };
    }

    // Phase 1: Assign fault type to each event
    let assignments = assign_faults(events.len(), faults, rng);

    // Phase 2: Transform based on assignments
    two_pass_transform(events, &assignments, rng)
}

/// Phase 1: For each event, roll [0, 1) and assign a fault type based on
/// cumulative percent thresholds. Each event gets at most one fault.
fn assign_faults(count: usize, faults: &FaultsBlock, rng: &mut StdRng) -> Vec<Assignment> {
    // Build cumulative thresholds
    let mut thresholds: Vec<(FaultType, f64)> = Vec::new();
    let mut cumulative = 0.0;
    for fl in &faults.faults {
        cumulative += fl.percent / 100.0;
        thresholds.push((fl.fault_type, cumulative));
    }

    (0..count)
        .map(|_| {
            let roll: f64 = rng.random();
            for &(ft, threshold) in &thresholds {
                if roll < threshold {
                    return Assignment::Fault(ft);
                }
            }
            Assignment::Clean
        })
        .collect()
}

/// Phase 2: Two-pass transform.
///
/// Pass 1: Build output list handling Clean, Drop, Duplicate, OutOfOrder.
///         Collect Late events into a deferred list with target offsets.
/// Pass 2: Insert deferred (late) events at their target positions.
fn two_pass_transform(
    events: Vec<GenEvent>,
    assignments: &[Assignment],
    rng: &mut StdRng,
) -> FaultResult {
    let n = events.len();
    let mut stats = FaultStats::default();
    let mut result: Vec<GenEvent> = Vec::with_capacity(n);
    // (event, target_insertion_index_in_result)
    let mut deferred: Vec<(GenEvent, usize)> = Vec::new();

    // Track which events have been consumed by an OutOfOrder swap partner
    let mut consumed = vec![false; n];

    let mut i = 0;
    while i < n {
        if consumed[i] {
            i += 1;
            continue;
        }

        match assignments[i] {
            Assignment::Clean => {
                result.push(events[i].clone());
                stats.clean += 1;
            }
            Assignment::Fault(FaultType::Drop) => {
                stats.dropped += 1;
            }
            Assignment::Fault(FaultType::Duplicate) => {
                result.push(events[i].clone());
                result.push(events[i].clone());
                stats.duplicate += 1;
            }
            Assignment::Fault(FaultType::OutOfOrder) => {
                // Swap with next unconsumed event
                if i + 1 < n && !consumed[i + 1] {
                    result.push(events[i + 1].clone());
                    result.push(events[i].clone());
                    consumed[i + 1] = true;
                    stats.out_of_order += 1;
                    // The partner is moved but not independently faulted
                    stats.clean += 1;
                } else {
                    // Last event or next already consumed: degrade to clean
                    result.push(events[i].clone());
                    stats.clean += 1;
                }
            }
            Assignment::Fault(FaultType::Late) => {
                // Defer: insert at a random later position in the output
                // We record the current output length + a random offset as the
                // target insertion index. The offset is chosen so the event
                // arrives noticeably later than its natural position.
                let current_pos = result.len();
                // Count remaining events (rough estimate for offset range)
                let remaining = n - i - 1;
                if remaining >= 1 {
                    let max_offset = remaining.min(120);
                    let min_offset = 1.min(max_offset);
                    let offset = rng.random_range(min_offset..=max_offset);
                    deferred.push((events[i].clone(), current_pos + offset));
                    stats.late += 1;
                } else {
                    // No room to defer, emit as clean
                    result.push(events[i].clone());
                    stats.clean += 1;
                }
            }
        }

        i += 1;
    }

    // Pass 2: Insert deferred (late) events at their target positions.
    // Sort deferred by target index in reverse so insertions don't shift
    // earlier targets.
    deferred.sort_by(|a, b| b.1.cmp(&a.1));
    for (event, target_idx) in deferred {
        let idx = target_idx.min(result.len());
        result.insert(idx, event);
    }

    FaultResult {
        events: result,
        stats,
    }
}
