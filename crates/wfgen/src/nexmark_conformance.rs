//! Flink NEXMark 测试集定义符合性声明（`gen-nexmark --check` / `verify-nexmark`
//! 的「符合性」报告内容）。
//!
//! 参照系：Flink 官方 NEXMark benchmark 库 [`nexmark/nexmark`]（原 flink-benchmarks，
//! Alibaba NEXMark 白皮书同源，VVR 基线亦用此库生成数据），默认配置
//! [`NexmarkConfiguration`]（personProportion=1 / auctionProportion=3 /
//! bidProportion=46、hotAuctionRatio=2、hotSellersRatio=4、hotBiddersRatio=4、
//! numInFlightAuctions=100、numActivePeople=1000、outOfOrderGroupSize=1）。
//!
//! 本模块把「生成器设计层面」的对照结论静态输出：与 `count` 无关的项
//! （比例/时间映射/ID 起始/热点机制/价格与有效期分布域）逐条给出 ✅（对齐）
//! 或 ⚠️（刻意偏离 + 理由 + 影响），供 `--check` / `verify` 报告引用，避免
//! 每次跑批重复推敲。逐事件值域/引用校验仍在 `run_checked` 内动态完成。
//!
//! [`nexmark/nexmark`]: https://github.com/nexmark/nexmark
//! [`NexmarkConfiguration`]: https://github.com/nexmark/nexmark/blob/master/nexmark-flink/src/main/java/com/github/nexmark/flink/NexmarkConfiguration.java

/// 结构语义与 Flink 官方一致的项（✅）。
const ALIGNED: &[&str] = &[
    "事件类型比例 2%/6%/92%（person/auction/bid）= Flink personProportion=1/auctionProportion=3/bidProportion=46（total=50）",
    "事件时间 = 事件序号线性映射（Flink timestampForEvent = baseTime + n×interEventDelay；wfgen = BASE_NS + n×SPAN/count），等价固定速率",
    "事件流严格按事件时间递增、无乱序（Flink outOfOrderGroupSize=1 默认）",
    "person/auction id 各自从 1000 起、唯一、递增（FIRST_PERSON_ID/FIRST_AUCTION_ID=1000 同源同值）",
    "引用完整性：auction.seller / bid.bidder 引用已出现 person、bid.auction 引用已出现 auction（Flink 'most primary key/foreign key relations are correct'，wfgen 更严：引用必已生成）",
    "hot auction 占比 50%（Flink hotAuctionRatio=2 → P=1-1/2=50%）",
];

/// 与 Flink 官方默认配置刻意偏离的项（⚠️）：每项 = 说明 + 理由 + 对查询语义/性能的影响。
const DEVIATIONS: &[(&str, &str, &str)] = &[
    (
        "热点（hot seller/bidder）机制",
        "wfgen：hot 50%（最近 15s 时间窗选人）/ cold 50%（最近 60s 窗）；Flink：hot 75%（hotSellersRatio=hotBiddersRatio=4，热点=最近 4 人批次第 1/2 人）、cold=最近 numActivePeople=1000 人 ±10 lead",
        "wfgen 用固定时间窗替代『最近 N 人』，保证 30m 固定 span 下引用密度不随 count 退化（Flink 官方注释明说 numActivePeople 就是为此）。影响：活跃实体域形状不同（q12 活跃 bidder 域 ~7k vs Flink ~1000+）、Q3/Q9 join 命中面集中度不同",
    ),
    (
        "bid 价格分布",
        "wfgen：hot auction 出价 [100,500]、cold [10,150]（阶梯分段）；Flink：PriceGenerator.nextPrice = round(10^(6u)×100) 对数均匀 [100, 10^8]，与 auction 是否 hot 无关",
        "影响价格阈值类查询命中面：官方 Q7 阈值 10000 在 wfgen 数据下永不命中（本地 q7 阈值 200/500/1000 为适配数据口径的改写）；Q5/Q9 价格相关过滤同理。性能口径无碍（对拍是同数据 oracle vs 引擎）",
    ),
    (
        "auction 有效期（in-flight 面）",
        "wfgen：expires = ns + [600s, 1800s]（固定 10-30 分钟）；Flink：1 + nextLong(2×horizonMs)，horizon≈numInFlightAuctions=100 个 auction 的生成间隔（30M/30min 口径下 ~百 ms 级）",
        "影响同时活跃 auction 数量（wfgen 十万级 vs Flink ~100）与 join 匹配面，进而影响 Q4/Q6/Q17 窗口语义与内存面。wfgen 用 10m fixed 窗口吸收，语义上等价『超长有效期』但性能面显著更大——对拍基准仍自洽，与白皮书数字对比时须披露",
    ),
    (
        "category 域",
        "wfgen：均匀 1..=26（26 类）；Flink：FIRST_CATEGORY_ID=10 + rand(5) → 10..14（5 类）",
        "影响按 category 分组查询（Q5/Q11/Q20 类）的桶数与每桶密度；两类下分布形状相似（均匀），桶数不同",
    ),
    (
        "channel 域",
        "wfgen：5 个固定通道（Google/Facebook/Apple/Direct/Test）均匀；Flink：50% 热门 4 通道（Google/Facebook/Baidu/Apple）+ 50% channel-0..9999",
        "影响按 channel 分组查询（官方 Q8 类）的值域；本地查询集未按 channel 分组，无实际影响",
    ),
    (
        "字符串字段格式",
        "wfgen：name=person_{id}、email=person{id}@example.com、8 城/8 州（city↔state 成对）、url=http://www.example.com/{n}；Flink：随机 first+last 姓名、随机 email、10 城/6 州（独立随机）、https 长 url",
        "city/state 只影响按地理分组的查询（官方 Q8/Q10 类，本地未实现）；name/email/url 无查询引用",
    ),
];

/// 输出符合性报告（多行文本，stderr 用；不污染 stdout 数据流/对拍输出）。
/// `brief=false`（--check）输出全部对齐项与偏离项；`brief=true`（verify 尾部）
/// 只输出偏离项摘要 + 结论，避免刷屏。
pub fn report(brief: bool) -> String {
    let mut s = String::new();
    s.push_str("== NEXMark 符合性（对照 Flink 官方 nexmark/nexmark 默认配置）==\n");
    if !brief {
        for line in ALIGNED {
            s.push_str(&format!("  ✅ {line}\n"));
        }
    }
    for (name, detail, impact) in DEVIATIONS {
        if brief {
            s.push_str(&format!("  ⚠️ 偏离 {name}\n"));
        } else {
            s.push_str(&format!(
                "  ⚠️ 偏离 {name}：\n     - {detail}\n     - 影响：{impact}\n"
            ));
        }
    }
    s.push_str(
        "  结论 结构骨架（比例/时间映射/ID 起始/引用完整性/hot auction 50%）与 Flink 官方一致；\n",
    );
    s.push_str(
        "        随机分布参数（价格/有效期/热点选择/category·channel 域/字符串格式）为 30m 固定\n",
    );
    s.push_str("        span + 字节级确定性重放目的而刻意偏离（见 wf-examples/performance/nexmark_pk/NEXMARK_CONFORMANCE.md）。\n");
    s.push_str("        正确性对拍（oracle vs 引擎同数据）不受影响；与白皮书/VVR 性能数字对比时须披露上述偏离。\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_contains_aligned_and_deviations() {
        let full = report(false);
        assert!(full.contains("事件类型比例 2%/6%/92%"));
        assert!(full.contains("偏离 热点"));
        assert!(full.contains("偏离 bid 价格分布"));
        assert!(full.contains("偏离 auction 有效期"));
        assert!(full.contains("偏离 category 域"));
        assert!(full.contains("偏离 channel 域"));
        assert!(full.contains("偏离 字符串字段格式"));
        assert!(full.contains("结论"));
        let brief = report(true);
        assert!(brief.contains("偏离 bid 价格分布"));
        assert!(!brief.contains("事件类型比例 2%/6%/92%"));
    }
}
