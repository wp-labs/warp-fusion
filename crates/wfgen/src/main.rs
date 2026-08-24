use clap::{Parser, Subcommand};

use wfgen::error::WfgenResult;

#[derive(Parser)]
#[command(
    name = "wfgen",
    version,
    about = "WarpFusion test data generator",
    propagate_version = true
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

/// 每个子命令的参数定义与实现都在各自的 `cmd_*` 模块里（`cmd_*::Args` +
/// `cmd_*::run`），这里只做命令注册与分发。
#[derive(Subcommand)]
enum Commands {
    /// Generate test data from a .wfg scenario file
    Gen(wfgen::cmd_gen::Args),
    /// Generate deterministic NEXMark events (Person/Auction/Bid) as JSONL
    GenNexmark(wfgen::cmd_gen_nexmark::Args),
    /// NEXMark 引擎结果验证：用真实 WFL 规则引擎（wf_engine）处理
    /// wfgen 生成的事件，产出各规则应 EMIT 计数（JSON），供与引擎
    /// daemon 实际 EMIT 对拍（nexmark_pk/bench.sh --verify）
    VerifyNexmark(wfgen::cmd_verify_nexmark::Args),
    /// 分层文件比对（L1 哈希相同性 → L2 Myers 差异量 → L3 --detail 定位）
    Diff(wfgen::cmd_diff::Args),
    /// Lint (validate) a .wfg scenario file
    Lint(wfgen::cmd_lint::Args),
    /// Verify actual alerts against oracle expectations
    Verify(wfgen::cmd_verify::Args),
    /// Send generated JSONL events to wfusion over TCP + Arrow IPC
    Send(wfgen::cmd_send::Args),
    /// Pre-encode JSONL events into Arrow frames for raw byte replay
    DumpFrames(wfgen::cmd_frames::DumpFramesArgs),
    /// Replay pre-encoded Arrow frame bytes over `connections` concurrent TCP
    /// connections (no JSON parsing / Arrow encoding on the hot path).
    /// `connections>1` is the C-UCP supply lever: the runtime's TCP source
    /// round-robins the connections across its `instances` reader loops.
    SendArrow(wfgen::cmd_frames::SendArrowArgs),
    /// Split a frame file into N key-sharded frame files (one per shard;
    /// same key always lands in the same file). Send them later with
    /// `send-arrow --shard-files` for zero-decode multi-connection replay.
    ShardFrames(wfgen::cmd_frames::ShardFramesArgs),
    /// Measure generation throughput (optional TCP send to wfusion)
    Bench(wfgen::cmd_bench::Args),
    /// Continuous data generation (daemon mode)
    Stream(wfgen::cmd_stream::Args),
    /// 性能诊断驱动（sentinel 漂流瓶协议，与 daemon 读同一份 perf-diag.toml）
    PerfDiag(wfgen::cmd_perf_diag::Args),
}

#[tokio::main]
async fn main() {
    if let Err(err) = run_cli().await {
        eprintln!("{}", err.report().render());
        std::process::exit(1);
    }
}

async fn run_cli() -> WfgenResult<()> {
    match Cli::parse().command {
        Commands::Gen(a) => wfgen::cmd_gen::run(a).await,
        Commands::GenNexmark(a) => wfgen::cmd_gen_nexmark::run_checked(a, false),
        Commands::VerifyNexmark(a) => wfgen::cmd_verify_nexmark::run(a),
        Commands::Diff(a) => {
            let same = wfgen::cmd_diff::run(&a)?;
            if !same {
                std::process::exit(1);
            }
            Ok(())
        }
        Commands::Lint(a) => wfgen::cmd_lint::run(a),
        Commands::Verify(a) => wfgen::cmd_verify::run(a),
        Commands::Send(a) => wfgen::cmd_send::run(a).await,
        Commands::DumpFrames(a) => wfgen::cmd_frames::dump_frames(a).await,
        Commands::SendArrow(a) => wfgen::cmd_frames::send_arrow(a).await,
        Commands::ShardFrames(a) => wfgen::cmd_frames::shard_frames(a).await,
        Commands::Bench(a) => wfgen::cmd_bench::run(a).await,
        Commands::Stream(a) => wfgen::cmd_stream::run(a).await,
        Commands::PerfDiag(a) => wfgen::cmd_perf_diag::run_perf_diag(a).await,
    }
}

#[cfg(test)]
mod tests {
    use clap::error::ErrorKind;

    use super::*;

    #[test]
    fn top_level_version_flag_is_available() {
        let result = Cli::try_parse_from(["wfgen", "--version"]);
        let err = match result {
            Ok(_) => panic!("--version should stop parsing with version output"),
            Err(err) => err,
        };
        assert_eq!(err.kind(), ErrorKind::DisplayVersion);
        assert!(err.to_string().contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn gen_out_optional_when_send_given() {
        // --send without --out must parse (out is now optional).
        let cli = Cli::try_parse_from(["wfgen", "gen", "--scenario", "x.wfg", "--send"]);
        assert!(cli.is_ok(), "expected parse success, got: {:?}", cli.err());
    }

    #[test]
    fn gen_no_wfl_flag_parses() {
        let cli = Cli::try_parse_from([
            "wfgen",
            "gen",
            "--scenario",
            "x.wfg",
            "--out",
            "out",
            "--no-wfl",
        ]);
        assert!(cli.is_ok(), "expected parse success, got: {:?}", cli.err());
    }

    #[test]
    fn gen_no_oracle_flag_parses() {
        // --no-oracle is a distinct flag (skip the WFL pipeline and oracle);
        // must parse independently of --no-wfl.
        let cli = Cli::try_parse_from([
            "wfgen",
            "gen",
            "--scenario",
            "x.wfg",
            "--out",
            "out",
            "--no-oracle",
        ]);
        assert!(cli.is_ok(), "expected parse success, got: {:?}", cli.err());
    }

    #[test]
    fn gen_no_wfl_and_no_oracle_both_parse() {
        let cli = Cli::try_parse_from([
            "wfgen",
            "gen",
            "--scenario",
            "x.wfg",
            "--out",
            "out",
            "--no-wfl",
            "--no-oracle",
        ]);
        assert!(cli.is_ok(), "expected parse success, got: {:?}", cli.err());
    }
}
