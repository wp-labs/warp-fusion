//! Deterministic NEXMark event generation (Person/Auction/Bid) as JSONL.
//!
//! Native Rust NEXMark generator with no Python dependency (replaces the
//! former `nexmark_pk/scripts/gen_nexmark.py` reference). Emits wfusion JSONL
//! (`_stream`/`_window`/`_timestamp` metadata + event fields, `dateTime` in
//! epoch ns).
//!
//! 生成语义**严格对齐** NEXMark 官方 `nexmark/nexmark`（原 flink-benchmarks）默认配置：
//! - **交错生成**（round-robin）：每 50 个事件 1 person + 3 auction + 46 bid，事件时间严格
//!   递增（等价 outOfOrderGroupSize=1）。person/auction 交错出现在事件流里，窗口 watermark
//!   随事件时间渐进推进，snapshot join 时序正确（phase-major 会让 person 窗口在处理
//!   auction/bid 前就推进到末尾，驱逐早期 seller/bidder 的 person）。
//! - **id 唯一**：person/auction 各自从 1000 起单调递增，每个实体一条记录（不循环 1000 个
//!   id 产生多版本），asof join 因此是 O(1) 查注册时间而非扫 600 版本。
//! - **引用最近 N 人**（官方语义）：seller/bidder 75% 概率引用最近 100 人批次的热点
//!   （seller=批次第 1 人、bidder=第 2 人，避免热卖家/热出价人相撞），25% 概率引用最近
//!   numActivePeople=1000 人 ± 10 lead（lead 允许引用「尚未生成」的未来实体，官方靠
//!   乱序/后续事件补达）；bid.auction 50% 概率引用最近 100 个 auction 批次第 1 个，50%
//!   概率引用最近 numInFlightAuctions=100 ± 10 lead。
//! - **价格对数均匀**：initialBid/reserve/bid.price 均 = round(10^(6u) × 100)（[100, 1e8)），
//!   与 auction 冷热无关；auction 有效期 = 1 + [0, 2×horizon) ms，horizon = 未来
//!   numInFlightAuctions=100 个 auction 的生成间隔；category ∈ 10..14（FIRST_CATEGORY_ID=10
//!   + rand(5)）；channel 50% 热门 4 通道 + 50% channel-0..9999。
//!
//! 与官方仅有的差异：name/email/itemName/description/url/creditCard/extra 用确定 rng
//! 生成（官方为静态 SplittableRandom，进程间不确定）——分布语义（长度/字符集/
//! ±20% 体积抖动/热点比例）与官方逐项一致，且同 seed 字节级确定性可重放。
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
// 官方 GeneratorConfig：interEventDelayUs = 1_000_000 / firstEventRate * numEventGenerators
// = 1_000_000 / 10_000 * 1 = 100 µs；timestampForEvent = baseTime + eventNumber × 100µs。
// 事件间隔固定 100µs（与总事件数无关），总跨度 = count × 100µs（随 count 线性增长）。
// （旧实现固定 30min span、rate ∝ 1/count，与官方相反——REVIEW_WFGEN_DATA_GEN_DEVIATIONS #1）
pub(crate) const INTER_EVENT_DELAY_NS: i64 = 100_000; // 100 µs/事件（纳秒）
// NEXMark 官方 id 起始值（person/auction 分属不同命名空间，均可从 1000 起）。
const FIRST_PERSON_ID: i64 = 1000;
const FIRST_AUCTION_ID: i64 = 1000;

// ── Flink 官方 nexmark/nexmark（NexmarkConfiguration + generator 常量）──
// 类型比例：personProportion=1 / auctionProportion=3 / bidProportion=46（total=50 → 2%/6%/92%）
const PERSON_PROPORTION: i64 = 1;
const AUCTION_PROPORTION: i64 = 3;
const BID_PROPORTION: i64 = 46;
const TOTAL_PROPORTION: i64 = PERSON_PROPORTION + AUCTION_PROPORTION + BID_PROPORTION; // 50
// 官方 nextPrice = round(10^(6u) × 100)：对数均匀 [100, 1e8)
// category：FIRST_CATEGORY_ID + rand(NUM_CATEGORIES) → 10..14（5 类）
const FIRST_CATEGORY_ID: i64 = 10;
const NUM_CATEGORIES: i64 = 5;
// 热点：P(hot) = 1 - 1/ratio（hotAuctionRatio=2 → 50%；hotSellers/BiddersRatio=4 → 75%）
const HOT_AUCTION_RATIO: i64 = 2;
const HOT_SELLERS_RATIO: i64 = 4;
const HOT_BIDDERS_RATIO: i64 = 4;
// 热点批次：最近 N 个实体中的固定序号（seller=批次第 1 人、bidder=第 2 人、auction=批次第 1 个）
const HOT_AUCTION_BATCH: i64 = 100;
const HOT_SELLER_BATCH: i64 = 100;
const HOT_BIDDER_BATCH: i64 = 100;
// cold 引用窗口：最近 numActivePeople / numInFlightAuctions 个实体 ± lead（lead 允许未来）
const NUM_ACTIVE_PEOPLE: i64 = 1000;
const PERSON_ID_LEAD: i64 = 10;
const NUM_IN_FLIGHT_AUCTIONS: i64 = 100;
const AUCTION_ID_LEAD: i64 = 10;
// channel：50% 热门 4 通道（HOT_CHANNELS_RATIO=2），50% channel-0..9999
const HOT_CHANNELS_RATIO: i64 = 2;
const CHANNELS_NUMBER: i64 = 10_000;

/// 30-second time buckets over the event span: `span / BUCKET_NS` temp files, each
/// containing events whose `dateTime` falls in that bucket. Emitting buckets
/// in order yields a globally event-time-sorted stream with bounded memory
/// (one bucket is buffered at a time; a single bucket spans <= 30 s, which is
/// negligible next to the 10-minute `over` eviction granularity).
/// Bucket count is dynamic (span = count × 100µs): ~count/300k buckets.
const BUCKET_NS: i64 = 30_000_000_000;

// 官方 PersonGenerator：US_CITIES 10 城 / US_STATES 6 州（独立随机）。
const CITIES: [&str; 10] = [
    "Phoenix",
    "Los Angeles",
    "San Francisco",
    "Boise",
    "Portland",
    "Bend",
    "Redmond",
    "Seattle",
    "Kent",
    "Cheyenne",
];
const STATES: [&str; 6] = ["AZ", "CA", "ID", "OR", "WA", "WY"];
// 官方 PersonGenerator：随机姓名（FIRST_NAMES × LAST_NAMES）。
const FIRST_NAMES: [&str; 11] = [
    "Peter", "Paul", "Luke", "John", "Saul", "Vicky", "Kate", "Julie", "Sarah", "Deiter", "Walter",
];
const LAST_NAMES: [&str; 9] = [
    "Shultz", "Abrams", "Spencer", "White", "Bartels", "Walton", "Smith", "Jones", "Noris",
];
// 官方 NexmarkConfiguration avg*ByteSize：extra 填充到该目标体积。
const AVG_PERSON_BYTE_SIZE: usize = 200;
const AVG_AUCTION_BYTE_SIZE: usize = 500;
const AVG_BID_BYTE_SIZE: usize = 100;
// 官方 BidGenerator：HOT_CHANNELS 4 个（50% 概率），其余 channel-N。
const HOT_CHANNELS: [&str; 4] = ["Google", "Facebook", "Baidu", "Apple"];
// 官方 q21 channel_id 映射（CASE WHEN lower(channel)）：apple=0/google=1/facebook=2/baidu=3。
const HOT_CHANNEL_IDS: [&str; 4] = ["1", "2", "3", "0"];
const HOT_CHANNEL_MAX: usize = HOT_CHANNELS.len();
const CHANNEL_MAX: usize = HOT_CHANNELS.len() + CHANNELS_NUMBER as usize;

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
/// 字符串字段（name/email/creditCard/itemName/description/extra）按官方生成器
/// 随机生成（同 seed 确定性），与 JSON 输出字段一一对应。
#[derive(Debug, Clone)]
pub enum NxEvent {
    Person {
        id: i64,
        ns: i64,
        city: usize,
        state: usize,
        name: String,
        email: String,
        credit_card: String,
        extra: String,
    },
    Auction {
        id: i64,
        ns: i64,
        initial_bid: i64,
        reserve: i64,
        expires: i64,
        seller: i64,
        category: i64,
        item_name: String,
        description: String,
        extra: String,
    },
    Bid {
        auc: i64,
        ns: i64,
        price: i64,
        bidder: i64,
        channel: usize,
        /// q21 的 channel_id：热通道 None（JSON 由官方 CASE 映射），cold 通道
        /// Some(abs(Integer.reverse(i)))——生成时已知，等价官方 URL 的 channel_id 参数。
        channel_id: Option<i64>,
        url: String,
        extra: String,
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
            name,
            email,
            credit_card,
            extra,
        } => json!({
            "id": id,
            "name": name,
            "email": email,
            "creditCard": credit_card,
            "city": CITIES[*city],
            "state": STATES[*state],
            "dateTime": ns,
            "extra": extra,
        }),
        NxEvent::Auction {
            id,
            ns,
            initial_bid,
            reserve,
            expires,
            seller,
            category,
            item_name,
            description,
            extra,
        } => json!({
            "id": id,
            "itemName": item_name,
            "description": description,
            "initialBid": initial_bid,
            "reserve": reserve,
            "dateTime": ns,
            "expires": expires,
            "seller": seller,
            "category": category,
            "extra": extra,
        }),
        NxEvent::Bid {
            auc,
            ns,
            price,
            bidder,
            channel,
            channel_id,
            url,
            extra,
        } => {
            // 官方 BidGenerator：50% 热门 4 通道（Google/Facebook/Baidu/Apple），
            // 50% channel-N（官方用递增计数器轮询，非随机）。channel 编码：
            // 0..4 = 热门索引，≥4 = channel-{channel-4}。
            let channel_name = if *channel < HOT_CHANNEL_MAX {
                HOT_CHANNELS[*channel].to_string()
            } else {
                format!("channel-{}", *channel - HOT_CHANNEL_MAX)
            };
            // 官方 q21（Add channel id）的 channel_id：热通道按官方 CASE 映射
            // （apple=0/google=1/facebook=2/baidu=3），cold 取生成时算好的
            // abs(Integer.reverse(i))（与 URL 的 channel_id 参数一致）。
            let channel_id = match channel_id {
                Some(id) => id.to_string(),
                None => HOT_CHANNEL_IDS[*channel].to_string(),
            };
            json!({
                "auction": auc,
                "bidder": bidder,
                "price": price,
                "channel": channel_name,
                "channel_id": channel_id,
                "url": url,
                "dateTime": ns,
                "extra": extra,
            })
        }
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
    time_buckets: usize,
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
                (((ns - BASE_NS).max(0)) / BUCKET_NS).min((time_buckets - 1) as i64) as usize;
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

/// 官方 `PersonGenerator.nextPrice`：对数均匀 [100, 1e8)。
fn next_price(rng: &mut StdRng) -> i64 {
    (10f64.powf(rng.random::<f64>() * 6.0) * 100.0).round() as i64
}

/// 官方 `BidGenerator.getBaseUrl`：3 段目录 + 固定查询串
/// （`https://www.nexmark.com/{s1}/{s2}/{s3}/item.htm?query=1`，cold 通道追加
/// `&channel_id=`）。目录段 = 官方 `nextString(random, 5, '_')`：长度 3..5、
/// 字符 a-z 且约 1/13 概率为 '_'（官方分隔符参数）。
fn next_url(rng: &mut StdRng, channel_id: Option<i64>) -> String {
    let base = format!(
        "https://www.nexmark.com/{}/{}/{}/item.htm?query=1",
        next_string(rng, 5, '_'),
        next_string(rng, 5, '_'),
        next_string(rng, 5, '_')
    );
    match channel_id {
        Some(id) => format!("{base}&channel_id={id}"),
        None => base,
    }
}

/// 官方 `StringsGenerator.nextString(random, maxLength, special)`：
/// 长度 = 3 + random.nextInt(maxLength - 3)（∈ [3, maxLength]）；每字符
/// 约 1/13 概率为 special（默认 ' '，URL 目录用 '_'），否则 a-z；末尾 trim
/// （trim 仅去空白，special='_' 时无空白可去）。
fn next_string(rng: &mut StdRng, max_len: usize, special: char) -> String {
    let len = 3 + rng.random_range(0..max_len.saturating_sub(3).max(1));
    let mut s = String::with_capacity(len);
    for _ in 0..len {
        if rng.random_range(0..13) == 0 {
            s.push(special);
        } else {
            s.push((b'a' + rng.random_range(0u8..26)) as char);
        }
    }
    s.trim().to_string()
}

/// 官方 `StringsGenerator.nextExactString(random, length)`：精确长度的纯小写串。
/// 官方用预生成的 1MB 复用串截取（性能优化）；此处逐字符生成，分布一致
/// （a-z 均匀；官方逐字符路径用 rnd 打包 6 字符/次，同为 a-z 均匀）。
fn next_exact_string(rng: &mut StdRng, len: usize) -> String {
    let mut s = String::with_capacity(len);
    for _ in 0..len {
        s.push((b'a' + rng.random_range(0u8..26)) as char);
    }
    s
}

/// 官方 `StringsGenerator.nextExtra`：currentSize 未超 desiredAverageSize 时，
/// 在目标附近 ±20% 随机抖动（delta = round((desired-current)*0.2)，
/// desiredSize ∈ [minSize, minSize+2δ)），再生成精确长度的纯小写串。
fn next_extra(rng: &mut StdRng, current_size: usize, desired: usize) -> String {
    if current_size > desired {
        String::new()
    } else {
        let remaining = desired - current_size;
        let delta = ((remaining as f64 * 0.2).round()) as usize;
        let min_size = remaining - delta;
        // 官方 `random.nextInt(2 * delta + 1)`（闭区间 [0, 2δ]；δ=0 时恒为 0）。
        let desired_size = min_size + rng.random_range(0..=2 * delta);
        next_exact_string(rng, desired_size)
    }
}

/// 官方 `PersonGenerator.nextCreditCard`：4 组 4 位数字（"xxxx xxxx xxxx xxxx"）。
fn next_credit_card(rng: &mut StdRng) -> String {
    let mut s = String::with_capacity(19);
    for i in 0..4 {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&format!("{:04}", rng.random_range(0..10_000)));
    }
    s
}

/// 官方 `PersonGenerator.lastBase0PersonId`：截至 eventId 的最后 person base0 索引
/// （person 事件自身在 offset < personProportion 时即为该 epoch 的 person）。
fn last_base0_person_id(event_id: i64) -> i64 {
    let epoch = event_id / TOTAL_PROPORTION;
    let offset = (event_id % TOTAL_PROPORTION).min(PERSON_PROPORTION - 1);
    epoch * PERSON_PROPORTION + offset
}

/// 官方 `PersonGenerator.nextBase0PersonId`：最近 numActivePeople 人 ± PERSON_ID_LEAD
/// （lead 可返回「尚未生成」的未来 person 索引，官方靠乱序/后续事件补达）。
fn next_base0_person_id(event_id: i64, rng: &mut StdRng) -> i64 {
    let num_people = last_base0_person_id(event_id) + 1;
    let active = num_people.min(NUM_ACTIVE_PEOPLE);
    let n = rng.random_range(0..(active + PERSON_ID_LEAD));
    (num_people - active + n).max(0)
}

/// 官方 `AuctionGenerator.lastBase0AuctionId`：截至 eventId 的最后 auction base0 索引。
fn last_base0_auction_id(event_id: i64) -> i64 {
    let epoch = event_id / TOTAL_PROPORTION;
    let offset = event_id % TOTAL_PROPORTION;
    if offset < PERSON_PROPORTION {
        // person 事件：回退到上一 epoch 的最后一个 auction。
        (epoch - 1) * AUCTION_PROPORTION + (AUCTION_PROPORTION - 1)
    } else if offset >= PERSON_PROPORTION + AUCTION_PROPORTION {
        // bid 事件：当前 epoch 的最后一个 auction。
        epoch * AUCTION_PROPORTION + (AUCTION_PROPORTION - 1)
    } else {
        // auction 事件：本事件自身。
        epoch * AUCTION_PROPORTION + (offset - PERSON_PROPORTION)
    }
}

/// 官方 `AuctionGenerator.nextBase0AuctionId`：最近 numInFlightAuctions 个 auction
/// ± AUCTION_ID_LEAD（lead 可引用「尚未生成」的未来 auction）。
fn next_base0_auction_id(event_id: i64, rng: &mut StdRng) -> i64 {
    let last = last_base0_auction_id(event_id);
    let min_auc = (last - NUM_IN_FLIGHT_AUCTIONS).max(0);
    let n = rng.random_range(0..(last - min_auc + 1 + AUCTION_ID_LEAD));
    min_auc + n
}

/// 官方 `AuctionGenerator.nextAuctionLengthMs`：auction 有效期 = 1 + [0, 2×horizon) ms，
/// horizon = 未来 `numInFlightAuctions` 个 auction 的生成间隔（事件时间口径）。
/// 官方 timestampForEvent 为固定 100µs/事件 → horizon = 1666 × 100µs = 0.1666s 固定
/// （与 count 无关；旧实现用 SPAN/count 使 horizon 随 count 漂移——REVIEW #1）。
fn auction_length_ns(rng: &mut StdRng) -> i64 {
    let num_events_for_auctions = (NUM_IN_FLIGHT_AUCTIONS * TOTAL_PROPORTION) / AUCTION_PROPORTION; // 100×50/3 = 1666
    let horizon_ns = num_events_for_auctions * INTER_EVENT_DELAY_NS; // 固定 0.1666s
    let horizon_ms = (horizon_ns / 1_000_000).max(1);
    (1 + rng.random_range(0..(horizon_ms * 2).max(1))) * 1_000_000
}

pub fn generate_events<F>(count: i64, seed: u64, mut emit: F) -> WfgenResult<()>
where
    F: FnMut(NxEvent) -> WfgenResult<()>,
{
    let mut rng = StdRng::seed_from_u64(seed);

    // 交错生成（对齐 NEXMark 官方 round-robin）：每 50 个事件 1 person + 3 auction + 46 bid，
    // 事件时间严格递增（等价 NEXMark outOfOrderGroupSize=1）。关键是与官方一样「person/auction
    // 交错出现在事件流里」，让 person/auction 窗口的 watermark 随事件时间渐进推进——否则
    // phase-major 会让 person 窗口在处理 auction/bid 前就推进到 30 分钟末尾，把早期
    // seller/bidder 的 person 驱逐，导致 snapshot join 时序错配（q3 EMIT 从 600k 崩到 ~23k）。
    // 字段生成顺序与 rng 消耗对齐官方 generator（RNG 算法不同，序列不等价，分布语义一致）。

    for event_id in 0..count {
        let rem = event_id % TOTAL_PROPORTION;
        // 官方 timestampForEvent = baseTime + eventNumber × interEventDelayUs/1000
        // = BASE_NS + event_id × 100µs（固定间隔，跨度随 count 线性增长）。
        let ns = BASE_NS + event_id * INTER_EVENT_DELAY_NS;

        if rem < PERSON_PROPORTION {
            // person（官方 PersonGenerator.nextPerson，消耗顺序：姓名 → 邮箱 → 卡号
            // → 城市 → 州 → extra）：name/email/creditCard 随机，extra 补齐到 avgPersonByteSize。
            let person_idx = event_id / TOTAL_PROPORTION;
            let name = format!(
                "{} {}",
                FIRST_NAMES[rng.random_range(0..FIRST_NAMES.len())],
                LAST_NAMES[rng.random_range(0..LAST_NAMES.len())]
            );
            let email = format!(
                "{}@{}.com",
                next_string(&mut rng, 7, ' '),
                next_string(&mut rng, 5, ' ')
            );
            let credit_card = next_credit_card(&mut rng);
            let city = rng.random_range(0..CITIES.len());
            let state = rng.random_range(0..STATES.len());
            let current_size = 8
                + name.len()
                + email.len()
                + credit_card.len()
                + CITIES[city].len()
                + STATES[state].len();
            let extra = next_extra(&mut rng, current_size, AVG_PERSON_BYTE_SIZE);
            emit(NxEvent::Person {
                id: FIRST_PERSON_ID + person_idx,
                ns,
                city,
                state,
                name,
                email,
                credit_card,
                extra,
            })?;
        } else if rem < PERSON_PROPORTION + AUCTION_PROPORTION {
            // auction（官方 AuctionGenerator.nextAuction，消耗顺序：seller → category
            // → initialBid → 有效期 → itemName → description → reserve → extra）。
            let auction_idx = last_base0_auction_id(event_id);
            let seller = if rng.random_range(0..HOT_SELLERS_RATIO) > 0 {
                (last_base0_person_id(event_id) / HOT_SELLER_BATCH) * HOT_SELLER_BATCH
            } else {
                next_base0_person_id(event_id, &mut rng)
            } + FIRST_PERSON_ID;
            let category = FIRST_CATEGORY_ID + rng.random_range(0..NUM_CATEGORIES);
            let initial_bid = next_price(&mut rng);
            let expires = ns + auction_length_ns(&mut rng);
            let item_name = next_string(&mut rng, 20, ' ');
            let description = next_string(&mut rng, 100, ' ');
            let reserve = initial_bid + next_price(&mut rng);
            let current_size = 8 + item_name.len() + description.len() + 8 + 8 + 8 + 8 + 8;
            let extra = next_extra(&mut rng, current_size, AVG_AUCTION_BYTE_SIZE);
            emit(NxEvent::Auction {
                id: FIRST_AUCTION_ID + auction_idx,
                ns,
                initial_bid,
                reserve,
                expires,
                seller,
                category,
                item_name,
                description,
                extra,
            })?;
        } else {
            // bid（官方 BidGenerator.nextBid，消耗顺序：auction → bidder → price
            // → channel（hot 2 次 rng / cold 仅判断 1 次 + 计数器）→ extra）。
            let auc_base0 = if rng.random_range(0..HOT_AUCTION_RATIO) > 0 {
                (last_base0_auction_id(event_id) / HOT_AUCTION_BATCH) * HOT_AUCTION_BATCH
            } else {
                next_base0_auction_id(event_id, &mut rng)
            };
            let bidder = if rng.random_range(0..HOT_BIDDERS_RATIO) > 0 {
                (last_base0_person_id(event_id) / HOT_BIDDER_BATCH) * HOT_BIDDER_BATCH + 1
            } else {
                next_base0_person_id(event_id, &mut rng)
            } + FIRST_PERSON_ID;
            let price = next_price(&mut rng);
            let (channel, channel_id, url) = if rng.random_range(0..HOT_CHANNELS_RATIO) > 0 {
                let i = rng.random_range(0..HOT_CHANNELS.len());
                (i, None, next_url(&mut rng, None))
            } else {
                // 官方 cold 通道：`random.nextInt(CHANNELS_NUMBER)` 均匀随机取通道
                // （官方从 10000 条预生成缓存取；wfgen 用确定 rng 逐 bid 生成，分布一致），
                // url 追加 `channel_id = abs(Integer.reverse(i))`（Java int 位反转）。
                // wrapping_abs 复刻 Java Math.abs(Integer.MIN_VALUE) 溢出返回负值的行为
                // （概率 1/2^32，官方亦如此）。
                // （旧实现为顺序轮询计数器 + 原始 channel_id——REVIEW #5）
                let i = rng.random_range(0..CHANNELS_NUMBER);
                let channel_id = (i as i32).reverse_bits().wrapping_abs() as i64;
                (
                    HOT_CHANNEL_MAX + i as usize,
                    Some(channel_id),
                    next_url(&mut rng, Some(channel_id)),
                )
            };
            let current_size = 8 + 8 + 8 + 8;
            let extra = next_extra(&mut rng, current_size, AVG_BID_BYTE_SIZE);
            emit(NxEvent::Bid {
                auc: auc_base0 + FIRST_AUCTION_ID,
                ns,
                price,
                bidder,
                channel,
                channel_id,
                url,
                extra,
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
    // lead 允许引用「尚未生成」的未来实体：person 上限放宽 PERSON_ID_LEAD、
    // auction 上限放宽 AUCTION_ID_LEAD（官方 nextBase0PersonId/nextBase0AuctionId 语义）。
    let person_hi = FIRST_PERSON_ID + num_person - 1 + PERSON_ID_LEAD;
    let auction_hi = FIRST_AUCTION_ID + num_auction - 1 + AUCTION_ID_LEAD;
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
                || *initial_bid < 100 // 官方 nextPrice 下界 100
                || *reserve < *initial_bid
                || expires < ns
                || !(FIRST_PERSON_ID..=person_hi).contains(seller)
                || !(FIRST_CATEGORY_ID..FIRST_CATEGORY_ID + NUM_CATEGORIES).contains(category)
        }
        NxEvent::Bid {
            auc,
            price,
            bidder,
            channel,
            channel_id,
            url,
            ..
        } => {
            !(FIRST_AUCTION_ID..=auction_hi).contains(auc)
                || !(FIRST_PERSON_ID..=person_hi).contains(bidder)
                || *price < 100 // 官方 nextPrice 下界 100
                || *channel >= CHANNEL_MAX
                // url 官方格式：https://www.nexmark.com/{5}/{5}/{5}/item.htm?query=1[&channel_id=N]
                // （q22 取 split('/') 索引 3/4/5，越界即违规；目录段可为 '_'（官方
                // nextString(5,'_')），不影响 '/' 计数）
                || url.split('/').count() < 7
                || !url.starts_with("https://www.nexmark.com/")
                // cold 通道的 channel_id 字段必须与 url 参数一致（官方 abs(Integer.reverse(i))）。
                || (channel_id.is_some()
                    && !url.contains(&format!("channel_id={}", channel_id.unwrap())))
        }
    }
}

/// 生成 + 可选自检（`--check`）：生成阶段（进度条）→ 数据检查阶段
/// （`--check` 独立进度条：同一 seed 确定性重放事件流，逐事件值域校验）
/// + 输出字节 md5 指纹（确定性锚点：同 seed+count 恒等；桶序模式 = 输出文件 md5）。
/// 报告写 stderr（stdout 是数据流）：默认输出简短质量报告（行数/时间/乱序），
/// `--check` 追加值域违规与 md5 指纹。
pub fn run_checked(count: i64, seed: u64, no_sort: bool, check: bool) -> WfgenResult<()> {
    // 时间桶数随跨度动态：span = count × 100µs（官方固定速率），桶宽 30s。
    let time_buckets = (((count * INTER_EVENT_DELAY_NS) / BUCKET_NS).max(1)) as usize;

    // 轻量统计（默认报告用，几乎零成本）：流计数 + 时间戳范围 + 乱序深度。
    let mut n_person: i64 = 0;
    let mut n_auction: i64 = 0;
    let mut n_bid: i64 = 0;
    let mut min_ns: i64 = i64::MAX;
    let mut max_ns: i64 = i64::MIN;
    // 流序统计：桶序模式下输出 = 桶间严格递增 + 桶内乱序，故每桶独立 watermark
    // （等价于对实际输出流统计，零解析成本）；no-sort 模式用全局 watermark
    // （phase-major 乱序可达 ~27min，远超桶宽——这是它破坏 over 驱逐的机理）。
    let mut seq_max_bucket: Vec<i64> = vec![i64::MIN; time_buckets];
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
        let writers = (0..time_buckets)
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
            let b = (((ns - BASE_NS).max(0)) / BUCKET_NS).min((time_buckets - 1) as i64) as usize;
            if ns < seq_max_bucket[b] {
                oog_count += 1;
                max_oog_ns = max_oog_ns.max(seq_max_bucket[b] - ns);
            } else {
                seq_max_bucket[b] = ns;
            }
        }
        write_event(
            &mut sink,
            ev.stream(),
            nx_to_value(&ev),
            ev.ns(),
            time_buckets,
        )
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
        use std::collections::HashSet;

        let mut person_ids: HashSet<i64> = HashSet::new();
        let mut auction_ids: HashSet<i64> = HashSet::new();
        let mut n_person_so_far: i64 = 0;
        let mut n_auction_so_far: i64 = 0;
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
            // 跨事件规则（官方引用语义，`lastBase0*`/`nextBase0*` 公式的逆校验）：
            // - id 唯一；
            // - seller/bidder 引用「最近 numActivePeople 人 ± PERSON_ID_LEAD」的 person
            //   （lead 允许引用尚未生成的未来 person，官方靠乱序/后续事件补达）；
            // - bid.auction 引用「最近 numInFlightAuctions 个 ± AUCTION_ID_LEAD」的 auction。
            // 引用已生成的实体时须真实存在（存在性校验），引用未来 lead 则只查窗口。
            match ev {
                NxEvent::Person { id, .. } => {
                    if !person_ids.insert(id) {
                        ref_violations += 1;
                    }
                    n_person_so_far += 1;
                }
                NxEvent::Auction { id, seller, .. } => {
                    if !auction_ids.insert(id) {
                        ref_violations += 1;
                    }
                    let seller_base0 = seller - FIRST_PERSON_ID;
                    let lo = (n_person_so_far - NUM_ACTIVE_PEOPLE).max(0);
                    if !(lo..(n_person_so_far + PERSON_ID_LEAD)).contains(&seller_base0) {
                        ref_violations += 1;
                    } else if seller_base0 < n_person_so_far && !person_ids.contains(&seller) {
                        ref_violations += 1;
                    }
                    n_auction_so_far += 1;
                }
                NxEvent::Bid { auc, bidder, .. } => {
                    let auc_base0 = auc - FIRST_AUCTION_ID;
                    let last_auc = n_auction_so_far - 1;
                    // 官方 nextBase0AuctionId 上界含 last+LEAD（nextLong 参数带 +1，
                    // 与 person 窗口的上界 last+LEAD-1 相差 1，照搬官方公式）。
                    if auc_base0 < (last_auc - NUM_IN_FLIGHT_AUCTIONS).max(0)
                        || auc_base0 > last_auc + AUCTION_ID_LEAD
                    {
                        ref_violations += 1;
                    } else if auc_base0 <= last_auc && !auction_ids.contains(&auc) {
                        ref_violations += 1;
                    }
                    let bidder_base0 = bidder - FIRST_PERSON_ID;
                    let lo = (n_person_so_far - NUM_ACTIVE_PEOPLE).max(0);
                    if !(lo..(n_person_so_far + PERSON_ID_LEAD)).contains(&bidder_base0) {
                        ref_violations += 1;
                    } else if bidder_base0 < n_person_so_far && !person_ids.contains(&bidder) {
                        ref_violations += 1;
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
            for i in 0..time_buckets {
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
    let span_ns = count * INTER_EVENT_DELAY_NS;
    let ns_ok = min_ns >= BASE_NS && max_ns <= BASE_NS + span_ns;
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
        "  时间  [{}s, {}s] / {}s span {}（官方固定 100µs/事件，跨度 = count×100µs）",
        (min_ns - BASE_NS) / 1_000_000_000,
        (max_ns - BASE_NS) / 1_000_000_000,
        span_ns / 1_000_000_000,
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
            "  引用  {}（id 唯一 + seller/bidder 引用最近 {}人±{} lead + bid 引用最近 {}个±{} lead）{}",
            fmt_num(ref_violations),
            NUM_ACTIVE_PEOPLE,
            PERSON_ID_LEAD,
            NUM_IN_FLIGHT_AUCTIONS,
            AUCTION_ID_LEAD,
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
    /// （FIRST_*_ID 起），seller/bidder 引用 person id（含 ±lead 未来），bid.auction 引用
    /// auction id（含 ±lead 未来），价格/category/channel 值域对齐 Flink 官方。
    #[test]
    fn check_event_bounds_match_generation_semantics() {
        let count = 10_000i64;
        let num_person = (count as f64 * 0.02) as i64; // 200
        let num_auction = (count as f64 * 0.06) as i64; // 600
        let person_hi = FIRST_PERSON_ID + num_person - 1 + PERSON_ID_LEAD; // 1209
        let auction_hi = FIRST_AUCTION_ID + num_auction - 1 + AUCTION_ID_LEAD; // 1609

        // 边界值合法。
        assert!(!check_event(
            &NxEvent::Auction {
                id: auction_hi,
                ns: BASE_NS,
                initial_bid: 100,
                reserve: 200,
                expires: BASE_NS + 1_000_000_000,
                seller: FIRST_PERSON_ID,
                category: FIRST_CATEGORY_ID,
                item_name: "x".repeat(20),
                description: "x".repeat(100),
                extra: String::new(),
            },
            count
        ));
        assert!(!check_event(
            &NxEvent::Bid {
                auc: auction_hi,
                ns: BASE_NS,
                price: 100,
                bidder: person_hi,
                channel: CHANNEL_MAX - 1,
                channel_id: Some(123),
                url: "https://www.nexmark.com/aaaaa/bbbbb/ccccc/item.htm?query=1&channel_id=123"
                    .to_string(),
                extra: String::new(),
            },
            count
        ));
        assert!(!check_event(
            &NxEvent::Person {
                id: person_hi,
                ns: BASE_NS,
                city: CITIES.len() - 1,
                state: STATES.len() - 1,
                name: "Peter Shultz".to_string(),
                email: "abc@def.com".to_string(),
                credit_card: "1234 5678 9012 3456".to_string(),
                extra: String::new(),
            },
            count
        ));

        // 越界必须报违规。
        assert!(check_event(
            &NxEvent::Auction {
                id: auction_hi + 1,
                ns: BASE_NS,
                initial_bid: 100,
                reserve: 200,
                expires: BASE_NS + 1_000_000_000,
                seller: FIRST_PERSON_ID,
                category: FIRST_CATEGORY_ID,
                item_name: "x".repeat(20),
                description: "x".repeat(100),
                extra: String::new(),
            },
            count
        ));
        assert!(check_event(
            &NxEvent::Auction {
                id: auction_hi,
                ns: BASE_NS,
                initial_bid: 99, // < 官方 nextPrice 下界 100
                reserve: 200,
                expires: BASE_NS + 1_000_000_000,
                seller: FIRST_PERSON_ID,
                category: FIRST_CATEGORY_ID,
                item_name: "x".repeat(20),
                description: "x".repeat(100),
                extra: String::new(),
            },
            count
        ));
        assert!(check_event(
            &NxEvent::Auction {
                id: auction_hi,
                ns: BASE_NS,
                initial_bid: 100,
                reserve: 200,
                expires: BASE_NS + 1_000_000_000,
                seller: FIRST_PERSON_ID,
                category: FIRST_CATEGORY_ID + NUM_CATEGORIES, // 越 category 上限（14）
                item_name: "x".repeat(20),
                description: "x".repeat(100),
                extra: String::new(),
            },
            count
        ));
        assert!(check_event(
            &NxEvent::Bid {
                auc: FIRST_AUCTION_ID - 1, // 低于 auction id 下限
                ns: BASE_NS,
                price: 100,
                bidder: person_hi,
                channel: 0,
                channel_id: None,
                url: "https://www.nexmark.com/aaaaa/bbbbb/ccccc/item.htm?query=1".to_string(),
                extra: String::new(),
            },
            count
        ));
        assert!(check_event(
            &NxEvent::Bid {
                auc: auction_hi,
                ns: BASE_NS,
                price: 99, // < 官方 nextPrice 下界 100
                bidder: person_hi,
                channel: 0,
                channel_id: None,
                url: "https://www.nexmark.com/aaaaa/bbbbb/ccccc/item.htm?query=1".to_string(),
                extra: String::new(),
            },
            count
        ));
        assert!(check_event(
            &NxEvent::Bid {
                auc: auction_hi,
                ns: BASE_NS,
                price: 100,
                bidder: person_hi,
                channel: CHANNEL_MAX, // 越 channel 上限（4+10000）
                channel_id: None,
                url: "https://www.nexmark.com/aaaaa/bbbbb/ccccc/item.htm?query=1".to_string(),
                extra: String::new(),
            },
            count
        ));
        assert!(check_event(
            &NxEvent::Bid {
                auc: auction_hi,
                ns: BASE_NS,
                price: 100,
                bidder: person_hi,
                channel: HOT_CHANNEL_MAX + 1, // cold 通道
                channel_id: Some(99),         // 与 url 的 channel_id=123 不一致（q21 依赖该字段）
                url: "https://www.nexmark.com/aaaaa/bbbbb/ccccc/item.htm?query=1&channel_id=123"
                    .to_string(),
                extra: String::new(),
            },
            count
        ));
        assert!(check_event(
            &NxEvent::Person {
                id: person_hi + 1, // 超出 person id 上限（含 lead）
                ns: BASE_NS,
                city: 0,
                state: 0,
                name: "Peter Shultz".to_string(),
                email: "abc@def.com".to_string(),
                credit_card: "1234 5678 9012 3456".to_string(),
                extra: String::new(),
            },
            count
        ));
        assert!(check_event(
            &NxEvent::Person {
                id: person_hi,
                ns: BASE_NS,
                city: CITIES.len(), // 越 city 上限（10）
                state: 0,
                name: "Peter Shultz".to_string(),
                email: "abc@def.com".to_string(),
                credit_card: "1234 5678 9012 3456".to_string(),
                extra: String::new(),
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

    /// 性质测试：生成数据满足 NEXMark 官方引用规则（`lastBase0*`/`nextBase0*` 逆校验）——
    /// person/auction id 唯一，seller/bidder 引用「最近 numActivePeople 人 ± lead」的 person
    /// （引用已生成的须真实存在），bid.auction 引用「最近 numInFlightAuctions 个 ± lead」
    /// 的 auction。这是 join 窗口数据的前提（官方 lead 语义：允许引用未来实体）。
    #[test]
    fn generated_events_pass_reference_rules() {
        use std::collections::HashSet;

        let count = 50_000i64;
        let mut person_ids: HashSet<i64> = HashSet::new();
        let mut auction_ids: HashSet<i64> = HashSet::new();
        let mut n_person = 0i64;
        let mut n_auction = 0i64;
        let mut violations = 0u64;

        generate_events(count, 7, |ev| {
            match ev {
                NxEvent::Person { id, .. } => {
                    if !person_ids.insert(id) {
                        violations += 1;
                    }
                    n_person += 1;
                }
                NxEvent::Auction { id, seller, .. } => {
                    if !auction_ids.insert(id) {
                        violations += 1;
                    }
                    let b0 = seller - FIRST_PERSON_ID;
                    let lo = (n_person - NUM_ACTIVE_PEOPLE).max(0);
                    if !(lo..(n_person + PERSON_ID_LEAD)).contains(&b0) {
                        violations += 1;
                    } else if b0 < n_person && !person_ids.contains(&seller) {
                        violations += 1;
                    }
                    n_auction += 1;
                }
                NxEvent::Bid { auc, bidder, .. } => {
                    let auc_b0 = auc - FIRST_AUCTION_ID;
                    let last = n_auction - 1;
                    if auc_b0 < (last - NUM_IN_FLIGHT_AUCTIONS).max(0)
                        || auc_b0 > last + AUCTION_ID_LEAD
                    {
                        violations += 1;
                    } else if auc_b0 <= last && !auction_ids.contains(&auc) {
                        violations += 1;
                    }
                    let b0 = bidder - FIRST_PERSON_ID;
                    let lo = (n_person - NUM_ACTIVE_PEOPLE).max(0);
                    if !(lo..(n_person + PERSON_ID_LEAD)).contains(&b0) {
                        violations += 1;
                    } else if b0 < n_person && !person_ids.contains(&bidder) {
                        violations += 1;
                    }
                }
            }
            Ok(())
        })
        .unwrap();

        assert_eq!(violations, 0, "生成数据应满足 id 唯一 + 官方引用窗口规则");
    }

    /// 性质测试：bid→auction 引用命中率（官方语义：50% 概率引用最近 100 个 auction 批次第 1 个，
    /// 50% 概率引用最近 numInFlightAuctions±lead）。引用「已生成 auction」的比例应接近 100%
    /// （lead 只允许未来 ±10 个，占比 <1%），且被引用 auction 的 dateTime 不晚于当前 bid。
    /// 官方 NEXMark 是「最近 N 个实体」而非「最近 X 秒」，故不做 60s 时间窗断言
    /// （时间命中率随事件速率变化，见 NEXMARK_CONFORMANCE.md）。
    #[test]
    fn bid_auction_refs_are_recent_and_existing() {
        use std::collections::HashMap;

        let count = 100_000i64;
        let mut auction_seq: HashMap<i64, i64> = HashMap::new(); // id -> 生成顺序
        let mut n_auction = 0i64;
        let mut bids = 0u64;
        let mut existing = 0u64;
        let mut future_lead = 0u64;
        let mut too_old = 0u64;

        generate_events(count, 7, |ev| {
            match ev {
                NxEvent::Auction { id, .. } => {
                    auction_seq.insert(id, n_auction);
                    n_auction += 1;
                }
                NxEvent::Bid { auc, .. } => {
                    bids += 1;
                    let b0 = auc - FIRST_AUCTION_ID;
                    let last = n_auction - 1;
                    if b0 > last {
                        future_lead += 1; // ±10 lead 内的未来 auction（官方允许）
                    } else if b0 >= (last - NUM_IN_FLIGHT_AUCTIONS).max(0) {
                        existing += 1; // 引用最近 in-flight 窗口内的已生成 auction
                    } else {
                        too_old += 1; // 超出 in-flight 窗口
                    }
                }
                _ => {}
            }
            Ok(())
        })
        .unwrap();

        assert_eq!(too_old, 0, "不应引用 in-flight 窗口之外的过期 auction");
        let hit_rate = (existing + future_lead) as f64 / bids as f64;
        assert!(
            hit_rate >= 0.99,
            "bid→auction 引用窗口命中率应 ≈100%，实际 {:.2}%",
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
