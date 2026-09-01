//! `wfgen gen-nexmark` 命令：确定性生成 NEXMark 事件流（Person/Auction/Bid）。
//!
//! 生成域（NxEvent / generate_events / 自检 / 官方语义常量）在 `nexmark` 模块
//! （与 `verify-nexmark` 共用）；本模块只负责命令编排：分桶输出、质量报告、
//! `--check` 数据自检与 md5 指纹。
//!
//! Usage: `wfgen gen-nexmark <count> [--seed N] [--no-sort] [--check]`

use std::fs::File;
use std::io::Write;
use std::sync::Arc;

use crate::error::{self, WfgenReason, WfgenResult};
use crate::progress::fmt_num;

use crate::nexmark::{AUCTION_ID_LEAD, FIRST_AUCTION_ID, FIRST_PERSON_ID, NUM_ACTIVE_PEOPLE};
use crate::nexmark::{
    BASE_NS, BUCKET_NS, INTER_EVENT_DELAY_NS, NxEvent, check_event, generate_events, nx_to_value,
};
use crate::nexmark::{NUM_IN_FLIGHT_AUCTIONS, PERSON_ID_LEAD};

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
    m.insert("_stream".to_string(), serde_json::json!(stream));
    m.insert("_window".to_string(), serde_json::json!(stream));
    m.insert("_timestamp".to_string(), serde_json::json!(iso(ns)));
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

/// `wfgen gen-nexmark` 参数：确定性生成 NEXMark 事件流（Person/Auction/Bid）。
#[derive(clap::Args)]
pub struct Args {
    /// Number of events to generate
    pub count: i64,

    /// RNG seed for deterministic output
    #[arg(long, default_value_t = 1)]
    pub seed: u64,

    /// Emit phase-major generation order instead of event-time order
    /// (pre-2026-08-20 behavior; breaks `over`-window time eviction)
    #[arg(long)]
    pub no_sort: bool,

    /// 生成自检：生成后独立检查阶段（同一 seed 重放，独立进度条），
    /// 逐事件值域校验 + 输出字节 md5 指纹
    /// （报告写 stderr；stdout 仍是数据流，可与 --no-sort 之外的管道共用）
    #[arg(long)]
    pub check: bool,
}

pub fn run(args: Args) -> WfgenResult<()> {
    run_checked(args, false)
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

/// 生成 + 可选自检（`--check`）：生成阶段（进度条）→ 数据检查阶段（`--check`
/// 独立进度条：同一 seed 确定性重放事件流，逐事件值域校验）+ 输出字节 md5 指纹
/// （确定性锚点：同 seed+count 恒等；桶序模式 = 输出文件 md5）。
///
/// 报告写 stderr（stdout 是数据流）：默认输出简短质量报告（行数/时间/乱序），
/// `--check` 追加值域违规与 md5 指纹。
pub fn run_checked(args: Args, force_check: bool) -> WfgenResult<()> {
    let Args {
        count,
        seed,
        no_sort,
        check,
    } = args;
    let check = check || force_check;
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
                    if !(lo..(n_person_so_far + PERSON_ID_LEAD)).contains(&seller_base0)
                        || (seller_base0 < n_person_so_far && !person_ids.contains(&seller))
                    {
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
                        || (auc_base0 <= last_auc && !auction_ids.contains(&auc))
                    {
                        ref_violations += 1;
                    }
                    let bidder_base0 = bidder - FIRST_PERSON_ID;
                    let lo = (n_person_so_far - NUM_ACTIVE_PEOPLE).max(0);
                    if !(lo..(n_person_so_far + PERSON_ID_LEAD)).contains(&bidder_base0)
                        || (bidder_base0 < n_person_so_far && !person_ids.contains(&bidder))
                    {
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
                digest
                    .iter()
                    .map(|b| format!("{:02x}", b))
                    .collect::<String>()
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
