//! Deterministic NEXMark event generation (Person/Auction/Bid) as JSONL.
//!
//! Native Rust port of `nexmark_pk/scripts/gen_nexmark.py` so benchmarks do not
//! depend on Python. Emits wfusion JSONL (`_stream`/`_window`/`_timestamp`
//! metadata + event fields, `dateTime` in epoch ns).
//!
//! Output is **globally time-ordered** (bucket-sorted, bounded memory): events
//! are generated phase-major (persons → auctions → bids) exactly like the
//! Python port, but each event is routed to one of `TIME_BUCKETS` 30-second
//! time buckets (a temp file per bucket) and emitted bucket-by-bucket, so the
//! stream arrives in event-time order. Batch event-time span therefore drops
//! from ~24 min (phase-major) to a few seconds at benchmark density (100M rows
//! / 30 min ≈ 1.8 s per 100k-row batch), letting `over`-window time eviction
//! work (previously it never fired: every 100k-row batch contained data near
//! the end of the 30 min span, so `batch.max_ts < watermark - over` never
//! held and the window retained the whole dataset — q1 RSS 21-25GB). The event
//! *set* (rng consumption order, fields) is unchanged — only emission order
//! differs — so EMIT ground truths stay valid.
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
    state: &'static str,
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

/// Extract the `dateTime` epoch-ns from one JSONL line without a full JSON
/// parse (hot path: once per emitted event, 100M+ events).
fn extract_date_time_ns(line: &[u8]) -> Option<i64> {
    const KEY: &[u8] = b"\"dateTime\":";
    let idx = line.windows(KEY.len()).position(|w| w == KEY)?;
    let rest = &line[idx + KEY.len()..];
    let end = rest
        .iter()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(rest.len());
    std::str::from_utf8(&rest[..end]).ok()?.parse().ok()
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

pub fn run(count: i64, seed: u64, no_sort: bool) -> WfgenResult<()> {
    let mut rng = StdRng::seed_from_u64(seed);

    let num_person = (count as f64 * 0.02) as i64;
    let num_auction = (count as f64 * 0.06) as i64;
    let num_bid = count - num_person - num_auction;

    // persons（参考数据，1000，有界）
    let persons: Vec<Person> = (0..PERSONS)
        .map(|_| Person {
            state: STATES[rng.random_range(0..STATES.len())],
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

    // persons（注册集中在前 10% 时间窗）
    for i in 0..num_person {
        let pid = (i % PERSONS as i64) as usize;
        let p = &persons[pid];
        let ns = BASE_NS + rng.random_range(0..=(SPAN_NS / 10));
        write_event(
            &mut sink,
            "person_events",
            json!({
                "id": pid as i64 + 1,
                "name": format!("person_{}", pid + 1),
                "email": format!("person{}@example.com", pid + 1),
                "city": CITIES[rng.random_range(0..CITIES.len())],
                "state": p.state,
                "dateTime": ns,
            }),
            ns,
        )?;
    }

    // auctions（时间窗 10%-100%）
    for i in 0..num_auction {
        let a = &auctions[i as usize];
        let ns = BASE_NS + rng.random_range((SPAN_NS / 10)..=SPAN_NS);
        write_event(
            &mut sink,
            "auction_events",
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
            ns,
        )?;
    }

    // bids（92% firehose，时间窗 20%-100%）
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
        write_event(
            &mut sink,
            "bid_events",
            json!({
                "auction": aidx as i64 + 1,
                "bidder": bidder,
                "price": price,
                "channel": CHANNELS[rng.random_range(0..CHANNELS.len())],
                "url": format!("http://www.example.com/{}", rng.random_range(100..=999)),
                "dateTime": ns,
                "extra": "",
            }),
            ns,
        )?;
    }

    match sink {
        BucketSink::Direct(mut out) => {
            out.flush()
                .map_err(|e| error::error(WfgenReason::Io, format!("flush stdout: {e}")))?;
        }
        BucketSink::Sorted { writers, tmp_dir } => {
            // Close all bucket files, then emit bucket-by-bucket: each bucket is
            // read, sorted by `dateTime` (bounded: one ~55MB bucket at a time
            // even at 100M events), and written in order → globally strict
            // event-time order. Temp dir removed afterwards.
            drop(writers);
            let stdout = std::io::stdout();
            let mut out = std::io::BufWriter::new(stdout.lock());
            for i in 0..TIME_BUCKETS {
                let path = tmp_dir.join(format!("b{i:03}.jsonl"));
                let data = std::fs::read(&path)
                    .map_err(|e| error::error(WfgenReason::Io, format!("read bucket file: {e}")))?;
                let mut entries: Vec<(i64, &[u8])> = data
                    .split(|&b| b == b'\n')
                    .filter(|l| !l.is_empty())
                    .filter_map(|l| extract_date_time_ns(l).map(|ns| (ns, l)))
                    .collect();
                entries.sort_by_key(|(ns, _)| *ns);
                for (_, line) in entries {
                    out.write_all(line)
                        .map_err(|e| error::error(WfgenReason::Io, format!("write stdout: {e}")))?;
                    out.write_all(b"\n")
                        .map_err(|e| error::error(WfgenReason::Io, format!("write stdout: {e}")))?;
                }
            }
            out.flush()
                .map_err(|e| error::error(WfgenReason::Io, format!("flush stdout: {e}")))?;
            let _ = std::fs::remove_dir_all(&tmp_dir);
        }
    }
    Ok(())
}
