// ---------------------------------------------------------------------------
// warp-fusion unified CLI entry point
// Binary: wfusion
// ---------------------------------------------------------------------------

mod admin_api;
mod cli_config;
mod error;
mod register;

use clap::{Parser, Subcommand};
use orion_error::report::DiagnosticReport;
use std::path::PathBuf;

use cli_config::{ConfigLoadArgs, run_engine_command};
use error::CliResult;
use wf_config::FusionMode;

// Thread-local-free, cross-thread scalable allocator: the engine runs many
// concurrent workers (parse pools, sharded rule tasks, sink writers) whose
// hot paths allocate small objects at high frequency. The system allocator's
// per-zone locks serialize those threads (verified by sample: __ulock_wait2
// under _malloc_zone_realloc on the on-each emit path with 6 rule shards).
#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// 分配器内存分账 provider（`wf_runtime::metrics::alloc_stats`）：用 mimalloc 的
/// `mi_process_info` 报告进程级 RSS/commit 峰值。
///
/// 存在的理由：引擎会计的 `window.memory_bytes` 与进程峰值之间长期有巨大缺口
/// （q13 100M：窗口 5.7GB vs 峰值 26GB），而缺口的**性质**决定修复方向——
/// `peak_commit ≫ 窗口合计` = 引擎真持有（找持有者）；`peak_commit ≈ 窗口合计`
/// 但 `peak_rss` 远大 = 段区/OS 伪影（看归还策略）。此前只能靠外部 footprint
/// 采样推断，且单点采样已误判过一次。macOS 上 mimalloc 精确报告 rss。
///
/// 见 `wp-reactor/docs/issues/q13-memory-peak-scales-with-volume.md`。
fn mimalloc_stats() -> wf_runtime::metrics::alloc_stats::AllocStats {
    let mut current_rss = 0usize;
    let mut peak_rss = 0usize;
    let mut current_commit = 0usize;
    let mut peak_commit = 0usize;
    let mut page_faults = 0usize;
    // 全部为 out-param（可空）；只取内存四项 + 缺页，时间项传 null。
    unsafe {
        libmimalloc_sys::mi_process_info(
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut current_rss,
            &mut peak_rss,
            &mut current_commit,
            &mut peak_commit,
            &mut page_faults,
        );
    }
    wf_runtime::metrics::alloc_stats::AllocStats {
        current_rss: current_rss as u64,
        peak_rss: peak_rss as u64,
        current_commit: current_commit as u64,
        peak_commit: peak_commit as u64,
        page_faults: page_faults as u64,
    }
}

// -- Top-level CLI -----------------------------------------------------------

#[derive(Parser)]
#[command(
    name = "wfusion",
    version,
    about = "WarpFusion CEP engine & tooling",
    propagate_version = true
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start engine in daemon mode (continuous, listens for input)
    Daemon {
        #[command(flatten)]
        load: ConfigLoadArgs,
        #[arg(long)]
        metrics: bool,
        #[arg(long)]
        metrics_interval: Option<String>,
        #[arg(long)]
        metrics_listen: Option<String>,
        /// 性能诊断模式配置（--perf-diag conf/perf-diag.toml）；不带 = 全关
        #[arg(long)]
        perf_diag: Option<PathBuf>,
    },
    /// Start engine in batch mode (replay input files, exit when done)
    Batch {
        #[command(flatten)]
        load: ConfigLoadArgs,
        #[arg(long)]
        metrics: bool,
        #[arg(long)]
        metrics_interval: Option<String>,
        #[arg(long)]
        metrics_listen: Option<String>,
        #[arg(long)]
        perf_diag: Option<PathBuf>,
    },
    /// Print version or check version requirement
    Version {
        /// Check if current version >= specified version (e.g. "0.1.0")
        /// Exits with code 0 if satisfied, 1 otherwise.
        #[arg(long)]
        ge: Option<String>,
    },
}

// -- Main entry --------------------------------------------------------------

#[tokio::main]
async fn main() {
    if let Err(err) = run_cli().await {
        let report: DiagnosticReport = err.report();
        eprintln!("{}", report.render());
        std::process::exit(1);
    }
}

async fn run_cli() -> CliResult<()> {
    // 分配器内存分账：尽早装入（在任何引擎/metrics 任务启动前），使
    // metrics.ndjson 从第一个采样区间就带 alloc.* 指标。幂等（OnceLock）。
    wf_runtime::metrics::alloc_stats::install_provider(mimalloc_stats);
    let cli = Cli::parse();

    match cli.command {
        Commands::Daemon {
            load,
            metrics,
            metrics_interval,
            metrics_listen,
            perf_diag,
        } => {
            run_engine_command(
                load,
                Some(FusionMode::Daemon),
                metrics,
                metrics_interval,
                metrics_listen,
                perf_diag,
            )
            .await?
        }
        Commands::Batch {
            load,
            metrics,
            metrics_interval,
            metrics_listen,
            perf_diag,
        } => {
            run_engine_command(
                load,
                Some(FusionMode::Batch),
                metrics,
                metrics_interval,
                metrics_listen,
                perf_diag,
            )
            .await?
        }
        Commands::Version { ge } => {
            let current = env!("CARGO_PKG_VERSION");
            match ge {
                Some(required) => {
                    if version_ge(current, &required) {
                        println!("{current} >= {required}");
                    } else {
                        eprintln!("{current} < {required}");
                        std::process::exit(1);
                    }
                }
                None => {
                    println!("wfusion {current}");
                }
            }
        }
    }

    Ok(())
}

/// Compare two semver-like version strings (e.g. "0.1.11" >= "0.1.0").
/// Returns true if `current >= required`.
fn version_ge(current: &str, required: &str) -> bool {
    let parse = |s: &str| -> Vec<u32> { s.split('.').filter_map(|p| p.parse().ok()).collect() };
    let c = parse(current);
    let r = parse(required);
    for i in 0..c.len().max(r.len()) {
        let cv = c.get(i).copied().unwrap_or(0);
        let rv = r.get(i).copied().unwrap_or(0);
        if cv > rv {
            return true;
        }
        if cv < rv {
            return false;
        }
    }
    true
}
