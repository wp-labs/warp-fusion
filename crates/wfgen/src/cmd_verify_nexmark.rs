//! Rust 版 NEXMark Q2-Q21 ground-truth 模拟器（对应
//! nexmark_pk/scripts/verify_ground_truth.py）。
//!
//! `wfgen verify-nexmark <count> [--seed N]`：内部用与 `gen-nexmark` 完全相同的
//! rng 序列生成事件（复用 `cmd_gen_nexmark::generate_events`），按与 gen 输出
//! 相同的 **30s 桶序** 喂模拟器（跳过 JSONL 中间产物）。Python 版 10M 需
//! ~3.5min，本实现 <10s。
//!
//! 语义镜像 verify_ground_truth.py：
//! - 懒过期堆 + per-key pending 去重（引擎 push_expiry_candidate 的 dedup）；
//! - `on event` fire+reset / `on event<accu>` rearm；滑动窗口 `match<key:10m>`；
//! - q16 固定 10m 桶 sum(price)>=1000；q21 anti join 用 person 窗口集合。

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

use serde_json::json;

use crate::cmd_gen_nexmark::generate_events;
use crate::error::WfgenResult;

const SPAN: i64 = 600_000_000_000; // 10m sliding window
const WITHIN_60S: i64 = 60_000_000_000; // q19 seq within 60s
const BUCKET_NS: i64 = 600_000_000_000; // q16 fixed bucket
const NONE: i64 = -1; // "no instance" sentinel for created/max slots

struct Sim {
    q2: i64,
    n_auction: i64,
    n_bid: i64,
    q5: [i64; 3],
    q6: i64,
    q7: [i64; 3],
    q8: i64,
    q10: i64,
    q13: i64,
    q15: i64,
    q16: i64,
    q17: i64,
    q18: i64,
    q19: i64,
    q20: i64,
    q21: i64,
    // q5/q7/q6 shared instance table:
    // [c10,n10,c50,n50,c100,n100,c200,m200,c500,m500,c1000,m1000,c_avg,sum_avg,cnt_avg]
    state: HashMap<i64, [i64; 15]>,
    heap: BinaryHeap<Reverse<(i64, i64, usize)>>, // (expire_at, auc, slot)
    pending: HashSet<(i64, usize)>,
    // q15-q20 independent rule state (own machines, shared lazy heap)
    q15_count: HashMap<i64, i64>,
    q15_created: HashMap<i64, i64>,
    q17_set: HashMap<i64, HashSet<i64>>,
    q17_created: HashMap<i64, i64>,
    q18_count: HashMap<i64, i64>,
    q18_created: HashMap<i64, i64>,
    q19_step: HashMap<i64, i64>,
    q19_t0: HashMap<i64, i64>,
    q19_created: HashMap<i64, i64>,
    q20_count: HashMap<i64, i64>,
    q20_created: HashMap<i64, i64>,
    heap2: BinaryHeap<Reverse<(i64, &'static str, i64)>>,
    pending2: HashSet<(&'static str, i64)>,
    // q16 fixed buckets + q21 person set
    q16_sum: HashMap<(i64, i64), i64>,
    person_ids: HashSet<i64>,
    watermark: i64,
}

impl Default for Sim {
    fn default() -> Self {
        Self::new()
    }
}

impl Sim {
    fn new() -> Self {
        Sim {
            q2: 0,
            n_auction: 0,
            n_bid: 0,
            q5: [0; 3],
            q6: 0,
            q7: [0; 3],
            q8: 0,
            q10: 0,
            q13: 0,
            q15: 0,
            q16: 0,
            q17: 0,
            q18: 0,
            q19: 0,
            q20: 0,
            q21: 0,
            state: HashMap::new(),
            heap: BinaryHeap::new(),
            pending: HashSet::new(),
            q15_count: HashMap::new(),
            q15_created: HashMap::new(),
            q17_set: HashMap::new(),
            q17_created: HashMap::new(),
            q18_count: HashMap::new(),
            q18_created: HashMap::new(),
            q19_step: HashMap::new(),
            q19_t0: HashMap::new(),
            q19_created: HashMap::new(),
            q20_count: HashMap::new(),
            q20_created: HashMap::new(),
            heap2: BinaryHeap::new(),
            pending2: HashSet::new(),
            q16_sum: HashMap::new(),
            person_ids: HashSet::new(),
            watermark: 0,
        }
    }

    fn push(&mut self, auc: i64, ci: usize, created: i64) {
        if self.pending.insert((auc, ci)) {
            self.heap.push(Reverse((created + SPAN, auc, ci)));
        }
    }

    fn push2(&mut self, rule: &'static str, auc: i64, created: i64) {
        if self.pending2.insert((rule, auc)) {
            self.heap2.push(Reverse((created + SPAN, rule, auc)));
        }
    }

    fn expire2(&mut self, rule: &'static str, auc: i64) {
        match rule {
            "q15" => {
                self.q15_count.remove(&auc);
                self.q15_created.remove(&auc);
            }
            "q17" => {
                self.q17_set.remove(&auc);
                self.q17_created.remove(&auc);
            }
            "q18" => {
                self.q18_count.remove(&auc);
                self.q18_created.remove(&auc);
            }
            "q19" => {
                self.q19_step.remove(&auc);
                self.q19_t0.remove(&auc);
                self.q19_created.remove(&auc);
            }
            "q20" => {
                self.q20_count.remove(&auc);
                self.q20_created.remove(&auc);
            }
            _ => {}
        }
    }

    /// Sweep the q15-q20 lazy expiry heap up to `watermark`.
    fn sweep2(&mut self, watermark: i64) {
        while let Some(Reverse((exp, rule, auc))) = self.heap2.peek().copied() {
            if exp > watermark {
                break;
            }
            self.heap2.pop();
            self.pending2.remove(&(rule, auc));
            let created = match rule {
                "q15" => self.q15_created.get(&auc).copied(),
                "q17" => self.q17_created.get(&auc).copied(),
                "q18" => self.q18_created.get(&auc).copied(),
                "q19" => self.q19_created.get(&auc).copied(),
                "q20" => self.q20_created.get(&auc).copied(),
                _ => None,
            };
            let Some(created) = created else { continue };
            let cur = created + SPAN;
            if cur > watermark {
                self.pending2.insert((rule, auc));
                self.heap2.push(Reverse((cur, rule, auc)));
                continue;
            }
            self.expire2(rule, auc);
        }
    }

    /// Sweep the q5/q7/q6 lazy expiry heap up to `watermark`.
    fn sweep(&mut self, watermark: i64) {
        while let Some(Reverse((exp, auc, ci))) = self.heap.peek().copied() {
            if exp > watermark {
                break;
            }
            self.heap.pop();
            self.pending.remove(&(auc, ci));
            let cur = self
                .state
                .get(&auc)
                .and_then(|st| if st[ci] != NONE { Some(st[ci] + SPAN) } else { None });
            let Some(cur) = cur else { continue };
            if cur > watermark {
                self.pending.insert((auc, ci));
                self.heap.push(Reverse((cur, auc, ci)));
                continue;
            }
            if let Some(st) = self.state.get_mut(&auc) {
                st[ci] = NONE;
                if ci < 6 {
                    st[ci + 1] = 0;
                } else if ci < 12 {
                    st[ci + 1] = NONE;
                } else {
                    st[13] = 0;
                    st[14] = 0;
                }
            }
        }
    }

    fn person(&mut self, id: i64) {
        self.q8 += 1;
        self.person_ids.insert(id);
    }

    fn auction(&mut self) {
        self.n_auction += 1;
    }

    fn bid(&mut self, auc: i64, price: i64, ns: i64, bidder: i64) {
        self.n_bid += 1;
        if auc % 123 == 0 {
            self.q2 += 1;
        }
        if auc % 7 == 0 {
            self.q10 += 1;
        }
        self.q13 += 1;

        if ns > self.watermark {
            self.watermark = ns;
        }
        self.sweep(self.watermark);
        self.sweep2(self.watermark);

        let st = self
            .state
            .entry(auc)
            .or_insert([NONE, 0, NONE, 0, NONE, 0, NONE, NONE, NONE, NONE, NONE, NONE, NONE, 0, 0]);

        // q5: count>=10/50/100 (fire+reset)
        for (i, t) in [10i64, 50, 100].iter().enumerate() {
            let ci = i * 2;
            let ni = ci + 1;
            if st[ci] == NONE {
                st[ci] = ns;
                if self.pending.insert((auc, ci)) {
                    self.heap.push(Reverse((ns + SPAN, auc, ci)));
                }
            }
            st[ni] += 1;
            if st[ni] >= *t {
                self.q5[i] += 1;
                st[ci] = ns;
                st[ni] = 0;
                if self.pending.insert((auc, ci)) {
                    self.heap.push(Reverse((ns + SPAN, auc, ci)));
                }
            }
        }
        // q7: running max >= 200/500/1000 (fire+reset)
        for (i, t) in [200i64, 500, 1000].iter().enumerate() {
            let ci = 6 + i * 2;
            let mi = ci + 1;
            if st[ci] == NONE {
                st[ci] = ns;
                if self.pending.insert((auc, ci)) {
                    self.heap.push(Reverse((ns + SPAN, auc, ci)));
                }
            }
            st[mi] = if st[mi] == NONE { price } else { st[mi].max(price) };
            if st[mi] >= *t {
                self.q7[i] += 1;
                st[ci] = ns;
                st[mi] = NONE;
                if self.pending.insert((auc, ci)) {
                    self.heap.push(Reverse((ns + SPAN, auc, ci)));
                }
            }
        }
        // q6: running avg price >= 200 (fire+reset)
        if st[12] == NONE {
            st[12] = ns;
            if self.pending.insert((auc, 12)) {
                self.heap.push(Reverse((ns + SPAN, auc, 12)));
            }
        }
        st[13] += price;
        st[14] += 1;
        if st[13] >= 200 * st[14] {
            self.q6 += 1;
            st[12] = ns;
            st[13] = 0;
            st[14] = 0;
            if self.pending.insert((auc, 12)) {
                self.heap.push(Reverse((ns + SPAN, auc, 12)));
            }
        }

        // q15: price>100 + count>=5 (fire+reset)
        if price > 100 {
            if !self.q15_created.contains_key(&auc) {
                self.q15_created.insert(auc, ns);
                self.q15_count.insert(auc, 0);
                self.push2("q15", auc, ns);
            }
            let c = self.q15_count.entry(auc).or_insert(0);
            *c += 1;
            if *c >= 5 {
                self.q15 += 1;
                *c = 0;
                self.q15_created.insert(auc, ns);
                self.push2("q15", auc, ns);
            }
        }

        // q17: distinct bidder count >= 20 (fire+reset, set cleared)
        if !self.q17_created.contains_key(&auc) {
            self.q17_created.insert(auc, ns);
            self.q17_set.insert(auc, HashSet::new());
            self.push2("q17", auc, ns);
        }
        {
            let set = self.q17_set.entry(auc).or_default();
            if set.insert(bidder) && set.len() >= 20 {
                self.q17 += 1;
                set.clear();
                self.q17_created.insert(auc, ns);
                self.push2("q17", auc, ns);
            }
        }

        // q18: accu count>=5 (rearm — fires on every subsequent bid)
        if !self.q18_created.contains_key(&auc) {
            self.q18_created.insert(auc, ns);
            self.q18_count.insert(auc, 0);
            self.push2("q18", auc, ns);
        }
        let c = self.q18_count.entry(auc).or_insert(0);
        *c += 1;
        if *c >= 5 {
            self.q18 += 1;
        }

        // q19: seq { has b; has b within 60s }
        if !self.q19_created.contains_key(&auc) {
            self.q19_created.insert(auc, ns);
            self.q19_step.insert(auc, 0);
            self.push2("q19", auc, ns);
        }
        {
            let step = *self.q19_step.entry(auc).or_insert(0);
            if step == 0 {
                self.q19_step.insert(auc, 1);
                self.q19_t0.insert(auc, ns);
            } else {
                let t0 = *self.q19_t0.entry(auc).or_insert(ns);
                let gap = ns - t0;
                if 0 <= gap && gap <= WITHIN_60S {
                    self.q19 += 1;
                    self.q19_step.insert(auc, 0);
                    self.q19_t0.insert(auc, NONE);
                } else {
                    self.q19_step.insert(auc, 0);
                    self.q19_t0.insert(auc, NONE);
                }
                self.q19_created.insert(auc, ns);
                self.push2("q19", auc, ns);
            }
        }

        // q20: any { count>=2; count>=3 } == count >= 3 (fire+reset)
        if !self.q20_created.contains_key(&auc) {
            self.q20_created.insert(auc, ns);
            self.q20_count.insert(auc, 0);
            self.push2("q20", auc, ns);
        }
        let c = self.q20_count.entry(auc).or_insert(0);
        *c += 1;
        if *c >= 3 {
            self.q20 += 1;
            *c = 0;
            self.q20_created.insert(auc, ns);
            self.push2("q20", auc, ns);
        }

        // q16: fixed 10m bucket sum(price)
        let bucket = (ns / BUCKET_NS) * BUCKET_NS;
        *self.q16_sum.entry((auc, bucket)).or_insert(0) += price;

        // q21: anti join — keep bid iff bidder not in person window
        if !self.person_ids.contains(&bidder) {
            self.q21 += 1;
        }
    }

    fn to_json(&self) -> String {
        let q16 = self.q16_sum.values().filter(|s| **s >= 1000).count() as i64;
        json!({
            "q2_mod123": self.q2,
            "q3_auction_seller": self.n_auction,
            "q4_real_avg_100": self.n_bid,
            "q5_bidcount_10": self.q5[0],
            "q5_bidcount_50": self.q5[1],
            "q5_bidcount_100": self.q5[2],
            "q6_avg_price_200": self.q6,
            "q7_maxbid_200": self.q7[0],
            "q7_maxbid_500": self.q7[1],
            "q7_maxbid_1000": self.q7[2],
            "q8_monitor_new_user": self.q8,
            "q10_arbitrary_selection": self.q10,
            "q13_bid_person_join": self.q13,
            "q15_high_bid_count_5": self.q15,
            "q16_sum_price_1000": q16,
            "q17_distinct_bidders_20": self.q17,
            "q18_accumulate_fires": self.q18,
            "q19_seq_two_bids": self.q19,
            "q20_any_count_3": self.q20,
            "q21_anti_person": self.q21,
            "_counts": {"auctions": self.n_auction, "bids": self.n_bid},
        })
        .to_string()
    }
}

/// 轻量事件（丢弃 gen 输出的无关字段，桶序收集内存有界）。
enum Ev {
    Person { id: i64, ns: i64 },
    Auction { ns: i64 },
    Bid { auc: i64, price: i64, ns: i64, bidder: i64 },
}

pub fn run(count: i64, seed: u64) -> WfgenResult<()> {
    // 与 `gen-nexmark` 相同的 30s 桶序：事件按桶收集（桶内生成序），再按桶序
    // 喂模拟器——否则 phase-major 生成序会让滑动窗口/watermark 对拍失真。
    const T_BUCKET_NS: i64 = 30_000_000_000;
    const BASE_NS: i64 = 1767225600000000000; // 与 cmd_gen_nexmark::BASE_NS 一致
    const BUCKETS: usize = 60;
    let mut buckets: Vec<Vec<Ev>> = (0..BUCKETS).map(|_| Vec::new()).collect();
    generate_events(count, seed, |stream, _ns, fields| {
        let ev = match stream {
            "person_events" => Ev::Person {
                id: fields.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
                ns: fields.get("dateTime").and_then(|v| v.as_i64()).unwrap_or(0),
            },
            "auction_events" => Ev::Auction {
                ns: fields.get("dateTime").and_then(|v| v.as_i64()).unwrap_or(0),
            },
            "bid_events" => Ev::Bid {
                auc: fields.get("auction").and_then(|v| v.as_i64()).unwrap_or(0),
                price: fields.get("price").and_then(|v| v.as_i64()).unwrap_or(0),
                ns: fields.get("dateTime").and_then(|v| v.as_i64()).unwrap_or(0),
                bidder: fields.get("bidder").and_then(|v| v.as_i64()).unwrap_or(0),
            },
            _ => return Ok(()),
        };
        let ns = match &ev {
            Ev::Person { ns, .. } | Ev::Auction { ns } | Ev::Bid { ns, .. } => *ns,
        };
        let b = (((ns - BASE_NS).max(0)) / T_BUCKET_NS).min((BUCKETS - 1) as i64) as usize;
        buckets[b].push(ev);
        Ok(())
    })?;

    let mut sim = Sim::new();
    for bucket in &mut buckets {
        for ev in bucket.drain(..) {
            match ev {
                Ev::Person { id, .. } => sim.person(id),
                Ev::Auction { .. } => sim.auction(),
                Ev::Bid { auc, price, ns, bidder } => sim.bid(auc, price, ns, bidder),
            }
        }
    }
    println!("{}", sim.to_json());
    Ok(())
}
