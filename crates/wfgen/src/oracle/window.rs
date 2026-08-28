use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use wf_engine::match_engine::{Event, FieldSource, JoinRow, Value, WindowLookup};
use wf_lang::WindowSchema;
use wf_lang::plan::RulePlan;

/// Join lookup with no state — every join misses. Used when a rule has joins
/// but no schemas were supplied (legacy oracle behavior).
pub(crate) struct EmptyLookup;

impl WindowLookup for EmptyLookup {
    fn snapshot_field_values(
        &self,
        _window: &str,
        _field: &str,
    ) -> Option<std::collections::HashSet<String>> {
        None
    }

    fn snapshot(&self, _window: &str) -> Option<Vec<JoinRow>> {
        None
    }

    fn join_lookup(&self, _window: &str, _key_field: &str, _key: &Value) -> Option<Vec<JoinRow>> {
        None
    }
}

/// P2 (Path A): per-rule join window state for the oracle reference evaluator.
///
/// Mirrors the engine window registry's retention: rows are indexed by the
/// rule's join-condition right-side key field and expire when
/// `ts + over <= watermark` (sliding `over`). Only windows targeted by the
/// rule's joins are tracked.
///
/// **每键多行**：join 目标可能是 bid（key=auction 不唯一，Q9 生命周期内同 auction
/// 多 bid 都要参与 reduce）——`rows` 按 key 存 `Vec<(ts, Event)>`（插入序），
/// 而不是单行覆盖。唯一键窗口（auction/person 表）每键恰 1 行，行为不变。
pub(crate) struct OracleLookup {
    windows: HashMap<String, OracleWindow>,
}

pub(crate) struct OracleWindow {
    /// Index key field (join condition's right side, e.g. `id`).
    key_field: String,
    /// key repr → 该键的全部行（插入序）。
    rows: HashMap<String, Vec<(i64, Event)>>,
    /// ts → keys of rows inserted at that ts. Monotonic watermark expiry pops
    /// from the front in amortized O(1) per event — a full `retain` scan per
    /// event would be O(window rows) × O(events) = quadratic (the 18m hang).
    by_ts: BTreeMap<i64, Vec<String>>,
    /// Sliding retention (`over`) of the target window.
    over_nanos: i64,
}

impl OracleLookup {
    /// Build the lookup for a rule's joins. `None` when the rule has no joins
    /// or no tracked window schema is found.
    pub(crate) fn build(plan: &RulePlan, schemas: &[WindowSchema]) -> Option<Self> {
        if plan.joins.is_empty() {
            return None;
        }
        let mut windows = HashMap::new();
        for join in &plan.joins {
            let Some(schema) = schemas.iter().find(|s| s.name == join.right_window) else {
                continue;
            };
            let Some(cond) = join.conds.first() else {
                continue;
            };
            // checker rejects nested join-condition paths — skip rather than
            // dropping ALL window state via `?` (a single odd join must not
            // disable join state for the other joins).
            let Some(key_field) = wf_lang::plan::JoinCondPlan::right_field_name(cond) else {
                continue;
            };
            windows.insert(
                join.right_window.clone(),
                OracleWindow {
                    key_field: key_field.to_string(),
                    rows: HashMap::new(),
                    by_ts: BTreeMap::new(),
                    over_nanos: schema.over.as_nanos() as i64,
                },
            );
        }
        if windows.is_empty() {
            None
        } else {
            Some(OracleLookup { windows })
        }
    }

    /// Record an incoming row of a tracked window, then expire rows older than
    /// `over` at THIS window's own watermark.
    ///
    /// Watermark semantics (mirrors the engine): each window's retention is
    /// driven by its OWN event stream (the auction window expires auction rows
    /// by auction events, not by the driver's bid timestamps). An event for an
    /// untracked window advances nothing — a late bid must not evict auction
    /// rows (the 2026-08 review finding; NEXMark's over≈auction lifetime masks
    /// it, but the semantics must not depend on that).
    pub(crate) fn feed_window(&mut self, window: &str, event: &Event, ts_nanos: i64) {
        let Some(win) = self.windows.get_mut(window) else {
            return;
        };
        win.expire_before(ts_nanos);
        if let Some(key) = event
            .field_value(&win.key_field)
            .and_then(|v| value_key_repr(&v))
        {
            win.rows
                .entry(key.clone())
                .or_default()
                .push((ts_nanos, event.clone()));
            win.by_ts.entry(ts_nanos).or_default().push(key);
        }
    }

    fn snapshot(&self, window: &str) -> Option<Vec<JoinRow>> {
        let win = self.windows.get(window)?;
        Some(
            win.rows
                .values()
                .flat_map(|rows| {
                    rows.iter()
                        .map(|(_, ev)| JoinRow::Event(Arc::new(ev.clone())))
                })
                .collect(),
        )
    }

    fn snapshot_with_timestamps(&self, window: &str) -> Option<Vec<(i64, JoinRow)>> {
        let win = self.windows.get(window)?;
        Some(
            win.rows
                .values()
                .flat_map(|rows| {
                    rows.iter()
                        .map(|(ts, ev)| (*ts, JoinRow::Event(Arc::new(ev.clone()))))
                })
                .collect(),
        )
    }

    fn join_lookup(&self, window: &str, key_field: &str, key: &Value) -> Option<Vec<JoinRow>> {
        let win = self.windows.get(window)?;
        if win.key_field != key_field {
            return None;
        }
        let key_repr = value_key_repr(key)?;
        win.rows.get(&key_repr).map(|rows| {
            rows.iter()
                .map(|(_, ev)| JoinRow::Event(Arc::new(ev.clone())))
                .collect()
        })
    }

    /// P3：deferred join 到期候选——该 key 的全部 (ts, row)。显式实现以
    /// 避免 trait 默认的全窗 `snapshot_with_timestamps` + 线性过滤（每次到期
    /// 评估 O(右窗全行)：Q8/Q9 对拍会退化到 O(评估次数 × 全窗) 卡死）。
    fn asof_candidates(
        &self,
        window: &str,
        key_field: &str,
        key: &Value,
    ) -> Option<Vec<(i64, JoinRow)>> {
        let win = self.windows.get(window)?;
        if win.key_field != key_field {
            return None;
        }
        let key_repr = value_key_repr(key)?;
        win.rows.get(&key_repr).map(|rows| {
            rows.iter()
                .map(|(ts, ev)| (*ts, JoinRow::Event(Arc::new(ev.clone()))))
                .collect()
        })
    }
}

impl OracleWindow {
    /// Drop rows with `ts + over <= watermark` (i.e. `ts <= watermark - over`).
    /// Watermark is monotonic, so this pops the front of the ts index.
    fn expire_before(&mut self, watermark: i64) {
        if self.over_nanos <= 0 {
            return;
        }
        let cutoff = watermark - self.over_nanos;
        let mut expired: Vec<String> = Vec::new();
        while let Some((&ts, _)) = self.by_ts.iter().next() {
            if ts > cutoff {
                break;
            }
            if let Some(mut keys) = self.by_ts.remove(&ts) {
                expired.append(&mut keys);
            }
        }
        for key in expired {
            if let Some(rows) = self.rows.get_mut(&key) {
                rows.retain(|(ts, _)| *ts > cutoff);
                if rows.is_empty() {
                    self.rows.remove(&key);
                }
            }
        }
    }
}

impl WindowLookup for OracleLookup {
    fn snapshot_field_values(
        &self,
        _window: &str,
        _field: &str,
    ) -> Option<std::collections::HashSet<String>> {
        None
    }

    fn snapshot(&self, window: &str) -> Option<Vec<JoinRow>> {
        self.snapshot(window)
    }

    fn snapshot_with_timestamps(&self, window: &str) -> Option<Vec<(i64, JoinRow)>> {
        self.snapshot_with_timestamps(window)
    }

    fn join_lookup(&self, window: &str, key_field: &str, key: &Value) -> Option<Vec<JoinRow>> {
        self.join_lookup(window, key_field, key)
    }

    fn asof_candidates(
        &self,
        window: &str,
        key_field: &str,
        key: &Value,
    ) -> Option<Vec<(i64, JoinRow)>> {
        self.asof_candidates(window, key_field, key)
    }
}

/// Stable string repr of a scalar join key value (insert and lookup sides go
/// through the same conversion, so they always agree). Float keys are excluded
/// by the checker for join index keys, so bit-exact f64 is fine here.
fn value_key_repr(v: &Value) -> Option<String> {
    match v {
        Value::Number(n) => Some(format!("n:{}", n.to_bits())),
        Value::Str(s) => Some(format!("s:{}", s)),
        Value::Bool(b) => Some(format!("b:{}", b)),
        _ => None,
    }
}
