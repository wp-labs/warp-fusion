use rand::SeedableRng;
use rand::rngs::StdRng;

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
                if !(lo..(n_person + PERSON_ID_LEAD)).contains(&b0)
                    || (b0 < n_person && !person_ids.contains(&seller))
                {
                    violations += 1;
                }
                n_auction += 1;
            }
            NxEvent::Bid { auc, bidder, .. } => {
                let auc_b0 = auc - FIRST_AUCTION_ID;
                let last = n_auction - 1;
                if auc_b0 < (last - NUM_IN_FLIGHT_AUCTIONS).max(0)
                    || auc_b0 > last + AUCTION_ID_LEAD
                    || (auc_b0 <= last && !auction_ids.contains(&auc))
                {
                    violations += 1;
                }
                let b0 = bidder - FIRST_PERSON_ID;
                let lo = (n_person - NUM_ACTIVE_PEOPLE).max(0);
                if !(lo..(n_person + PERSON_ID_LEAD)).contains(&b0)
                    || (b0 < n_person && !person_ids.contains(&bidder))
                {
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

/// 官方 `nextExtra` 是半开区间：desiredSize = minSize + (delta==0 ? 0 : nextInt(2×delta))
/// ∈ [minSize, minSize+2δ)。2026-08-22 曾误写成闭区间（多一个最大值），回归锁死。
#[test]
fn next_extra_matches_official_half_open_range() {
    // delta == 0（remaining 很小）：官方三元给 0 → 精确补齐，无抖动。
    assert_eq!(next_extra(&mut StdRng::seed_from_u64(1), 30, 31).len(), 1);
    assert_eq!(next_extra(&mut StdRng::seed_from_u64(1), 10, 10).len(), 0); // current==desired → ""
    assert_eq!(next_extra(&mut StdRng::seed_from_u64(1), 11, 10).len(), 0); // current>desired → ""

    // delta > 0：remaining=68 → delta=round(13.6)=14 → minSize=54，
    // desiredSize ∈ [54, 54+27]（半开 [0,28)）。
    let mut rng = StdRng::seed_from_u64(42);
    let mut seen: Vec<usize> = Vec::new();
    for _ in 0..50_000 {
        let s = next_extra(&mut rng, 32, 100); // bid：current=32, desired=100
        seen.push(s.len());
        assert!(
            s.chars().all(|c| c.is_ascii_lowercase()),
            "nextExactString 应纯小写"
        );
    }
    let min = *seen.iter().min().unwrap();
    let max = *seen.iter().max().unwrap();
    assert_eq!(min, 54, "desiredSize 下界 = minSize = remaining−delta");
    assert_eq!(max, 81, "desiredSize 上界 = minSize+2δ−1（半开，不含 82）");
}

/// 官方 `nextString`：长度 = 3 + nextInt(maxLength−3) ∈ [3, maxLength−1]（半开上界），
/// 但 special=' '（默认）时 trim 会去掉首尾空格 → 输出长度可 <3（官方同行为，
/// 只能断言上界 ≤ maxLength−1 与字符集）。
#[test]
fn next_string_length_in_official_range() {
    for max_len in [5usize, 7, 20, 100] {
        let mut rng = StdRng::seed_from_u64(7);
        for _ in 0..20_000 {
            let s = next_string(&mut rng, max_len, ' ');
            assert!(
                s.len() < max_len,
                "len={} 应 < {max_len}（trim 后可更短）",
                s.len()
            );
            assert!(s.chars().all(|c| c == ' ' || c.is_ascii_lowercase()));
        }
    }
    // special='_' 时 trim 无效（trim 只去空白），'_' 保留在串内；
    // 无空白可去 → 长度恒 ∈ [3, max_len−1]。
    let mut rng = StdRng::seed_from_u64(9);
    for _ in 0..20_000 {
        let s = next_string(&mut rng, 5, '_');
        assert!(
            (3..5).contains(&s.len()),
            "special='_' 无 trim：len={} 应在 [3, 4]",
            s.len()
        );
        assert!(s.chars().all(|c| c == '_' || c.is_ascii_lowercase()));
    }
}

/// 官方 `createChannelUrlCache`：每条 cold 通道缓存 90% 概率追加 channel_id
/// （`random.nextInt(10) > 0`），10% 无参数。分布语义：cold bid 的 channel_id
/// 缺失率 ≈10%（q21 的 WHERE 会过滤这些行 → 输出量 = 热 50% + cold 90%×50% = 95%）。
#[test]
fn cold_channel_id_present_about_90pct() {
    let count = 200_000i64;
    let mut cold = 0u64;
    let mut missing = 0u64;
    generate_events(count, 7, |ev| {
        if let NxEvent::Bid {
            channel,
            channel_id,
            ..
        } = ev
            && channel >= HOT_CHANNEL_MAX
        {
            cold += 1;
            if channel_id.is_none() {
                missing += 1;
            }
        }
        Ok(())
    })
    .unwrap();
    let rate = missing as f64 / cold as f64;
    assert!(
        (0.05..0.15).contains(&rate),
        "cold channel_id 缺失率应 ≈10%，实际 {:.1}% ({missing}/{cold})",
        rate * 100.0
    );
}

/// 官方 `nextAuctionLengthMs` 的 horizonMs = floor((n+1666)×100µs/1ms) − floor(n×100µs/1ms)
/// → 166/167ms 随 n mod 10 抖动（1666×0.1=166.6 的毫秒相位）。wfgen 用整数除法复刻。
#[test]
fn auction_horizon_ms_matches_official_ms_rounding() {
    assert_eq!(auction_horizon_ms(0), 166); // 0.0→0, 166.6→166
    assert_eq!(auction_horizon_ms(9), 167); // 0.9→0, 167.5→167
    assert_eq!(auction_horizon_ms(10), 166); // 1.0→1, 167.6→167
    assert_eq!(auction_horizon_ms(5), 167); // 0.5→0, 167.1→167
    // 有效期落在 [1, 2×horizon] ms 区间（官方 1 + nextLong(2×horizon)，含上界；返回纳秒）。
    let mut rng = StdRng::seed_from_u64(3);
    for _ in 0..10_000 {
        let len = auction_length_ns(9, &mut rng);
        assert!(
            (1_000_000..=334_000_000).contains(&len),
            "len={len} 应在 [1, 334]ms"
        );
    }
}

/// check_event 的 channel_id↔url 一致性：cold 且 channel_id=None（官方 10% 无参数）
/// 合法且 url 不得含 channel_id；Some 时 url 必须含该参数。
#[test]
fn check_event_cold_channel_id_consistency() {
    let count = 10_000i64;
    let ok_url = "https://www.nexmark.com/aaaaa/bbbbb/ccccc/item.htm?query=1".to_string();
    // cold + None + url 无参数 → 合法（官方 10% 无 channel_id）。
    assert!(!check_event(
        &NxEvent::Bid {
            auc: FIRST_AUCTION_ID + 1,
            ns: BASE_NS,
            price: 100,
            bidder: FIRST_PERSON_ID + 1,
            channel: HOT_CHANNEL_MAX + 5,
            channel_id: None,
            url: ok_url.clone(),
            extra: String::new(),
        },
        count
    ));
    // cold + None + url 含 channel_id → 违规（字段与 url 矛盾）。
    assert!(check_event(
        &NxEvent::Bid {
            auc: FIRST_AUCTION_ID + 1,
            ns: BASE_NS,
            price: 100,
            bidder: FIRST_PERSON_ID + 1,
            channel: HOT_CHANNEL_MAX + 5,
            channel_id: None,
            url: "https://www.nexmark.com/aaaaa/bbbbb/ccccc/item.htm?query=1&channel_id=123"
                .to_string(),
            extra: String::new(),
        },
        count
    ));
    // cold + Some(123) + url 缺该参数 → 违规（沿用既有用例逻辑）。
    assert!(check_event(
        &NxEvent::Bid {
            auc: FIRST_AUCTION_ID + 1,
            ns: BASE_NS,
            price: 100,
            bidder: FIRST_PERSON_ID + 1,
            channel: HOT_CHANNEL_MAX + 5,
            channel_id: Some(123),
            url: ok_url,
            extra: String::new(),
        },
        count
    ));
}

/// nx_to_value 的 channel_id 字段输出：热通道 → 官方 CASE 映射；cold Some → 位反转值；
/// cold None（10% 无参数）→ 空串（q21 的 WHERE 过滤依据）。
#[test]
fn nx_to_value_channel_id_output() {
    // 热通道
    let v = nx_to_value(&NxEvent::Bid {
        auc: 1,
        ns: BASE_NS,
        price: 100,
        bidder: 1,
        channel: 0, // Google
        channel_id: None,
        url: "u".to_string(),
        extra: String::new(),
    });
    assert_eq!(v["channel_id"], "1"); // HOT_CHANNEL_IDS[0]
    // cold Some
    let v = nx_to_value(&NxEvent::Bid {
        auc: 1,
        ns: BASE_NS,
        price: 100,
        bidder: 1,
        channel: HOT_CHANNEL_MAX + 42,
        channel_id: Some(99),
        url: "u".to_string(),
        extra: String::new(),
    });
    assert_eq!(v["channel_id"], "99");
    // cold None → 空串
    let v = nx_to_value(&NxEvent::Bid {
        auc: 1,
        ns: BASE_NS,
        price: 100,
        bidder: 1,
        channel: HOT_CHANNEL_MAX + 42,
        channel_id: None,
        url: "u".to_string(),
        extra: String::new(),
    });
    assert_eq!(v["channel_id"], "");
}
