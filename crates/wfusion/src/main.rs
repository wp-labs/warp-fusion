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
