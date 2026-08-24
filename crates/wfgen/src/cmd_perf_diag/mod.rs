//! `wfgen perf-diag` — 性能诊断驱动（sentinel 漂流瓶协议）。
//!
//! 与 daemon 读同一份 `perf-diag.toml`（诊断档列表 = 轮数）。对每个诊断档 k：
//!
//! 1. 轮询 `perf_sentinel.ndjson` 直到 `stage{current=k}`（引擎已切换好档 k）；
//! 2. `T0 = now()`；发预编码帧前缀（覆盖 N 行）+ 帧尾追加
//!    `__wf_sentinel{round=k, n=n_k, start_ns=T0}` 帧（同连接同 seq 尾部）；
//! 3. 轮询哨兵文件直到 `sentinel{round=k, n=n_k}`（含引擎补的 `emit_ns`）；
//! 4. `EPS = n_k / (emit_ns − start_ns)`（全程无外部记账）。
//!
//! 每 (点, N) 取多轮 max（`--rounds`），输出墙表（EPS 单调 → 增量成本归属）。
//! 数据由小到大（`--n-list "100k,1m,3m"`）：小 N 秒级出方向，大 N 区分
//! per-event 墙 vs 固定开销墙。

use std::path::PathBuf;
use std::time::Duration;

use orion_error::conversion::SourceErr;
use tokio::io::AsyncWriteExt;

use wf_config::PerfConfig;

use crate::error;
use crate::error::{WfgenReason, WfgenResult};

mod frames;
mod sentinel;
#[cfg(test)]
mod tests;

pub(crate) use frames::*;
pub(crate) use sentinel::*;

// ---------------------------------------------------------------------------
// 参数
// ---------------------------------------------------------------------------

/// `wfgen perf-diag` 命令行参数（CLI 直通，字段即 clap 旗标）。
#[derive(clap::Args)]
pub struct Args {
    /// 诊断配置（--diag conf/perf-diag.toml；[[stages]] 列表 = 轮数）
    #[arg(long)]
    pub diag: PathBuf,
    /// 预编码帧文件（wfgen dump-frames 产物，数据部分）
    #[arg(long)]
    pub frames: PathBuf,
    /// TCP 数据端口
    #[arg(long, default_value = "127.0.0.1:9800")]
    pub addr: String,
    /// 数据量列表（"100k,1m,3m"；缺省 = 帧文件全部行）
    #[arg(long)]
    pub n_list: Option<String>,
    /// 每点轮数（取 max，降负载噪声）
    #[arg(long, default_value = "1")]
    pub rounds: usize,
    /// 哨兵记录文件（默认 data/perf_sentinel.ndjson）
    #[arg(long)]
    pub sentinels: Option<PathBuf>,
    /// 墙表输出文件（默认 data/perf_diag_wall.txt）
    #[arg(long)]
    pub output: Option<PathBuf>,
    /// 单次等待（切换/哨兵记录）超时秒数
    #[arg(long, default_value = "60")]
    pub timeout_secs: u64,
}

// ---------------------------------------------------------------------------
// 驱动
// ---------------------------------------------------------------------------

/// 执行一轮诊断：切档 → 发帧+哨兵 → 读完成信号 → 算 EPS。
pub async fn run_perf_diag(args: Args) -> WfgenResult<()> {
    let config = PerfConfig::load(&args.diag).map_err(|e| {
        error::error(
            WfgenReason::Validation,
            format!("load {}: {e}", args.diag.display()),
        )
    })?;
    let stages = config.stages;
    if stages.is_empty() {
        return Err(error::error(
            WfgenReason::Validation,
            format!("{} 需至少一个 [[stages]]", args.diag.display()),
        ));
    }
    let rounds = args.rounds.max(1);

    // 扫描帧文件（一次性：行数/字节区间），发送时纯字节复制。
    let frames = scan_frames(&args.frames)?;
    if frames.is_empty() {
        return Err(error::error(
            WfgenReason::Validation,
            format!("{} 无帧", args.frames.display()),
        ));
    }
    let total_rows: u64 = frames.iter().map(|f| f.rows).sum();
    let data = std::fs::read(&args.frames).source_err(
        WfgenReason::Io,
        format!("reading {}", args.frames.display()),
    )?;

    let n_list = if let Some(spec) = &args.n_list {
        parse_n_list(spec)?
    } else {
        vec![total_rows]
    };
    for &n in &n_list {
        if n > total_rows {
            return Err(error::error(
                WfgenReason::Validation,
                format!("--n-list 含 {n} 行，但帧文件仅 {total_rows} 行"),
            ));
        }
    }

    let sentinels = args
        .sentinels
        .unwrap_or_else(|| PathBuf::from("data/perf_sentinel.ndjson"));
    let output = args
        .output
        .unwrap_or_else(|| PathBuf::from("data/perf_diag_wall.txt"));
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .source_err(WfgenReason::Io, format!("creating {}", parent.display()))?;
    }
    let timeout = Duration::from_secs(args.timeout_secs.max(1));

    println!(
        "== perf-diag: stages={} n-list={:?} rounds={} frames={} total_rows={} ==",
        stages.len(),
        n_list,
        rounds,
        frames.len(),
        total_rows
    );

    let mut wall_lines: Vec<String> = Vec::new();
    for (k, stage) in stages.iter().enumerate() {
        // 1. 等引擎切到档 k（启动即 stages[0]，后续 sentinel 驱动）。
        wait_for_stage(&sentinels, k, timeout).await?;
        println!("== stage {k} [{}] applied — sending ==", stage.name);
        for &n_target in &n_list {
            let mut best_eps = 0.0f64;
            for r in 0..rounds {
                // 2. 帧前缀（覆盖 n_target 行）+ 哨兵帧；T0 = 构建时刻 ≈ 发送开始。
                let (prefix, sent_n) = prefix_for_n(&frames, &data, n_target);
                let start_ns = now_nanos();
                let sentinel_frame = build_sentinel_frame(k as i64, sent_n as i64, start_ns)?;
                let mut payload = Vec::with_capacity(prefix.len() + sentinel_frame.len());
                payload.extend_from_slice(prefix);
                payload.extend_from_slice(&sentinel_frame);
                send_payload(&args.addr, &payload).await?;

                // 3. 读完成信号：sentinel{round=k, n=sent_n}（第 r 条）。
                let rec =
                    wait_for_sentinel(&sentinels, k as i64, sent_n as i64, r, timeout).await?;
                let eps = compute_eps(
                    rec.n.unwrap_or(sent_n as i64),
                    rec.start_ns.unwrap(),
                    rec.emit_ns.unwrap(),
                )
                .ok_or_else(|| {
                    error::error(
                        WfgenReason::Validation,
                        format!(
                            "sentinel 时间序异常: emit_ns={:?} start_ns={:?}",
                            rec.emit_ns, rec.start_ns
                        ),
                    )
                })?;
                best_eps = best_eps.max(eps);
                println!(
                    "  {}/{}: sent {} rows in {:?} → eps={:.0}",
                    stage.name,
                    r + 1,
                    sent_n,
                    Duration::from_nanos((rec.emit_ns.unwrap() - rec.start_ns.unwrap()) as u64),
                    eps
                );
            }
            wall_lines.push(format!(
                "{}  eps={:.0} n={} rounds={}",
                stage.name, best_eps, n_target, rounds
            ));
        }
    }

    let table = wall_lines.join("\n");
    std::fs::write(&output, table.clone() + "\n")
        .source_err(WfgenReason::Io, format!("writing {}", output.display()))?;
    println!(
        "\n== wall table ==\n{table}\n== done: 结果在 {} ==",
        output.display()
    );
    Ok(())
}

/// 单连接发送载荷（字节复制，零解析）并 shutdown。
pub(crate) async fn send_payload(addr: &str, payload: &[u8]) -> WfgenResult<()> {
    let stream = tokio::net::TcpStream::connect(&addr).await.source_err(
        WfgenReason::Network,
        format!("connecting to runtime: {addr}"),
    )?;
    stream
        .set_nodelay(true)
        .source_err(WfgenReason::Network, "set_nodelay")?;
    let mut sink = stream;
    sink.write_all(payload)
        .await
        .source_err(WfgenReason::Network, "tcp send")?;
    sink.shutdown()
        .await
        .source_err(WfgenReason::Network, "tcp shutdown")?;
    Ok(())
}

