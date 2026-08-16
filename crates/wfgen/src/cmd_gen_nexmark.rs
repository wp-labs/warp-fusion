//! Deterministic NEXMark event generation (Person/Auction/Bid) as JSONL.
//!
//! Native Rust port of `nexmark_pk/scripts/gen_nexmark.py` so benchmarks do not
//! depend on Python. Emits wfusion JSONL (`_stream`/`_window`/`_timestamp`
//! metadata + event fields, `dateTime` in epoch ns).
//!
//! Output is **streamed per phase** (persons → auctions → bids), not buffered:
//! 100M events are written as they are generated, so wfgen memory stays bounded
//! (~reference data only) instead of holding the whole event set in a Vec.
//! Event ordering is phase-major rather than globally time-sorted; the event
//! span is ~30min and nexmark windows allow 30m lateness, so no late drops.
//!
//! Usage: `wfgen gen-nexmark <count> [--seed N]`

use rand::Rng;
use rand::SeedableRng;
use rand::rngs::StdRng;
use serde_json::json;

use std::io::Write;

use crate::error::{self, WfgenReason, WfgenResult};

const BASE_NS: i64 = 1767225600000000000; // 2026-01-01T00:00:00Z
const SPAN_NS: i64 = 1800_000_000_000; // 30 min event span
const PERSONS: usize = 1000;
const HOT_SELLERS: usize = 250; // 25% 卖家为 hot
const HOT_BIDDERS: usize = 250; // 25% 出价者为 hot

const CITIES: [&str; 8] = [
    "Mountain View", "San Francisco", "Sunnyvale", "New York",
    "Los Angeles", "Chicago", "Boston", "Austin",
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

/// Serialize one event (fields + `_stream`/`_window`/`_timestamp` metadata)
/// straight to `out` as a single JSONL line. No buffering of the event set.
fn write_event(
    out: &mut impl Write,
    stream: &str,
    fields: serde_json::Value,
    ns: i64,
) -> WfgenResult<()> {
    let mut m = fields.as_object().cloned().unwrap_or_default();
    m.insert("_stream".to_string(), json!(stream));
    m.insert("_window".to_string(), json!(stream));
    m.insert("_timestamp".to_string(), json!(iso(ns)));
    serde_json::to_writer(&mut *out, &serde_json::Value::Object(m))
        .map_err(|e| error::error(WfgenReason::Serialization, format!("serialize event: {e}")))?;
    out.write_all(b"\n")
        .map_err(|e| error::error(WfgenReason::Io, format!("write stdout: {e}")))?;
    Ok(())
}

pub fn run(count: i64, seed: u64) -> WfgenResult<()> {
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

    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());

    // persons（注册集中在前 10% 时间窗）
    for i in 0..num_person {
        let pid = (i % PERSONS as i64) as usize;
        let p = &persons[pid];
        let ns = BASE_NS + rng.random_range(0..=(SPAN_NS / 10));
        write_event(
            &mut out,
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
            &mut out,
            "auction_events",
            json!({
                "id": i + 1,
                "itemName": format!("item_{}", i),
                "description": format!("desc {}", i),
                "initialBid": rng.random_range(10..=1000),
                "reserve": rng.random_range(1000..=10000),
                "dateTime": ns,
                "expires": ns + rng.random_range(600_000_000_000..=1800_000_000_000),
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
            &mut out,
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

    out.flush()
        .map_err(|e| error::error(WfgenReason::Io, format!("flush stdout: {e}")))?;
    Ok(())
}
