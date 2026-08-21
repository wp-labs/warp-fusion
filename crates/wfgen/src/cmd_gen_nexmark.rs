//! Deterministic NEXMark event generation (Person/Auction/Bid) as JSONL.
//!
//! Native Rust NEXMark generator with no Python dependency (replaces the
//! former `nexmark_pk/scripts/gen_nexmark.py` reference). Emits wfusion JSONL
//! (`_stream`/`_window`/`_timestamp` metadata + event fields, `dateTime` in
//! epoch ns).
//!
//! 生成语义对齐 NEXMark 官方 `nexmark/nexmark` generator：
//! - **交错生成**（round-robin）：每 50 个事件 1 person + 3 auction + 46 bid，事件时间严格
//!   递增（等价 outOfOrderGroupSize=1）。person/auction 交错出现在事件流里，窗口 watermark
//!   随事件时间渐进推进，snapshot join 时序正确（phase-major 会让 person 窗口在处理
//!   auction/bid 前就推进到末尾，驱逐早期 seller/bidder 的 person）。
//! - **id 唯一**：person/auction 各自从 1000 起单调递增，每个实体一条记录（不循环 1000 个
//!   id 产生多版本），asof join 因此是 O(1) 查注册时间而非扫 600 版本。
//! - **引用最近**：seller/bidder 引用「最近 60s 内（cold）/ 15s 内（hot）的 person」，
//!   bid.auction 引用「最近 60s 内的 auction」，保证 asof within 60s 命中率 ≈ 100%。
//!
//! Usage: `wfgen gen-nexmark <count> [--seed N] [--no-sort]`

use rand::Rng;
use rand::SeedableRng;
use rand::rngs::StdRng;
use serde_json::json;

use std::fs::File;
use std::io::Write;
use std::sync::Arc;

use crate::error::{self, WfgenReason, WfgenResult};
use crate::progress::fmt_num;

const BASE_NS: i64 = 1767225600000000000; // 2026-01-01T00:00:00Z
const SPAN_NS: i64 = 1_800_000_000_000; // 30 min event span
// NEXMark 官方 id 起始值（person/auction 分属不同命名空间，均可从 1000 起）。
const FIRST_PERSON_ID: i64 = 1000;
const FIRST_AUCTION_ID: i64 = 1000;
// 引用「最近 X 秒内」的实体（时间窗口固定，保证 asof within 60s 命中率不随数据规模退化；
// NEXMark 官方用固定事件速率 + numActivePeople，我们固定 30m span，等价地用固定时间窗）。
const COLD_PERSON_WINDOW_NS: i64 = 60_000_000_000; // cold 引用最近 60s
const HOT_PERSON_WINDOW_NS: i64 = 15_000_000_000; // hot 引用最近 15s
const AUCTION_WINDOW_NS: i64 = 60_000_000_000; // bid 引用最近 60s 的 auction

/// 30-second time buckets over the 30-minute span: 60 temp files, each
/// containing events whose `dateTime` falls in that bucket. Emitting buckets
/// in order yields a globally event-time-sorted stream with bounded memory
/// (one bucket is buffered at a time; a single bucket spans <= 30 s, which is
/// negligible next to the 10-minute `over` eviction granularity).
const BUCKET_NS: i64 = 30_000_000_000;
const TIME_BUCKETS: usize = (SPAN_NS / BUCKET_NS) as usize; // 60

const CITIES: [&str; 8] = [
    "Mountain View",
    "San Francisco",
    "Sunnyvale",
    "New York",
    "Los Angeles",
    "Chicago",
    "Boston",
    "Austin",
];
const STATES: [&str; 8] = ["CA", "CA", "CA", "NY", "CA", "IL", "MA", "TX"];
const CHANNELS: [&str; 5] = ["Google", "Facebook", "Apple", "Direct", "Test"];

struct Auction {
    hot: bool,
}

/// ISO UTC with the same literal `.000Z` millis as the Python generator.
fn iso(ns: i64) -> String {
    let secs = ns.div_euclid(1_000_000_000);
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S.000Z").to_string())
        .unwrap_or_default()
}

/// Emit target: 60 time-bucket temp files (sorted mode) or stdout (legacy
/// phase-major mode). `Sorted` temp files are removed after emission in `run`.
enum BucketSink {
    /// `--no-sort`: phase-major generation order straight to stdout.
    /// Box 里可能包 HashingWriter（`--check` 指纹），stdout lock 非 Send，故不约束 Send。
    Direct(std::io::BufWriter<Box<dyn std::io::Write>>),
    /// Sorted mode: one temp file per 30 s bucket, emitted bucket-by-bucket.
    Sorted {
        writers: Vec<Option<std::io::BufWriter<File>>>,
        tmp_dir: std::path::PathBuf,
    },
}

/// 轻量事件描述：与 JSON 输出字段一一对应（rng 消耗顺序不变），
/// gen 在回调里转 JSON，verify-nexmark 直接用字段（省 JSON 构造/解析）。
#[derive(Debug, Clone, Copy)]
pub enum NxEvent {
    Person {
        id: i64,
        ns: i64,
        city: usize,
        state: usize,
    },
    Auction {
        id: i64,
        ns: i64,
        initial_bid: i64,
        reserve: i64,
        expires: i64,
        seller: i64,
        category: i64,
    },
    Bid {
        auc: i64,
        ns: i64,
        price: i64,
        bidder: i64,
        channel: usize,
        url: i64,
    },
}

impl NxEvent {
    pub fn stream(&self) -> &'static str {
        match self {
            NxEvent::Person { .. } => "person_events",
            NxEvent::Auction { .. } => "auction_events",
            NxEvent::Bid { .. } => "bid_events",
        }
    }

    pub fn ns(&self) -> i64 {
        match self {
            NxEvent::Person { ns, .. } | NxEvent::Auction { ns, .. } | NxEvent::Bid { ns, .. } => {
                *ns
            }
        }
    }
}

/// NxEvent → JSON 字段（不含 _stream/_window/_timestamp 元数据；由 write_event 补）。
pub(crate) fn nx_to_value(ev: &NxEvent) -> serde_json::Value {
    match ev {
        NxEvent::Person {
            id,
            ns,
            city,
            state,
        } => json!({
            "id": id,
            "name": format!("person_{}", id),
            "email": format!("person{}@example.com", id),
            "city": CITIES[*city],
            "state": STATES[*state],
            "dateTime": ns,
        }),
        NxEvent::Auction {
            id,
            ns,
            initial_bid,
            reserve,
            expires,
            seller,
            category,
        } => json!({
            "id": id,
            // 原版语义：id = i + 1，itemName/description 用 0 基 i（= id-1），保持兼容
            "itemName": format!("item_{}", id - 1),
            "description": format!("desc {}", id - 1),
            "initialBid": initial_bid,
            "reserve": reserve,
            "dateTime": ns,
            "expires": expires,
            "seller": seller,
            "category": category,
            "extra": "",
        }),
        NxEvent::Bid {
            auc,
            ns,
            price,
            bidder,
            channel,
            url,
        } => json!({
            "auction": auc,
            "bidder": bidder,
            "price": price,
            "channel": CHANNELS[*channel],
            "url": format!("http://www.example.com/{}", url),
            "dateTime": ns,
            "extra": "",
        }),
    }
}
/// Serialize one event (fields + `_stream`/`_window`/`_timestamp` metadata)
/// and route it to the time bucket covering `ns` (or stdout in `--no-sort`
/// mode). Writes stay bounded: one bucket file is buffered at a time, never
/// the whole event set.
fn write_event(
    sink: &mut BucketSink,
    stream: &str,
    fields: serde_json::Value,
    ns: i64,
) -> WfgenResult<()> {
    let mut m = fields.as_object().cloned().unwrap_or_default();
    m.insert("_stream".to_string(), json!(stream));
    m.insert("_window".to_string(), json!(stream));
    m.insert("_timestamp".to_string(), json!(iso(ns)));
    let value = serde_json::Value::Object(m);
    match sink {
        BucketSink::Direct(out) => {
            serde_json::to_writer(&mut *out, &value).map_err(|e| {
                error::error(WfgenReason::Serialization, format!("serialize event: {e}"))
            })?;
            out.write_all(b"\n")
                .map_err(|e| error::error(WfgenReason::Io, format!("write stdout: {e}")))?;
        }
        BucketSink::Sorted { writers, .. } => {
            let bucket =
                (((ns - BASE_NS).max(0)) / BUCKET_NS).min((TIME_BUCKETS - 1) as i64) as usize;
            let w = writers[bucket]
                .as_mut()
                .expect("bucket writer must exist (see run)");
            serde_json::to_writer(&mut *w, &value).map_err(|e| {
                error::error(WfgenReason::Serialization, format!("serialize event: {e}"))
            })?;
            w.write_all(b"\n")
                .map_err(|e| error::error(WfgenReason::Io, format!("write bucket file: {e}")))?;
        }
    }
    Ok(())
}

/// 「dateTime <= ns 的最近实体（person/auction）的 base0 索引」（实体时间严格递增均匀分布）。
/// 对齐 NEXMark 官方 `lastBase0PersonId`：引用「最近生成的实体」，使 asof join 命中
/// 「时间上最近的注册实体」而非扫描 600 个版本。用 i128 中间计算避免 i64 溢出。
fn idx_before(ns: i64, total: i64) -> i64 {
    if total <= 0 {
        return 0;
    }
    let idx = ((ns - BASE_NS).max(0) as i128 * total as i128 / SPAN_NS as i128) as i64;
    idx.min(total - 1)
}

/// 「dateTime 在 [ns - within_ns, ns] 的实体 base0 索引范围 [lo, hi]」。
fn window_range(ns: i64, total: i64, within_ns: i64) -> (i64, i64) {
    let hi = idx_before(ns, total);
    let lo = idx_before(ns - within_ns, total);
    (lo, hi)
}

/// 在「最近 within_ns 秒内」的实体里随机选一个 base0 索引（含上下界）。
fn pick_in_window(ns: i64, total: i64, within_ns: i64, rng: &mut StdRng) -> i64 {
    let (lo, hi) = window_range(ns, total, within_ns);
    if hi > lo {
        lo + rng.random_range(0..(hi - lo + 1))
    } else {
        lo
    }
}

pub fn generate_events<F>(count: i64, seed: u64, mut emit: F) -> WfgenResult<()>
where
    F: FnMut(NxEvent) -> WfgenResult<()>,
{
    let mut rng = StdRng::seed_from_u64(seed);

    let num_person = (count as f64 * 0.02) as i64;
    let num_auction = (count as f64 * 0.06) as i64;

    // 交错生成（对齐 NEXMark 官方 round-robin）：每 50 个事件 1 person + 3 auction + 46 bid，
    // 事件时间严格递增（等价 NEXMark outOfOrderGroupSize=1）。关键是与官方一样「person/auction
    // 交错出现在事件流里」，让 person/auction 窗口的 watermark 随事件时间渐进推进——否则
    // phase-major 会让 person 窗口在处理 auction/bid 前就推进到 30 分钟末尾，把早期
    // seller/bidder 的 person 驱逐，导致 snapshot join 时序错配（q3 EMIT 从 600k 崩到 ~23k）。
    const PERSON_PROPORTION: i64 = 1;
    const AUCTION_PROPORTION: i64 = 3;
    const BID_PROPORTION: i64 = 46;
    const TOTAL_PROPORTION: i64 = PERSON_PROPORTION + AUCTION_PROPORTION + BID_PROPORTION;

    let mut auctions: Vec<Auction> = Vec::with_capacity(num_auction as usize);

    for event_id in 0..count {
        let rem = event_id % TOTAL_PROPORTION;
        let ns = BASE_NS + (event_id as i128 * SPAN_NS as i128 / count as i128) as i64;

        if rem < PERSON_PROPORTION {
            // person：id 唯一（FIRST_PERSON_ID 起），每个 person 一条记录。
            let person_idx = event_id / TOTAL_PROPORTION;
            emit(NxEvent::Person {
                id: FIRST_PERSON_ID + person_idx,
                ns,
                city: rng.random_range(0..CITIES.len()),
                state: rng.random_range(0..STATES.len()),
            })?;
        } else if rem < PERSON_PROPORTION + AUCTION_PROPORTION {
            // auction：id 唯一，seller 引用最近 person。
            let auction_idx =
                (event_id / TOTAL_PROPORTION) * AUCTION_PROPORTION + (rem - PERSON_PROPORTION);
            let hot = rng.random::<f64>() < 0.50;
            let seller = pick_in_window(
                ns,
                num_person,
                if hot {
                    HOT_PERSON_WINDOW_NS
                } else {
                    COLD_PERSON_WINDOW_NS
                },
                &mut rng,
            ) + FIRST_PERSON_ID;
            // 必须显式 i32：原 json! 写法里 10..=1000 无约束→推断 i32，类型不一致会打乱 rng 序列。
            let initial_bid = rng.random_range(10..=1000i32) as i64;
            let reserve = rng.random_range(1000..=10000i32) as i64;
            let expires = ns + rng.random_range(600_000_000_000..=1_800_000_000_000);
            let category = rng.random_range(1..=26);
            auctions.push(Auction { hot });
            emit(NxEvent::Auction {
                id: FIRST_AUCTION_ID + auction_idx,
                ns,
                initial_bid,
                reserve,
                expires,
                seller,
                category,
            })?;
        } else {
            // bid：auction 引用最近 auction，bidder 引用最近 person。
            let auc_base0 = pick_in_window(ns, num_auction, AUCTION_WINDOW_NS, &mut rng);
            let a = &auctions[auc_base0 as usize];
            let price = if a.hot {
                rng.random_range(100..=500i32)
            } else {
                rng.random_range(10..=150i32)
            } as i64;
            let bidder = pick_in_window(
                ns,
                num_person,
                if rng.random::<f64>() < 0.5 {
                    HOT_PERSON_WINDOW_NS
                } else {
                    COLD_PERSON_WINDOW_NS
                },
                &mut rng,
            ) + FIRST_PERSON_ID;
            let channel = rng.random_range(0..CHANNELS.len());
            let url = rng.random_range(100..=999i32) as i64;
            emit(NxEvent::Bid {
                auc: auc_base0 + FIRST_AUCTION_ID,
                ns,
                price,
                bidder,
                channel,
                url,
            })?;
        }
    }

    Ok(())
}

pub fn run(count: i64, seed: u64, no_sort: bool) -> WfgenResult<()> {
    run_checked(count, seed, no_sort, false)
}

/// 包装 writer：写 inner 的同时累计输出字节 md5（指纹）。
struct HashingWriter<W: std::io::Write> {
    inner: W,
    hasher: Arc<std::sync::Mutex<md5::Md5>>,
}

impl<W: std::io::Write> HashingWriter<W> {
    fn new(inner: W, hasher: Arc<std::sync::Mutex<md5::Md5>>) -> Self {
        Self { inner, hasher }
    }
}

impl<W: std::io::Write> std::io::Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        use md5::Digest;
        let n = self.inner.write(buf)?;
        self.hasher.lock().unwrap().update(&buf[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// 单事件值域自检：返回该事件是否违规（字段范围须与 `generate_events` 的
/// 生成语义一致——person/auction id 唯一（FIRST_*_ID 起），seller/bidder 引用
/// person id，bid.auction 引用 auction id）。
fn check_event(ev: &NxEvent, count: i64) -> bool {
    let num_person = (count as f64 * 0.02) as i64;
    let num_auction = (count as f64 * 0.06) as i64;
    let person_hi = FIRST_PERSON_ID + num_person - 1;
    let auction_hi = FIRST_AUCTION_ID + num_auction - 1;
    match ev {
        NxEvent::Person {
            id, city, state, ..
        } => {
            !(FIRST_PERSON_ID..=person_hi).contains(id)
                || *city >= CITIES.len()
                || *state >= STATES.len()
        }
        NxEvent::Auction {
            id,
            ns,
            initial_bid,
            reserve,
            expires,
            seller,
            category,
            ..
        } => {
            !(FIRST_AUCTION_ID..=auction_hi).contains(id)
                || !(10..=1000).contains(initial_bid)
                || !(1000..=10000).contains(reserve)
                || expires < ns
                || !(FIRST_PERSON_ID..=person_hi).contains(seller)
                || !(1..=26).contains(category)
        }
        NxEvent::Bid {
            auc,
            price,
            bidder,
            channel,
            url,
            ..
        } => {
            !(FIRST_AUCTION_ID..=auction_hi).contains(auc)
                || !(FIRST_PERSON_ID..=person_hi).contains(bidder)
                || *channel >= CHANNELS.len()
                || !(100..=999).contains(url)
                || *price < 10
        }
    }
}

/// 生成 + 可选自检（`--check`）：生成阶段（进度条）→ 数据检查阶段
/// （`--check` 独立进度条：同一 seed 确定性重放事件流，逐事件值域校验）
/// + 输出字节 md5 指纹（确定性锚点：同 seed+count 恒等；桶序模式 = 输出文件 md5）。
/// 报告写 stderr（stdout 是数据流）：默认输出简短质量报告（行数/时间/乱序），
/// `--check` 追加值域违规与 md5 指纹。
pub fn run_checked(count: i64, seed: u64, no_sort: bool, check: bool) -> WfgenResult<()> {
    // 轻量统计（默认报告用，几乎零成本）：流计数 + 时间戳范围 + 乱序深度。
    let mut n_person: i64 = 0;
    let mut n_auction: i64 = 0;
    let mut n_bid: i64 = 0;
    let mut min_ns: i64 = i64::MAX;
    let mut max_ns: i64 = i64::MIN;
    // 流序统计：桶序模式下输出 = 桶间严格递增 + 桶内乱序，故每桶独立 watermark
    // （等价于对实际输出流统计，零解析成本）；no-sort 模式用全局 watermark
    // （phase-major 乱序可达 ~27min，远超桶宽——这是它破坏 over 驱逐的机理）。
    let mut seq_max_bucket: [i64; TIME_BUCKETS] = [i64::MIN; TIME_BUCKETS];
    let mut seq_max_global: i64 = i64::MIN;
    let mut max_oog_ns: i64 = 0;
    let mut oog_count: u64 = 0;
    let fingerprint: Option<Arc<std::sync::Mutex<md5::Md5>>> =
        check.then(|| Arc::new(std::sync::Mutex::new(md5::Md5::default())));

    let sink = if no_sort {
        let stdout = std::io::stdout();
        let inner: Box<dyn std::io::Write> = match &fingerprint {
            Some(h) => Box::new(HashingWriter::new(stdout.lock(), Arc::clone(h))),
            None => Box::new(stdout.lock()),
        };
        BucketSink::Direct(std::io::BufWriter::with_capacity(1 << 20, inner))
    } else {
        let tmp_dir =
            std::env::temp_dir().join(format!("wfgen_nexmark_{}_{}", std::process::id(), seed));
        let _ = std::fs::remove_dir_all(&tmp_dir);
        std::fs::create_dir_all(&tmp_dir)
            .map_err(|e| error::error(WfgenReason::Io, format!("create bucket temp dir: {e}")))?;
        let writers = (0..TIME_BUCKETS)
            .map(|i| {
                let f = File::create(tmp_dir.join(format!("b{i:03}.jsonl"))).map_err(|e| {
                    error::error(WfgenReason::Io, format!("create bucket file: {e}"))
                })?;
                Ok(Some(std::io::BufWriter::with_capacity(1 << 20, f)))
            })
            .collect::<WfgenResult<Vec<_>>>()?;
        BucketSink::Sorted { writers, tmp_dir }
    };
    let mut sink = sink;

    // 生成阶段进度条（stderr、仅 TTY；stdout 是数据流不被污染）。
    let pb = crate::progress::ProgressBar::new(count as u64, "gen-nexmark");
    generate_events(count, seed, |ev| {
        pb.tick();
        // 轻量统计（默认报告）：流计数 + 时间戳范围 + 乱序深度。
        let ns = ev.ns();
        match ev {
            NxEvent::Person { .. } => n_person += 1,
            NxEvent::Auction { .. } => n_auction += 1,
            NxEvent::Bid { .. } => n_bid += 1,
        }
        min_ns = min_ns.min(ns);
        max_ns = max_ns.max(ns);
        if no_sort {
            if ns < seq_max_global {
                oog_count += 1;
                max_oog_ns = max_oog_ns.max(seq_max_global - ns);
            } else {
                seq_max_global = ns;
            }
        } else {
            let b = (((ns - BASE_NS).max(0)) / BUCKET_NS).min((TIME_BUCKETS - 1) as i64) as usize;
            if ns < seq_max_bucket[b] {
                oog_count += 1;
                max_oog_ns = max_oog_ns.max(seq_max_bucket[b] - ns);
            } else {
                seq_max_bucket[b] = ns;
            }
        }
        write_event(&mut sink, ev.stream(), nx_to_value(&ev), ev.ns())
    })?;
    pb.finish();

    // 数据检查阶段（仅 --check）：同一 seed 确定性重放事件流，逐事件值域校验 + 跨事件
    // 规则校验（person/auction id 唯一、seller/bidder 引用「最近 person」、bid.auction
    // 引用已存在 auction）。事件序列与生成阶段完全一致（generate_events 是纯函数，同 seed
    // 恒等），检查独立成阶段以拥有自己的进度条；报告仍走 stderr（stdout 是数据流）。
    let mut violations: u64 = 0;
    let mut v_person: u64 = 0;
    let mut v_auction: u64 = 0;
    let mut v_bid: u64 = 0;
    let mut ref_violations: u64 = 0;
    if check {
        use std::collections::{HashMap, HashSet};

        let mut person_ids: HashSet<i64> = HashSet::new();
        let mut person_time: HashMap<i64, i64> = HashMap::new();
        let mut auction_ids: HashSet<i64> = HashSet::new();
        let pb_check = crate::progress::ProgressBar::new(count as u64, "check: 值域+引用校验");
        generate_events(count, seed, |ev| {
            pb_check.tick();
            if check_event(&ev, count) {
                violations += 1;
                match ev {
                    NxEvent::Person { .. } => v_person += 1,
                    NxEvent::Auction { .. } => v_auction += 1,
                    NxEvent::Bid { .. } => v_bid += 1,
                }
            }
            // 跨事件规则：id 唯一 + 引用「最近 person/auction」（被引用实体已生成且
            // dateTime <= 当前事件时间）。这锁定了 NEXMark 官方生成语义。
            match ev {
                NxEvent::Person { id, ns, .. } => {
                    if !person_ids.insert(id) {
                        ref_violations += 1;
                    }
                    person_time.insert(id, ns);
                }
                NxEvent::Auction { id, ns, seller, .. } => {
                    if !auction_ids.insert(id) {
                        ref_violations += 1;
                    }
                    match person_time.get(&seller) {
                        Some(&seller_ns) if seller_ns <= ns => {}
                        _ => ref_violations += 1,
                    }
                }
                NxEvent::Bid {
                    auc, ns, bidder, ..
                } => {
                    if !auction_ids.contains(&auc) {
                        ref_violations += 1;
                    }
                    match person_time.get(&bidder) {
                        Some(&bidder_ns) if bidder_ns <= ns => {}
                        _ => ref_violations += 1,
                    }
                }
            }
            Ok(())
        })?;
        pb_check.finish();
    }

    match sink {
        BucketSink::Direct(mut out) => {
            out.flush()
                .map_err(|e| error::error(WfgenReason::Io, format!("flush stdout: {e}")))?;
        }
        BucketSink::Sorted { writers, tmp_dir } => {
            // Close all bucket files, then emit bucket-by-bucket in order. No
            // per-bucket sort: a 30 s bucket is far below the 10 min `over`
            // eviction granularity, so bucket-internal generation order is
            // fine (batch span at 100M density ≈ seconds; previously phase-
            // major emission made it ~24 min and time eviction never fired).
            // Skipping the sort avoids re-reading + parsing + sorting the
            // whole 10GB+ event set. Temp dir removed afterwards.
            drop(writers);
            let stdout = std::io::stdout();
            let inner: Box<dyn std::io::Write> = match &fingerprint {
                Some(h) => Box::new(HashingWriter::new(stdout.lock(), Arc::clone(h))),
                None => Box::new(stdout.lock()),
            };
            let mut out = std::io::BufWriter::with_capacity(1 << 20, inner);
            for i in 0..TIME_BUCKETS {
                let path = tmp_dir.join(format!("b{i:03}.jsonl"));
                let data = std::fs::read(&path)
                    .map_err(|e| error::error(WfgenReason::Io, format!("read bucket file: {e}")))?;
                out.write_all(&data)
                    .map_err(|e| error::error(WfgenReason::Io, format!("write stdout: {e}")))?;
            }
            out.flush()
                .map_err(|e| error::error(WfgenReason::Io, format!("flush stdout: {e}")))?;
            let _ = std::fs::remove_dir_all(&tmp_dir);
        }
    }

    // 数据质量报告（stderr；stdout 是数据流）：默认输出行数/时间范围/乱序
    // 三项轻量指标；--check 追加值域违规与 md5 指纹。
    let expect_person = (count as f64 * 0.02) as i64;
    let expect_auction = (count as f64 * 0.06) as i64;
    let expect_bid = count - expect_person - expect_auction;
    let rows_ok = n_person == expect_person && n_auction == expect_auction && n_bid == expect_bid;
    let ns_ok = min_ns >= BASE_NS && max_ns <= BASE_NS + SPAN_NS;
    let total = n_person + n_auction + n_bid;
    eprintln!(
        "== gen-nexmark {} {} ==",
        fmt_num(count as u64),
        if check { "--check" } else { "(质量报告)" }
    );
    eprintln!(
        "  行数  {}（person {} / auction {} / bid {}）{}",
        fmt_num(total as u64),
        fmt_num(n_person as u64),
        fmt_num(n_auction as u64),
        fmt_num(n_bid as u64),
        if rows_ok {
            "✅"
        } else {
            "❌ 与期望比例不符"
        }
    );
    eprintln!(
        "  时间  [{}s, {}s] / 30m span {}",
        (min_ns - BASE_NS) / 1_000_000_000,
        (max_ns - BASE_NS) / 1_000_000_000,
        if ns_ok { "✅" } else { "❌ 越界" }
    );
    eprintln!(
        "  乱序  {} 事件（{:.2}%）最大乱序 {:.1}s {}",
        fmt_num(oog_count),
        100.0 * oog_count as f64 / total as f64,
        max_oog_ns as f64 / 1_000_000_000.0,
        if oog_count == 0 {
            "✅ 事件时间严格递增（对齐 NEXMark outOfOrderGroupSize=1）"
        } else {
            "❌ 出现乱序"
        }
    );
    if check {
        eprintln!(
            "  违规  {}（person {} / auction {} / bid {}）{}",
            fmt_num(violations),
            fmt_num(v_person),
            fmt_num(v_auction),
            fmt_num(v_bid),
            if violations == 0 { "✅" } else { "❌" }
        );
        eprintln!(
            "  引用  {}（person/auction id 唯一 + seller/bidder 引用最近 person + bid 引用已存在 auction）{}",
            fmt_num(ref_violations),
            if ref_violations == 0 { "✅" } else { "❌" }
        );
        let fp = fingerprint
            .as_ref()
            .map(|h| {
                use md5::Digest;
                let digest = h.lock().unwrap().clone().finalize();
                format!("{:x}", digest)
            })
            .unwrap_or_default();
        eprintln!(
            "  指纹  {} {}",
            fp,
            if fp.is_empty() {
                "（未计算）"
            } else {
                "✅ 同 seed+count 输出字节恒等"
            }
        );
        // 数据符合性声明（对照 Flink 官方 nexmark/nexmark 默认配置，静态结论）。
        eprintln!();
        eprint!("{}", crate::nexmark_conformance::report(false));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归：generate_events 是纯函数——同一 seed 重放产出字节一致的事件流。
    /// 这是 ground truth 可对同一数据独立复算的前提（--check 阶段依赖同 seed 重放）。
    #[test]
    fn generate_events_is_deterministic() {
        const COUNT: i64 = 2000;
        const SEED: u64 = 1;

        let mut a: Vec<(String, i64, serde_json::Value)> = Vec::new();
        generate_events(COUNT, SEED, |ev| {
            a.push((ev.stream().to_string(), ev.ns(), nx_to_value(&ev)));
            Ok(())
        })
        .unwrap();

        let mut b: Vec<(String, i64, serde_json::Value)> = Vec::new();
        generate_events(COUNT, SEED, |ev| {
            b.push((ev.stream().to_string(), ev.ns(), nx_to_value(&ev)));
            Ok(())
        })
        .unwrap();

        assert_eq!(a, b, "同一 seed 重放应产出完全一致的事件流");
    }

    /// 回归：id/auc/seller/bidder 值域须与生成语义一致——person/auction id 唯一
    /// （FIRST_*_ID 起），seller/bidder 引用 person id，bid.auction 引用 auction id。
    #[test]
    fn check_event_bounds_match_generation_semantics() {
        let count = 10_000i64;
        let num_person = (count as f64 * 0.02) as i64; // 200
        let num_auction = (count as f64 * 0.06) as i64; // 600
        let person_hi = FIRST_PERSON_ID + num_person - 1; // 1199
        let auction_hi = FIRST_AUCTION_ID + num_auction - 1; // 1599

        // 边界值合法。
        assert!(!check_event(
            &NxEvent::Auction {
                id: auction_hi,
                ns: BASE_NS,
                initial_bid: 10,
                reserve: 1000,
                expires: BASE_NS + 1_000_000_000,
                seller: FIRST_PERSON_ID,
                category: 1,
            },
            count
        ));
        assert!(!check_event(
            &NxEvent::Bid {
                auc: auction_hi,
                ns: BASE_NS,
                price: 10,
                bidder: person_hi,
                channel: 0,
                url: 100,
            },
            count
        ));
        assert!(!check_event(
            &NxEvent::Person {
                id: person_hi,
                ns: BASE_NS,
                city: CITIES.len() - 1,
                state: STATES.len() - 1,
            },
            count
        ));

        // 越界必须报违规。
        assert!(check_event(
            &NxEvent::Auction {
                id: auction_hi + 1,
                ns: BASE_NS,
                initial_bid: 10,
                reserve: 1000,
                expires: BASE_NS + 1_000_000_000,
                seller: FIRST_PERSON_ID,
                category: 1,
            },
            count
        ));
        assert!(check_event(
            &NxEvent::Bid {
                auc: FIRST_AUCTION_ID - 1, // 低于 auction id 下限
                ns: BASE_NS,
                price: 10,
                bidder: person_hi,
                channel: 0,
                url: 100,
            },
            count
        ));
        assert!(check_event(
            &NxEvent::Bid {
                auc: auction_hi,
                ns: BASE_NS,
                price: 9, // < 10 下限
                bidder: person_hi,
                channel: 0,
                url: 100,
            },
            count
        ));
        assert!(check_event(
            &NxEvent::Person {
                id: person_hi + 1, // 超出 person id 上限
                ns: BASE_NS,
                city: 0,
                state: 0,
            },
            count
        ));
    }

    /// 回归：--check 与生成器自洽——真实 `generate_events` 产出的事件
    /// 必须全部通过 `check_event`（否则自检与生成语义脱节，产生假违规）。
    #[test]
    fn generated_events_pass_self_check() {
        let count = 10_000i64;
        let mut violations = 0u64;
        generate_events(count, 7, |ev| {
            if check_event(&ev, count) {
                violations += 1;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(violations, 0, "生成事件不应触发自检违规");
    }

    /// 性质测试：生成数据满足 NEXMark 官方引用规则——person/auction id 唯一，
    /// seller/bidder 引用「dateTime <= 事件时间的已生成 person」，bid.auction 引用
    /// 已生成 auction。这是 asof join O(1) 语义的数据前提。
    #[test]
    fn generated_events_pass_reference_rules() {
        use std::collections::{HashMap, HashSet};

        let count = 50_000i64;
        let mut person_ids: HashSet<i64> = HashSet::new();
        let mut person_time: HashMap<i64, i64> = HashMap::new();
        let mut auction_ids: HashSet<i64> = HashSet::new();
        let mut violations = 0u64;

        generate_events(count, 7, |ev| {
            match ev {
                NxEvent::Person { id, ns, .. } => {
                    if !person_ids.insert(id) {
                        violations += 1;
                    }
                    person_time.insert(id, ns);
                }
                NxEvent::Auction { id, ns, seller, .. } => {
                    if !auction_ids.insert(id) {
                        violations += 1;
                    }
                    match person_time.get(&seller) {
                        Some(&seller_ns) if seller_ns <= ns => {}
                        _ => violations += 1,
                    }
                }
                NxEvent::Bid {
                    auc, ns, bidder, ..
                } => {
                    if !auction_ids.contains(&auc) {
                        violations += 1;
                    }
                    match person_time.get(&bidder) {
                        Some(&bidder_ns) if bidder_ns <= ns => {}
                        _ => violations += 1,
                    }
                }
            }
            Ok(())
        })
        .unwrap();

        assert_eq!(violations, 0, "生成数据应满足 id 唯一 + 引用最近规则");
    }

    /// 性质测试：q22 的 asof join `within 60s` 命中率应接近 100%。
    /// NEXMark 官方 bidder 引用「最近注册的 person」，其 dateTime 紧贴 bid 时间，
    /// 因此每个 bid 都能在 60s 内命中对应 person——这是 O(1) asof join 的数据前提。
    #[test]
    fn asof_join_within_60s_hit_rate_is_near_100() {
        use std::collections::HashMap;

        const WITHIN_NS: i64 = 60_000_000_000;
        let count = 100_000i64;
        let mut person_time: HashMap<i64, i64> = HashMap::new();
        let mut bids = 0u64;
        let mut hits = 0u64;

        generate_events(count, 7, |ev| {
            match ev {
                NxEvent::Person { id, ns, .. } => {
                    person_time.insert(id, ns);
                }
                NxEvent::Bid { bidder, ns, .. } => {
                    bids += 1;
                    if let Some(&p_ns) = person_time.get(&bidder)
                        && p_ns <= ns
                        && p_ns >= ns - WITHIN_NS
                    {
                        hits += 1;
                    }
                }
                _ => {}
            }
            Ok(())
        })
        .unwrap();

        let hit_rate = hits as f64 / bids as f64;
        assert!(
            hit_rate > 0.99,
            "asof within 60s 命中率应接近 100%，实际 {:.2}%",
            hit_rate * 100.0
        );
    }

    /// 性质测试：交错生成的事件时间严格递增（等价 NEXMark outOfOrderGroupSize=1），
    /// 输出流无乱序——这是 person/auction 窗口 watermark 渐进推进、snapshot join 时序
    /// 正确（不被 phase-major 提前推进 watermark 而破坏）的前提。
    #[test]
    fn emission_is_time_ordered() {
        let count = 50_000i64;
        let mut seq_max = i64::MIN;
        let mut oog_count = 0u64;
        generate_events(count, 7, |ev| {
            let ns = ev.ns();
            if ns < seq_max {
                oog_count += 1;
            } else {
                seq_max = ns;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(oog_count, 0, "交错生成的事件时间应严格递增（无乱序）");
    }
}
