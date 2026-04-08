use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod cmd_bench;
mod cmd_gen;
mod cmd_helpers;
mod cmd_lint;
mod cmd_send;
mod cmd_verify;
mod tcp_send;

#[derive(Parser)]
#[command(name = "wfgen", about = "WarpFusion test data generator")]
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

        /// Output directory
        #[arg(long)]
        out: PathBuf,

        /// Additional .wfs schema files (beyond those in `use` declarations)
        #[arg(long)]
        ws: Vec<PathBuf>,

        /// Additional .wfl rule files (beyond those in `use` declarations)
        #[arg(long)]
        wfl: Vec<PathBuf>,

        /// Disable oracle generation even if the .wfg has an oracle block
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
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Gen {
            scenario,
            format,
            out,
            ws,
            wfl,
            no_oracle,
            send,
            addr,
        } => cmd_gen::run(scenario, format, out, ws, wfl, no_oracle, send, addr),
        Commands::Lint { scenario, ws, wfl } => cmd_lint::run(scenario, ws, wfl),
        Commands::Verify {
            expected,
            actual,
            score_tolerance,
            time_tolerance,
            meta,
            format,
        } => cmd_verify::run(
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
        } => cmd_send::run(scenario, input, addr, ws),
        Commands::Bench {
            scenario,
            ws,
            wfl,
            duration,
            send,
            addr,
        } => cmd_bench::run(scenario, ws, wfl, duration, send, addr),
    }
}
