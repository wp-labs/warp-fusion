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

        /// Skip the WFL pipeline entirely: no rule loading, no
        /// `_global.wfl` / yield-preset evaluation, no compilation, no
        /// injection-aware event generation, and no oracle/expected output.
        /// Generation falls back to baseline background events. Equivalent to
        /// `--no-oracle`.
        #[arg(long)]
        no_wfl: bool,

        /// Skip the WFL pipeline and oracle/expected output: only scenario
        /// events are generated. No WFL compilation, no `_global.wfl` /
        /// yield-preset evaluation, and no oracle sidecar files
        /// (`.except.jsonl` / `.except.meta.jsonl`). Equivalent to `--no-wfl`.
        #[arg(long)]
        no_oracle: bool,

        /// Send generated events to wfusion over TCP + Arrow IPC
        #[arg(long)]
        send: bool,

        /// Runtime TCP address used with --send, e.g. 127.0.0.1:9800
        #[arg(long, default_value = "127.0.0.1:9800")]
        addr: String,
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

        /// Path to generated events JSONL file (from `wfgen gen`)
        #[arg(long)]
        input: PathBuf,

        /// Runtime TCP address, e.g. 127.0.0.1:9800
        #[arg(long, default_value = "127.0.0.1:9800")]
        addr: String,

        /// Additional .wfs schema files (beyond those in `use` declarations)
        #[arg(long)]
        ws: Vec<PathBuf>,
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

        /// Sleep (ms) between generate batches — controls event rate
        #[arg(long, default_value = "100")]
        rate_sleep: u64,
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
        } => wfgen::cmd_send::run(scenario, input, addr, ws).await,
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
            rate_sleep,
        } => wfgen::cmd_stream::run(scenario_dir, ws, wfl, addr, interval, rate_sleep).await,
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
