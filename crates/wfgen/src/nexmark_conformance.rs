//! Flink NEXMark 测试集定义符合性声明（`gen-nexmark --check` / `verify-nexmark`
//! 的「符合性」报告内容）。
//!
//! 参照系：Flink 官方 NEXMark benchmark 库 [`nexmark/nexmark`]（原 flink-benchmarks，
//! Alibaba NEXMark 白皮书同源，VVR 基线亦用此库生成数据），默认配置
//! [`NexmarkConfiguration`]（personProportion=1 / auctionProportion=3 /
//! bidProportion=46、hotAuctionRatio=2 → 50% hot auction、hotSellersRatio=4 /
//! hotBiddersRatio=4 → 75% hot seller/bidder、numInFlightAuctions=100、
//! numActivePeople=1000、outOfOrderGroupSize=1）与其 generator 公式
//! （`lastBase0PersonId`/`nextBase0PersonId`/`lastBase0AuctionId`/
//! `nextBase0AuctionId`/`nextPrice`/`nextAuctionLengthMs`/channel 生成）。
//!
//! 2026-08-21 起 wfgen **严格对齐**上述定义：比例/时间映射/ID/引用窗口/热点/价格/
//! 有效期/category/channel/city·state 均照搬官方公式（RNG 为 StdRng 而非官方
//! SplittableRandom，字节级不等价但分布语义一致）。残余差异仅为无查询引用的
//! 字符串格式与字段裁剪（见 [`DEVIATIONS`]）。
//!
//! [`nexmark/nexmark`]: https://github.com/nexmark/nexmark
//! [`NexmarkConfiguration`]: https://github.com/nexmark/nexmark/blob/master/nexmark-flink/src/main/java/com/github/nexmark/flink/NexmarkConfiguration.java

/// 与 Flink 官方严格对齐的项（✅），按生成器逻辑分组。
const ALIGNED: &[&str] = &[
    "事件类型比例 2%/6%/92%（person/auction/bid）= Flink personProportion=1/auctionProportion=3/bidProportion=46（total=50）",
    "事件时间 = 事件序号线性映射（Flink timestampForEvent = baseTime + n×interEventDelay；wfgen = BASE_NS + n×SPAN/count），等价固定速率",
    "事件流严格按事件时间递增、无乱序（Flink outOfOrderGroupSize=1 默认）",
    "person/auction id 各自从 1000 起、唯一、递增（FIRST_PERSON_ID/FIRST_AUCTION_ID=1000 同源同值）",
    "引用窗口 = 官方 lastBase0*/nextBase0* 公式：seller/bidder 75% 热点（最近 100 人批次第 1/2 人）+ 25% 最近 numActivePeople=1000 人 ±10 lead；bid.auction 50% 热点（最近 100 个批次第 1 个）+ 50% 最近 numInFlightAuctions=100 ±10 lead（lead 允许引用未来实体，官方语义）",
    "hot auction 占比 50%（hotAuctionRatio=2 → P=1-1/2=50%）；hot seller/bidder 占比 75%（hotSellers/BiddersRatio=4）",
    "价格对数均匀：initialBid/reserve/bid.price 均 = 官方 nextPrice = round(10^(6u)×100) ∈ [100, 1e8)，与 auction 冷热无关",
    "auction 有效期 = 官方 nextAuctionLengthMs = 1 + [0, 2×horizon) ms，horizon = 未来 numInFlightAuctions=100 个 auction 的生成间隔",
    "category ∈ 10..14（FIRST_CATEGORY_ID=10 + rand(NUM_CATEGORIES=5)）",
    "channel：50% 热门 4 通道（Google/Facebook/Baidu/Apple，HOT_CHANNELS_RATIO=2）+ 50% channel-0..9999",
    "city/state：官方 PersonGenerator 10 城 / 6 州（AZ,CA,ID,OR,WA,WY），独立随机",
];

/// 残余差异（⚠️，均为无查询引用的格式/裁剪项，`_` 字段不被任何查询读取）：
/// (说明, 理由与影响)。
const DEVIATIONS: &[(&str, &str)] = &[
    (
        "name/email/itemName/description/url 为确定性模板（官方为随机字符串）",
        "本地 schema 裁剪；无查询引用（官方 Q8/Q10 按 city/state 分组已对齐值域），零影响",
    ),
    (
        "无 creditCard/extra 填充字段（官方 Person 含 creditCard、各事件含 extra 补齐到 avgByteSize）",
        "本地 schema 无 creditCard，extra 恒为 \"\"；无查询引用，零影响",
    ),
    (
        "bidder id 单加 FIRST_PERSON_ID（nexmark-flink 存在 bidder 双加 1000 的 bug）",
        "按 Beam 官方语义单加；本地查询集无 bidder→person join（Q3/Q9 用 auction.seller），零影响",
    ),
    (
        "channel/url 用确定性 StdRng（nexmark-flink 的 HOT_URLS/CHANNEL_URL_CACHE 用静态 SplittableRandom，进程间不确定）",
        "为保证同 seed 字节级确定性重放，刻意用确定 RNG；分布语义（50%/50%、4 热通道）与官方一致",
    ),
];

/// 输出符合性报告（多行文本，stderr 用；不污染 stdout 数据流/对拍输出）。
/// `brief=false`（--check）输出全部对齐项与残余差异；`brief=true`（verify 尾部）
/// 只输出残余差异摘要 + 结论。
pub fn report(brief: bool) -> String {
    let mut s = String::new();
    s.push_str("== NEXMark 符合性（严格对齐 Flink 官方 nexmark/nexmark 默认配置）==\n");
    if !brief {
        for line in ALIGNED {
            s.push_str(&format!("  ✅ {line}\n"));
        }
    }
    for (name, impact) in DEVIATIONS {
        if brief {
            s.push_str(&format!("  ⚠️ 残余差异 {name}\n"));
        } else {
            s.push_str(&format!("  ⚠️ 残余差异 {name}：{impact}\n"));
        }
    }
    s.push_str(
        "  结论 生成语义（比例/时间/ID/引用窗口/热点/价格/有效期/category/channel/city·state）\n",
    );
    s.push_str(
        "        已与 Flink 官方默认配置逐项对齐；残余差异仅为无查询引用的字符串模板与字段裁剪。\n",
    );
    s.push_str("        正确性对拍（oracle vs 引擎同数据）与白皮书/VVR 数字对比的可比性均不受残余差异影响。\n");
    s.push_str("        逐项对照见 wf-examples/performance/nexmark_pk/NEXMARK_CONFORMANCE.md。\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_contains_aligned_and_deviations() {
        let full = report(false);
        assert!(full.contains("事件类型比例 2%/6%/92%"));
        assert!(full.contains("价格对数均匀"));
        assert!(full.contains("有效期"));
        assert!(full.contains("category ∈ 10..14"));
        assert!(full.contains("残余差异"));
        assert!(full.contains("结论"));
        let brief = report(true);
        assert!(brief.contains("残余差异"));
        assert!(!brief.contains("事件类型比例 2%/6%/92%"));
    }
}
