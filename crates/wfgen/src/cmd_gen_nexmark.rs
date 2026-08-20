//! Deterministic NEXMark event generation (Person/Auction/Bid) as JSONL.
//!
//! Native Rust port of `nexmark_pk/scripts/gen_nexmark.py` so benchmarks do not
//! depend on Python. Emits wfusion JSONL (`_stream`/`_window`/`_timestamp`
//! metadata + event fields, `dateTime` in epoch ns).
//!
//! Output is **globally time-ordered at bucket granularity** (bounded memory):
//! events are generated phase-major (persons → auctions → bids) exactly like
//! the Python port, but each event is routed to one of `TIME_BUCKETS` 30-second
//! time buckets (a temp file per bucket) and emitted bucket-by-bucket, so the
//! stream arrives in approximate event-time order (bucket-internal order is the
//! generation order; a 30 s bucket is far below the 10 min `over` eviction
//! granularity). Batch event-time span therefore drops from ~24 min
//! (phase-major) to <= ~30 s, letting `over`-window time eviction work
//! (previously it never fired: every 100k-row batch contained data near the
//! end of the 30 min span, so `batch.max_ts < watermark - over` never held and
//! the window retained the whole dataset — q1 RSS 21-25GB). The event *set*
//! (rng consumption order, fields) is unchanged — only emission order differs
//! — so EMIT ground truths stay valid.
//!
//! Usage: `wfgen gen-nexmark <count> [--seed N] [--no-sort]`

use rand::Rng;
use rand::SeedableRng;
use rand::rngs::StdRng;
use serde_json::json;

use std::fs::File;
use std::io::Write;

use crate::error::{self, WfgenReason, WfgenResult};

const BASE_NS: i64 = 1767225600000000000; // 2026-01-01T00:00:00Z
const SPAN_NS: i64 = 1_800_000_000_000; // 30 min event span
const PERSONS: usize = 1000;
const HOT_SELLERS: usize = 250; // 25% 卖家为 hot
const HOT_BIDDERS: usize = 250; // 25% 出价者为 hot

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

struct Person {
    state_idx: usize,
}

struct Auction {
    seller: i64,
    category: i64,
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
    Direct(std::io::BufWriter<std::io::Stdout>),
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
fn nx_to_value(ev: &NxEvent) -> serde_json::Value {
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

pub fn generate_events<F>(count: i64, seed: u64, mut emit: F) -> WfgenResult<()>
where
    F: FnMut(NxEvent) -> WfgenResult<()>,
{
    let mut rng = StdRng::seed_from_u64(seed);

    let num_person = (count as f64 * 0.02) as i64;
    let num_auction = (count as f64 * 0.06) as i64;
    let num_bid = count - num_person - num_auction;

    // persons（参考数据，1000，有界）
    let persons: Vec<Person> = (0..PERSONS)
        .map(|_| Person {
            state_idx: rng.random_range(0..STATES.len()),
        })
        .collect();

    // auctions（参考数据，6% 量级，有界；bids 按 auction 引用其属性）
    let auctions: Vec<Auction> = (0..num_auction)
        .map(|_| {
            let hot = rng.random::<f64>() < 0.50;
            let seller = if hot {
                rng.random_range(1..=HOT_SELLERS as i64)
            } else {
                rng.random_range(1..=PERSONS as i64)
            };
            Auction {
                seller,
                category: rng.random_range(1..=26),
                hot,
            }
        })
        .collect();

    // persons（注册集中在前 10% 时间窗）
    for i in 0..num_person {
        let pid = (i % PERSONS as i64) as usize;
        let p = &persons[pid];
        let ns = BASE_NS + rng.random_range(0..=(SPAN_NS / 10));
        let city = rng.random_range(0..CITIES.len());
        emit(NxEvent::Person {
            id: pid as i64 + 1,
            ns,
            city,
            state: p.state_idx,
        })?;
    }

    // auctions（时间窗 10%-100%）
    for i in 0..num_auction {
        let a = &auctions[i as usize];
        let ns = BASE_NS + rng.random_range((SPAN_NS / 10)..=SPAN_NS);
        // 必须显式 i32：原 json! 写法里 10..=1000 无约束→推断 i32（serde to_value
        // 默认），rand 对 i32/i64 采样结果不同，类型不一致会打乱 rng 序列。
        let initial_bid = rng.random_range(10..=1000i32) as i64;
        let reserve = rng.random_range(1000..=10000i32) as i64;
        let expires = ns + rng.random_range(600_000_000_000..=1_800_000_000_000);
        emit(NxEvent::Auction {
            id: i + 1,
            ns,
            initial_bid,
            reserve,
            expires,
            seller: a.seller,
            category: a.category,
        })?;
    }

    // bids（92% firehose，时间窗 20%-100%）
    for _ in 0..num_bid {
        let aidx = rng.random_range(0..auctions.len());
        let a = &auctions[aidx];
        // 同 auction：原 json! 里 price 无约束→i32，需显式 i32 保持 rng 序列。
        let price = if a.hot {
            rng.random_range(100..=500i32)
        } else {
            rng.random_range(10..=150i32)
        } as i64;
        let bidder = if rng.random::<f64>() < 0.5 {
            rng.random_range(1..=HOT_BIDDERS as i64)
        } else {
            rng.random_range(1..=PERSONS as i64)
        };
        let ns = BASE_NS + rng.random_range((SPAN_NS / 5)..=SPAN_NS);
        // 同 auction：原 json! 里 100..=999 无约束→i32，需显式 i32 保持 rng 序列。
        let channel = rng.random_range(0..CHANNELS.len());
        let url = rng.random_range(100..=999i32) as i64;
        emit(NxEvent::Bid {
            auc: aidx as i64 + 1,
            ns,
            price,
            bidder,
            channel,
            url,
        })?;
    }

    Ok(())
}

pub fn run(count: i64, seed: u64, no_sort: bool) -> WfgenResult<()> {
    let sink = if no_sort {
        BucketSink::Direct(std::io::BufWriter::new(std::io::stdout()))
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

    generate_events(count, seed, |ev| {
        write_event(&mut sink, ev.stream(), nx_to_value(&ev), ev.ns())
    })?;

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
            let mut out = std::io::BufWriter::with_capacity(1 << 20, stdout.lock());
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归测试：NxEvent 路径（当前 generate_events）必须与重构前的 json! 写法
    /// 产出完全一致的事件流（rng 序列不变）。
    ///
    /// 背景：重构时把 emit 回调从 `json!` 值改为 `NxEvent` 结构体，导致
    /// `random_range(10..=1000)` 的整数类型推断从 i32（无约束，serde to_value
    /// 默认）变成 i64（结构体字段类型）。rand 对 i32/i64 范围采样结果不同，
    /// auction/bid 序列整体偏移（person 因 city 用 usize 不受影响）。
    /// 本测试用原版 json! 写法作为参照实现，逐事件断言一致性。
    #[test]
    fn nxevent_preserves_json_rng_sequence() {
        const COUNT: i64 = 2000;
        const SEED: u64 = 1;

        // 参照实现：重构前（HEAD）generate_events 的 json! 写法，保持原版类型推断。
        let mut rng = StdRng::seed_from_u64(SEED);
        let num_person = (COUNT as f64 * 0.02) as i64;
        let num_auction = (COUNT as f64 * 0.06) as i64;
        let num_bid = COUNT - num_person - num_auction;

        let persons: Vec<usize> = (0..PERSONS)
            .map(|_| rng.random_range(0..STATES.len()))
            .collect();
        struct RefAuction {
            seller: i64,
            category: i64,
            hot: bool,
        }
        let auctions: Vec<RefAuction> = (0..num_auction)
            .map(|_| {
                let hot = rng.random::<f64>() < 0.50;
                let seller = if hot {
                    rng.random_range(1..=HOT_SELLERS as i64)
                } else {
                    rng.random_range(1..=PERSONS as i64)
                };
                RefAuction {
                    seller,
                    category: rng.random_range(1..=26),
                    hot,
                }
            })
            .collect();

        let mut ref_events: Vec<(String, i64, serde_json::Value)> = Vec::new();
        for i in 0..num_person {
            let pid = (i % PERSONS as i64) as usize;
            let ns = BASE_NS + rng.random_range(0..=(SPAN_NS / 10));
            ref_events.push((
                "person_events".into(),
                ns,
                json!({
                    "id": pid as i64 + 1,
                    "name": format!("person_{}", pid + 1),
                    "email": format!("person{}@example.com", pid + 1),
                    "city": CITIES[rng.random_range(0..CITIES.len())],
                    "state": STATES[persons[pid]],
                    "dateTime": ns,
                }),
            ));
        }
        for i in 0..num_auction {
            let a = &auctions[i as usize];
            let ns = BASE_NS + rng.random_range((SPAN_NS / 10)..=SPAN_NS);
            ref_events.push((
                "auction_events".into(),
                ns,
                json!({
                    "id": i + 1,
                    "itemName": format!("item_{}", i),
                    "description": format!("desc {}", i),
                    "initialBid": rng.random_range(10..=1000),
                    "reserve": rng.random_range(1000..=10000),
                    "dateTime": ns,
                    "expires": ns + rng.random_range(600_000_000_000..=1_800_000_000_000),
                    "seller": a.seller,
                    "category": a.category,
                    "extra": "",
                }),
            ));
        }
        for _ in 0..num_bid {
            let aidx = rng.random_range(0..auctions.len());
            let a = &auctions[aidx];
            let price = if a.hot {
                rng.random_range(100..=500)
            } else {
                rng.random_range(10..=150)
            };
            let bidder = if rng.random::<f64>() < 0.5 {
                rng.random_range(1..=HOT_BIDDERS as i64)
            } else {
                rng.random_range(1..=PERSONS as i64)
            };
            let ns = BASE_NS + rng.random_range((SPAN_NS / 5)..=SPAN_NS);
            ref_events.push((
                "bid_events".into(),
                ns,
                json!({
                    "auction": aidx as i64 + 1,
                    "bidder": bidder,
                    "price": price,
                    "channel": CHANNELS[rng.random_range(0..CHANNELS.len())],
                    "url": format!("http://www.example.com/{}", rng.random_range(100..=999)),
                    "dateTime": ns,
                    "extra": "",
                }),
            ));
        }

        // 目标：当前生产路径。
        let mut got: Vec<(String, i64, serde_json::Value)> = Vec::new();
        generate_events(COUNT, SEED, |ev| {
            got.push((ev.stream().to_string(), ev.ns(), nx_to_value(&ev)));
            Ok(())
        })
        .unwrap();

        assert_eq!(got.len(), ref_events.len(), "事件数不一致");
        for (idx, ((g_stream, g_ns, g_val), (r_stream, r_ns, r_val))) in
            got.iter().zip(ref_events.iter()).enumerate()
        {
            assert_eq!(g_stream, r_stream, "event {idx}: stream 不一致");
            assert_eq!(g_ns, r_ns, "event {idx}: ns 不一致");
            assert_eq!(g_val, r_val, "event {idx}: 字段不一致（rng 序列被破坏）");
        }
    }
}
