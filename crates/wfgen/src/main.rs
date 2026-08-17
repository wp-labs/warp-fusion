use std::path::PathBuf;

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

#[derive(Subcommand)]
enum Commands {
    /// Generate test data from a .wfg scenario file
    Gen {
        /// Path to the .wfg scenario file
        #[arg(long)]
        scenario: PathBuf,

        /// Output format: "jsonl" or "arrow" ("arrow-ipc"/"ipc" aliases)
        #[arg(long, default_value = "jsonl")]
        format: String,

        /// Output directory. Optional when --send is used; at least one of
        /// --out / --send must be given.
        #[arg(long)]
        out: Option<PathBuf>,

        /// Additional .wfs schema files (beyond those in `use` declarations)
        #[arg(long)]
        ws: Vec<PathBuf>,

        /// Additional .wfl rule files (beyond those in `use` declarations)
        #[arg(long)]
        wfl: Vec<PathBuf>,

        /// Skip the entire WFL pipeline: no rule loading, no `_global.wfl` /
        /// yield-preset evaluation, no compilation, no injection-aware event
        /// generation, and no oracle/expected output. Generation falls back to
        /// baseline background events.
        #[arg(long)]
        no_wfl: bool,

        /// Skip oracle/expected output only: WFL is still compiled, so
        /// injection `use()` fixed values apply and generated events are
        /// inject-aware; no `.except.jsonl` / `.except.meta.jsonl` sidecars
        /// are written. Use `--no-wfl` to also drop rule compilation.
        #[arg(long)]
        no_oracle: bool,

        /// Send generated events to wfusion over TCP + Arrow IPC
        #[arg(long)]
        send: bool,

        /// Runtime TCP address used with --send, e.g. 127.0.0.1:9800
        #[arg(long, default_value = "127.0.0.1:9800")]
        addr: String,
    },
    /// Generate deterministic NEXMark events (Person/Auction/Bid) as JSONL
    GenNexmark {
        /// Number of events to generate
        count: i64,

        /// RNG seed for deterministic output
        #[arg(long, default_value_t = 1)]
        seed: u64,
    },
    /// Lint (validate) a .wfg scenario file
    Lint {
        /// Path to the .wfg scenario file
        scenario: PathBuf,

        /// Additional .wfs schema files (beyond those in `use` declarations)
        #[arg(long)]
        ws: Vec<PathBuf>,

        /// Additional .wfl rule files (beyond those in `use` declarations)
        #[arg(long)]
        wfl: Vec<PathBuf>,
    },
    /// Verify actual alerts against oracle expectations
    Verify {
        /// Path to the oracle (expected) JSONL file
        #[arg(long)]
        expected: PathBuf,

        /// Path to the actual alerts JSONL file
        #[arg(long)]
        actual: PathBuf,

        /// Score tolerance for matching (overrides meta file if set)
        #[arg(long)]
        score_tolerance: Option<f64>,

        /// Time tolerance for matching in seconds (overrides meta file if set)
        #[arg(long)]
        time_tolerance: Option<f64>,

        /// Path to oracle meta JSON with tolerances (written by gen)
        #[arg(long)]
        meta: Option<PathBuf>,

        /// Output format: "json" or "markdown" (default: json)
        #[arg(long, default_value = "json")]
        format: String,
    },
    /// Send generated JSONL events to wfusion over TCP + Arrow IPC
    Send {
        /// Path to the .wfg scenario file (used to load schemas)
        #[arg(long)]
        scenario: PathBuf,

        /// Path to generated events JSONL file, or `-` to read stdin
        #[arg(long)]
        input: PathBuf,

        /// Runtime TCP address, e.g. 127.0.0.1:9800
        #[arg(long, default_value = "127.0.0.1:9800")]
        addr: String,

        /// Additional .wfs schema files (beyond those in `use` declarations)
        #[arg(long)]
        ws: Vec<PathBuf>,

        /// Stream in batches of this many events over one persistent
        /// connection. Omit to read the whole input and send once.
        #[arg(long)]
        chunk: Option<usize>,

        /// Sleep this many ms between streamed batches (pacing; needs --chunk)
        #[arg(long)]
        rate_ms: Option<u64>,
    },
    /// Pre-encode JSONL events into Arrow frames for raw byte replay
    DumpFrames {
        /// Path to the .wfg scenario file (used to load schemas)
        #[arg(long)]
        scenario: PathBuf,

        /// Path to generated events JSONL file, or `-` to read stdin
        #[arg(long)]
        input: PathBuf,

        /// Runtime TCP address used only to borrow the framed encoder
        #[arg(long, default_value = "127.0.0.1:9800")]
        addr: String,

        /// Additional .wfs schema files (beyond those in `use` declarations)
        #[arg(long)]
        ws: Vec<PathBuf>,

        /// Path to write the encoded frame bytes to
        #[arg(long)]
        output: PathBuf,

        /// Accumulate this many events per Arrow batch (default: one-shot,
        /// matching `send` without --chunk). Bounds per-batch memory for huge
        /// event counts.
        #[arg(long)]
        chunk: Option<usize>,

        /// Frame byte cap (default 8388608 = 8MiB). A frame is one window commit;
        /// smaller frames → lower per-batch memory, more commits.
        #[arg(long, default_value_t = wfgen::output::arrow_ipc::DEFAULT_MAX_FRAME_BYTES)]
        max_frame_bytes: usize,

        /// Frame row cap (default 100000).
        #[arg(long, default_value_t = wfgen::output::arrow_ipc::DEFAULT_MAX_FRAME_ROWS)]
        max_frame_rows: usize,
    },
    /// Replay pre-encoded Arrow frame bytes over one TCP connection (no JSON
    /// parsing / Arrow encoding on the hot path)
    SendArrow {
        /// Path to the frames file produced by `wfgen dump-frames`
        #[arg(long)]
        input: PathBuf,

        /// Runtime TCP address, e.g. 127.0.0.1:9800
        #[arg(long, default_value = "127.0.0.1:9800")]
        addr: String,
    },
    /// Measure generation throughput (optional TCP send to wfusion)
    Bench {
        /// Path to the .wfg scenario file
        #[arg(long)]
        scenario: PathBuf,

        /// Additional .wfs schema files (beyond those in `use` declarations)
        #[arg(long)]
        ws: Vec<PathBuf>,

        /// Additional .wfl rule files (beyond those in `use` declarations)
        #[arg(long)]
        wfl: Vec<PathBuf>,

        /// Sustained bench duration (e.g. "30s", "2m"). Omit for single-shot.
        #[arg(long)]
        duration: Option<String>,

        /// Send generated events to wfusion over TCP + Arrow IPC
        #[arg(long)]
        send: bool,

        /// Runtime TCP address used with --send, e.g. 127.0.0.1:9800
        #[arg(long, default_value = "127.0.0.1:9800")]
        addr: String,
    },
    /// Continuous data generation (daemon mode)
    Stream {
        /// Directory containing .wfg scenario files (cycled indefinitely)
        #[arg(long)]
        scenario_dir: PathBuf,

        /// Schema files (.wfs)
        #[arg(long)]
        ws: Vec<PathBuf>,

        /// Rule files (.wfl) — required for injection to work correctly
        #[arg(long, required = true)]
        wfl: Vec<PathBuf>,

        /// Target TCP address (wparse tcp_src)
        #[arg(long, default_value = "127.0.0.1:9800")]
        addr: String,

        /// Seconds per scenario before switching
        #[arg(long, default_value = "60")]
        interval: u64,

        /// Target event rate (events/sec). 0 = use the scenario's declared `gen N/s`
        #[arg(long, default_value = "0")]
        rate: u64,

        /// Event-time slice per batch (ms). Batch size = rate × slice, capped for bounded memory
        #[arg(long, default_value = "1000")]
        slice_ms: u64,
    },
}

#[tokio::main]
async fn main() {
    if let Err(err) = run_cli().await {
        eprintln!("{}", err.report().render());
        std::process::exit(1);
    }
}

async fn run_cli() -> WfgenResult<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Gen {
            scenario,
            format,
            out,
            ws,
            wfl,
            no_wfl,
            no_oracle,
            send,
            addr,
        } => {
            wfgen::cmd_gen::run(
                scenario, format, out, ws, wfl, no_wfl, no_oracle, send, addr,
            )
            .await
        }
        Commands::GenNexmark { count, seed } => wfgen::cmd_gen_nexmark::run(count, seed),
        Commands::Lint { scenario, ws, wfl } => wfgen::cmd_lint::run(scenario, ws, wfl),
        Commands::Verify {
            expected,
            actual,
            score_tolerance,
            time_tolerance,
            meta,
            format,
        } => wfgen::cmd_verify::run(
            expected,
            actual,
            score_tolerance,
            time_tolerance,
            meta,
            format,
        ),
        Commands::Send {
            scenario,
            input,
            addr,
            ws,
            chunk,
            rate_ms,
        } => wfgen::cmd_send::run(scenario, input, addr, ws, chunk, rate_ms).await,
        Commands::DumpFrames {
            scenario,
            input,
            addr,
            ws,
            output,
            chunk,
            max_frame_bytes,
            max_frame_rows,
        } => {
            wfgen::cmd_frames::dump_frames(
                scenario,
                input,
                addr,
                ws,
                output,
                chunk,
                max_frame_bytes,
                max_frame_rows,
            )
            .await
        }
        Commands::SendArrow { input, addr } => wfgen::cmd_frames::send_arrow(input, addr).await,
        Commands::Bench {
            scenario,
            ws,
            wfl,
            duration,
            send,
            addr,
        } => wfgen::cmd_bench::run(scenario, ws, wfl, duration, send, addr).await,
        Commands::Stream {
            scenario_dir,
            ws,
            wfl,
            addr,
            interval,
            rate,
            slice_ms,
        } => wfgen::cmd_stream::run(scenario_dir, ws, wfl, addr, interval, rate, slice_ms).await,
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
