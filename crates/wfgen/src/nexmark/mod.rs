//! Deterministic NEXMark event generation (Person/Auction/Bid) — shared domain
//! between `gen-nexmark`（生成命令）与 `verify-nexmark`（oracle 对拍）。

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

use crate::error::WfgenResult;

pub(crate) const BASE_NS: i64 = 1767225600000000000; // 2026-01-01T00:00:00Z
// 官方 GeneratorConfig：interEventDelayUs = 1_000_000 / firstEventRate * numEventGenerators
// = 1_000_000 / 10_000 * 1 = 100 µs；timestampForEvent = baseTime + eventNumber × 100µs。
// 事件间隔固定 100µs（与总事件数无关），总跨度 = count × 100µs（随 count 线性增长）。
// （旧实现固定 30min span、rate ∝ 1/count，与官方相反——REVIEW_WFGEN_DATA_GEN_DEVIATIONS #1）
pub(crate) const INTER_EVENT_DELAY_NS: i64 = 100_000; // 100 µs/事件（纳秒）
// NEXMark 官方 id 起始值（person/auction 分属不同命名空间，均可从 1000 起）。
pub(crate) const FIRST_PERSON_ID: i64 = 1000;
pub(crate) const FIRST_AUCTION_ID: i64 = 1000;

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
pub(crate) const NUM_ACTIVE_PEOPLE: i64 = 1000;
pub(crate) const PERSON_ID_LEAD: i64 = 10;
pub(crate) const NUM_IN_FLIGHT_AUCTIONS: i64 = 100;
pub(crate) const AUCTION_ID_LEAD: i64 = 10;
// channel：50% 热门 4 通道（HOT_CHANNELS_RATIO=2），50% channel-0..9999
const HOT_CHANNELS_RATIO: i64 = 2;
const CHANNELS_NUMBER: i64 = 10_000;

/// 30-second time buckets over the event span: `span / BUCKET_NS` temp files, each
/// containing events whose `dateTime` falls in that bucket. Emitting buckets
/// in order yields a globally event-time-sorted stream with bounded memory
/// (one bucket is buffered at a time; a single bucket spans <= 30 s, which is
/// negligible next to the 10-minute `over` eviction granularity).
/// Bucket count is dynamic (span = count × 100µs): ~count/300k buckets.
pub(crate) const BUCKET_NS: i64 = 30_000_000_000;

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
        /// q21 的 channel_id：热通道 None（JSON 由官方 CASE 映射）；cold 通道
        /// Some(abs(Integer.reverse(i))) = url 含 channel_id 参数，None = 官方
        /// 10% 概率无 channel_id 参数（JSON 输出空串，q21 WHERE 过滤该行）。
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
            // （apple=0/google=1/facebook=2/baidu=3）；cold 取生成时算好的
            // abs(Integer.reverse(i))（与 URL 的 channel_id 参数一致）；cold 且
            // 官方 10% 概率无 channel_id 参数 → 输出空串（q21 的 WHERE 过滤）。
            let channel_id = match channel_id {
                Some(id) => id.to_string(),
                None if *channel < HOT_CHANNEL_MAX => HOT_CHANNEL_IDS[*channel].to_string(),
                None => String::new(),
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
/// desiredSize = minSize + (delta==0 ? 0 : nextInt(2×delta))——**半开** [0, 2δ)，
/// δ=0 时恒 0（官方源码逐行对照），再生成精确长度的纯小写串。
fn next_extra(rng: &mut StdRng, current_size: usize, desired: usize) -> String {
    if current_size > desired {
        String::new()
    } else {
        let remaining = desired - current_size;
        let delta = ((remaining as f64 * 0.2).round()) as usize;
        let min_size = remaining - delta;
        // 官方 `random.nextInt(2 * delta)` 半开区间；delta==0 时官方三元给 0
        // （Rust `0..0` 是空区间会 panic，必须保留特判）。
        let desired_size = min_size
            + if delta == 0 {
                0
            } else {
                rng.random_range(0..2 * delta)
            };
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

/// 官方 horizonMs（毫秒）：timestampForEvent(n+1666) − timestampForEvent(n)，其中
/// timestampForEvent(n) = baseTime + floor(n×100µs / 1ms)（官方 `(long)(n×100.0)/1000`
/// 向下取整）→ 166/167ms 随 n mod 10 抖动（1666×0.1=166.6，floor 差依赖 n 的毫秒相位）。
fn auction_horizon_ms(event_id: i64) -> i64 {
    let num_events_for_auctions = (NUM_IN_FLIGHT_AUCTIONS * TOTAL_PROPORTION) / AUCTION_PROPORTION; // 100×50/3 = 1666
    let t_now_ms = (event_id * INTER_EVENT_DELAY_NS) / 1_000_000;
    let t_future_ms = ((event_id + num_events_for_auctions) * INTER_EVENT_DELAY_NS) / 1_000_000;
    (t_future_ms - t_now_ms).max(1)
}

/// 官方 `AuctionGenerator.nextAuctionLengthMs`：auction 有效期 = 1 + [0, 2×horizon) ms
/// （官方 `1 + nextLong(max(horizonMs×2, 1))`，返回值毫秒；wfgen 换算纳秒）。
fn auction_length_ns(event_id: i64, rng: &mut StdRng) -> i64 {
    let horizon_ms = auction_horizon_ms(event_id);
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
            let expires = ns + auction_length_ns(event_id, &mut rng);
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
                // 缓存创建时 `random.nextInt(10) > 0` → 90% 概率 url 追加
                // `channel_id = abs(Integer.reverse(i))`（Java int 位反转），10% 无参数
                // （q21 的 WHERE 会过滤无 channel_id 的 cold bid → 输出量 = 热 50% + cold
                // 90%×50% = 95% 的 bid，与官方一致）。wrapping_abs 复刻 Java
                // Math.abs(Integer.MIN_VALUE) 溢出返回负值的行为（概率 1/2^32）。
                // （旧实现为顺序轮询 + 100% 追加——REVIEW #5 及 2026-08-22 复核修正）
                let i = rng.random_range(0..CHANNELS_NUMBER);
                let channel_id = if rng.random_range(0..10) > 0 {
                    Some((i as i32).reverse_bits().wrapping_abs() as i64)
                } else {
                    None
                };
                (
                    HOT_CHANNEL_MAX + i as usize,
                    channel_id,
                    next_url(&mut rng, channel_id),
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
/// 单事件值域自检：返回该事件是否违规（字段范围须与 `generate_events` 的
/// 生成语义一致——person/auction id 唯一（FIRST_*_ID 起），seller/bidder 引用
/// person id，bid.auction 引用 auction id）。
pub(crate) fn check_event(ev: &NxEvent, count: i64) -> bool {
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
                // cold 通道的 channel_id 字段必须与 url 参数一致（官方 abs(Integer.reverse(i))）：
                // Some → url 必须含该参数；cold 且 None（官方 10% 无 channel_id）→ url 不得含。
                || (channel_id.is_some()
                    && !url.contains(&format!("channel_id={}", channel_id.unwrap())))
                || (channel_id.is_none()
                    && *channel >= HOT_CHANNEL_MAX
                    && url.contains("channel_id="))
        }
    }
}

#[cfg(test)]
mod tests;
