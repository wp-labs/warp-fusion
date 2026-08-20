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
use std::collections::BinaryHeap;
use std::sync::Arc;
use std::thread;

use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::json;

use crate::cmd_gen_nexmark::{NxEvent, generate_events};
use crate::error::WfgenResult;

const SPAN: i64 = 600_000_000_000; // 10m sliding window
const WITHIN_60S: i64 = 60_000_000_000; // q19 seq within 60s
const BUCKET_NS: i64 = 600_000_000_000; // q16 fixed bucket
const NONE: i64 = -1; // "no instance" sentinel for created/max slots

/// 分片并行的事件载体：`wm` 是处理该事件时**全局** watermark（按 30s 桶序的
/// 前缀 max ns，含当前事件），与单线程 `if ns > watermark { watermark = ns }`
/// 语义完全一致——否则分片各自的局部 watermark 会滞后，过期判定漂移。
struct ShardedEvent {
    ev: NxEvent,
    wm: i64,
}

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
    state: FxHashMap<i64, [i64; 15]>,
    heap: BinaryHeap<Reverse<(i64, i64, usize)>>, // (expire_at, auc, slot)
    pending: FxHashSet<(i64, usize)>,
    // q15-q20 independent rule state (own machines, shared lazy heap)
    q15_count: FxHashMap<i64, i64>,
    q15_created: FxHashMap<i64, i64>,
    q17_set: FxHashMap<i64, FxHashSet<i64>>,
    q17_created: FxHashMap<i64, i64>,
    q18_count: FxHashMap<i64, i64>,
    q18_created: FxHashMap<i64, i64>,
    q19_step: FxHashMap<i64, i64>,
    q19_t0: FxHashMap<i64, i64>,
    q19_created: FxHashMap<i64, i64>,
    q20_count: FxHashMap<i64, i64>,
    q20_created: FxHashMap<i64, i64>,
    heap2: BinaryHeap<Reverse<(i64, &'static str, i64)>>,
    pending2: FxHashSet<(&'static str, i64)>,
    // q16 fixed buckets
    q16_sum: FxHashMap<(i64, i64), i64>,
}

impl Default for Sim {
    fn default() -> Self {
        Self::new()
    }
}

impl Sim {
    fn new() -> Self {
        Self::with_capacity(1024)
    }

    /// `auction_cap` 是预期的 auction 数（num_auction）；各 per-key 状态按此预分配
    /// 容量，避免 30M/100M 规模下大量 rehash。
    fn with_capacity(auction_cap: usize) -> Self {
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
            state: FxHashMap::with_capacity_and_hasher(auction_cap, Default::default()),
            heap: BinaryHeap::new(),
            pending: FxHashSet::with_capacity_and_hasher(auction_cap * 4, Default::default()),
            q15_count: FxHashMap::with_capacity_and_hasher(auction_cap, Default::default()),
            q15_created: FxHashMap::with_capacity_and_hasher(auction_cap, Default::default()),
            q17_set: FxHashMap::with_capacity_and_hasher(auction_cap, Default::default()),
            q17_created: FxHashMap::with_capacity_and_hasher(auction_cap, Default::default()),
            q18_count: FxHashMap::with_capacity_and_hasher(auction_cap, Default::default()),
            q18_created: FxHashMap::with_capacity_and_hasher(auction_cap, Default::default()),
            q19_step: FxHashMap::with_capacity_and_hasher(auction_cap, Default::default()),
            q19_t0: FxHashMap::with_capacity_and_hasher(auction_cap, Default::default()),
            q19_created: FxHashMap::with_capacity_and_hasher(auction_cap, Default::default()),
            q20_count: FxHashMap::with_capacity_and_hasher(auction_cap, Default::default()),
            q20_created: FxHashMap::with_capacity_and_hasher(auction_cap, Default::default()),
            heap2: BinaryHeap::new(),
            pending2: FxHashSet::with_capacity_and_hasher(auction_cap * 4, Default::default()),
            q16_sum: FxHashMap::with_capacity_and_hasher(auction_cap * 2, Default::default()),
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
            let cur = self.state.get(&auc).and_then(|st| {
                if st[ci] != NONE {
                    Some(st[ci] + SPAN)
                } else {
                    None
                }
            });
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

    fn bid(
        &mut self,
        auc: i64,
        price: i64,
        ns: i64,
        bidder: i64,
        wm: i64,
        persons: &FxHashSet<i64>,
    ) {
        self.n_bid += 1;
        if auc % 123 == 0 {
            self.q2 += 1;
        }
        if auc % 7 == 0 {
            self.q10 += 1;
        }
        self.q13 += 1;

        // watermark 由分片并行调用方预计算（全局 30s 桶序前缀 max ns，含本事件）。
        self.sweep(wm);
        self.sweep2(wm);

        let st = self.state.entry(auc).or_insert([
            NONE, 0, NONE, 0, NONE, 0, NONE, NONE, NONE, NONE, NONE, NONE, NONE, 0, 0,
        ]);

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
            st[mi] = if st[mi] == NONE {
                price
            } else {
                st[mi].max(price)
            };
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
            self.q17_set.insert(auc, FxHashSet::default());
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
        if !persons.contains(&bidder) {
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

/// 分片并行主流程。`n_shards` 只影响并行度，不改变结果（wm 全局预计算）。
fn run_sharded(count: i64, seed: u64, n_shards: usize) -> WfgenResult<()> {
    // 与 `gen-nexmark` 相同的 30s 桶序：事件按桶收集（桶内生成序），再按桶序
    // 喂模拟器——否则 phase-major 生成序会让滑动窗口/watermark 对拍失真。
    const T_BUCKET_NS: i64 = 30_000_000_000;
    const BASE_NS: i64 = 1767225600000000000; // 与 cmd_gen_nexmark::BASE_NS 一致
    const BUCKETS: usize = 60;
    let mut buckets: Vec<Vec<NxEvent>> = (0..BUCKETS).map(|_| Vec::new()).collect();
    // 生成阶段进度条（stderr、仅 TTY）。
    let pb_gen = crate::progress::ProgressBar::new(count as u64, "verify: 生成事件");
    generate_events(count, seed, |ev| {
        pb_gen.tick();
        let ns = ev.ns();
        let b = (((ns - BASE_NS).max(0)) / T_BUCKET_NS).min((BUCKETS - 1) as i64) as usize;
        buckets[b].push(ev);
        Ok(())
    })?;
    pb_gen.finish();

    // 第二遍：按桶序扫描，预计算全局前缀 max ns（含当前 bid，即单线程
    // `if ns > watermark { watermark = ns }` 之后的 watermark——注意原版
    // person/auction 事件不推进 watermark），同时把 bid 路由到分片。
    // person/auction 只做无状态处理；桶逐个释放控制峰值内存。
    let mut shards: Vec<Vec<ShardedEvent>> = (0..n_shards).map(|_| Vec::new()).collect();
    let mut person_ids: FxHashSet<i64> =
        FxHashSet::with_capacity_and_hasher(1024, Default::default());
    let mut q8: i64 = 0;
    let mut n_auction: i64 = 0;
    let mut cur_max: i64 = i64::MIN;
    for b in 0..BUCKETS {
        let evs = std::mem::take(&mut buckets[b]);
        for ev in evs {
            let ns = ev.ns();
            match ev {
                NxEvent::Person { id, .. } => {
                    q8 += 1;
                    person_ids.insert(id);
                }
                NxEvent::Auction { .. } => {
                    n_auction += 1;
                }
                NxEvent::Bid { auc, .. } => {
                    cur_max = cur_max.max(ns);
                    let s = (auc as u64 % n_shards as u64) as usize;
                    shards[s].push(ShardedEvent { ev, wm: cur_max });
                }
            }
        }
    }
    drop(buckets);

    // 并行处理：每分片一个 Sim，状态按 auction 隔离，watermark 用预存全局值。
    let persons = Arc::new(person_ids);
    let n_bid = count - (count as f64 * 0.02) as i64 - (count as f64 * 0.06) as i64;
    let per_shard_cap = (n_auction as usize / n_shards).max(1);
    let pb_sim = crate::progress::ProgressBar::new(n_bid as u64, "verify: 模拟");
    let sim_counter = pb_sim.counter();
    let mut sims: Vec<Sim> = thread::scope(|scope| {
        let handles: Vec<_> = shards
            .into_iter()
            .map(|shard| {
                let persons = Arc::clone(&persons);
                let counter = Arc::clone(&sim_counter);
                scope.spawn(move || {
                    let mut sim = Sim::with_capacity(per_shard_cap);
                    for se in shard {
                        if let NxEvent::Bid {
                            auc,
                            ns,
                            price,
                            bidder,
                            ..
                        } = se.ev
                        {
                            sim.bid(auc, price, ns, bidder, se.wm, &persons);
                        }
                        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    sim
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    pb_sim.finish();

    // 合并分片计数（q16_sum 按 (auc, bucket) key 求和，不同分片 key 不相交）。
    let mut merged = sims.remove(0);
    for s in sims {
        merged.q2 += s.q2;
        merged.n_bid += s.n_bid;
        for i in 0..3 {
            merged.q5[i] += s.q5[i];
            merged.q7[i] += s.q7[i];
        }
        merged.q6 += s.q6;
        merged.q10 += s.q10;
        merged.q13 += s.q13;
        merged.q15 += s.q15;
        merged.q17 += s.q17;
        merged.q18 += s.q18;
        merged.q19 += s.q19;
        merged.q20 += s.q20;
        merged.q21 += s.q21;
        for (k, v) in s.q16_sum {
            *merged.q16_sum.entry(k).or_insert(0) += v;
        }
    }
    merged.q8 = q8;
    merged.n_auction = n_auction;

    println!("{}", merged.to_json());
    Ok(())
}

pub fn run(count: i64, seed: u64) -> WfgenResult<()> {
    let n_shards = std::env::var("WFGEN_VERIFY_SHARDS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or_else(|| {
            thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .min(64)
        });
    run_sharded(count, seed, n_shards)
}
