// ---------------------------------------------------------------------------
// warp-fusion unified CLI entry point
// Binary: wfusion
// ---------------------------------------------------------------------------

mod cli_config;
mod error;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use orion_error::report::DiagnosticReport;

use cli_config::{ConfigLoadArgs, run_config_command, run_engine_command};
use error::{CliResult, into_cli_error_from_wfgen, into_cli_error_from_wfl};

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
    /// Start the WarpFusion engine
    Run {
        #[command(flatten)]
        load: ConfigLoadArgs,
        #[arg(long)]
        metrics: bool,
        #[arg(long)]
        metrics_interval: Option<String>,
        #[arg(long)]
        metrics_listen: Option<String>,
    },
    /// Inspect and diff configuration
    Config {
        #[command(subcommand)]
        command: cli_config::ConfigCommands,
    },
    /// Test data generation & validation (wfgen)
    #[command(name = "scenario")]
    Scenario {
        #[command(subcommand)]
        command: ScenarioCommands,
    },
    /// Rule authoring, testing & replay (wfl)
    #[command(name = "rule")]
    Rule {
        #[command(subcommand)]
        command: RuleCommands,
    },
}

// -- Scenario subcommands (wfgen) --------------------------------------------

#[derive(Subcommand)]
enum ScenarioCommands {
    Gen {
        #[arg(long)]
        scenario: PathBuf,
        #[arg(long, default_value = "jsonl")]
        format: String,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        ws: Vec<PathBuf>,
        #[arg(long)]
        wfl: Vec<PathBuf>,
        #[arg(long)]
        no_oracle: bool,
        #[arg(long)]
        send: bool,
        #[arg(long, default_value = "127.0.0.1:9800")]
        addr: String,
    },
    Lint {
        #[arg(long)]
        scenario: PathBuf,
        #[arg(long)]
        ws: Vec<PathBuf>,
        #[arg(long)]
        wfl: Vec<PathBuf>,
    },
    Verify {
        #[arg(long)]
        expected: PathBuf,
        #[arg(long)]
        actual: PathBuf,
        #[arg(long)]
        score_tolerance: Option<f64>,
        #[arg(long)]
        time_tolerance: Option<f64>,
        #[arg(long)]
        meta: Option<PathBuf>,
        #[arg(long, default_value = "json")]
        format: String,
    },
    Send {
        #[arg(long)]
        scenario: PathBuf,
        #[arg(long)]
        input: PathBuf,
        #[arg(long, default_value = "127.0.0.1:9800")]
        addr: String,
        #[arg(long)]
        ws: Vec<PathBuf>,
    },
    Bench {
        #[arg(long)]
        scenario: PathBuf,
        #[arg(long)]
        ws: Vec<PathBuf>,
        #[arg(long)]
        wfl: Vec<PathBuf>,
        #[arg(long)]
        duration: Option<String>,
        #[arg(long)]
        send: bool,
        #[arg(long, default_value = "127.0.0.1:9800")]
        addr: String,
    },
}

// -- Rule subcommands (wfl) --------------------------------------------------

#[derive(Subcommand)]
enum RuleCommands {
    Explain {
        file: PathBuf,
        #[arg(short, long, default_value = "schemas/*.wfs")]
        schemas: Vec<String>,
        #[arg(long)]
        var: Vec<String>,
    },
    Lint {
        file: PathBuf,
        #[arg(short, long, default_value = "schemas/*.wfs")]
        schemas: Vec<String>,
        #[arg(long)]
        var: Vec<String>,
    },
    Fmt {
        files: Vec<PathBuf>,
        #[arg(short, long)]
        write: bool,
        #[arg(long)]
        check: bool,
    },
    Replay {
        file: PathBuf,
        #[arg(short, long, default_value = "schemas/*.wfs")]
        schemas: Vec<String>,
        #[arg(short, long)]
        input: PathBuf,
        #[arg(long)]
        var: Vec<String>,
    },
    #[command(name = "verify")]
    ReplayVerify {
        file: Option<PathBuf>,
        #[arg(long)]
        case: Option<String>,
        #[arg(long, default_value = "data")]
        data_dir: PathBuf,
        #[arg(short, long, default_value = "schemas/*.wfs")]
        schemas: Vec<String>,
        #[arg(short, long)]
        input: Option<PathBuf>,
        #[arg(long)]
        var: Vec<String>,
        #[arg(long)]
        expected: Option<PathBuf>,
        #[arg(long)]
        score_tolerance: Option<f64>,
        #[arg(long)]
        time_tolerance: Option<f64>,
        #[arg(long)]
        meta: Option<PathBuf>,
        #[arg(long, default_value = "markdown")]
        format: String,
    },
    Test {
        file: PathBuf,
        #[arg(short, long, default_value = "schemas/*.wfs")]
        schemas: Vec<String>,
        #[arg(long)]
        var: Vec<String>,
        #[arg(long)]
        shuffle: bool,
        #[arg(long)]
        runs: Option<usize>,
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
        Commands::Run {
            load,
            metrics,
            metrics_interval,
            metrics_listen,
        } => run_engine_command(load, metrics, metrics_interval, metrics_listen).await?,
        Commands::Config { command } => run_config_command(command).await?,
        Commands::Scenario { command } => match command {
            ScenarioCommands::Gen {
                scenario,
                format,
                out,
                ws,
                wfl,
                no_oracle,
                send,
                addr,
            } => {
                wfgen::cmd_gen::run(scenario, format, out, ws, wfl, no_oracle, send, addr)
                    .map_err(into_cli_error_from_wfgen)?;
            }
            ScenarioCommands::Lint { scenario, ws, wfl } => {
                wfgen::cmd_lint::run(scenario, ws, wfl).map_err(into_cli_error_from_wfgen)?;
            }
            ScenarioCommands::Verify {
                expected,
                actual,
                score_tolerance,
                time_tolerance,
                meta,
                format,
            } => {
                wfgen::cmd_verify::run(
                    expected,
                    actual,
                    score_tolerance,
                    time_tolerance,
                    meta,
                    format,
                )
                .map_err(into_cli_error_from_wfgen)?;
            }
            ScenarioCommands::Send {
                scenario,
                input,
                addr,
                ws,
            } => {
                wfgen::cmd_send::run(scenario, input, addr, ws)
                    .map_err(into_cli_error_from_wfgen)?;
            }
            ScenarioCommands::Bench {
                scenario,
                ws,
                wfl,
                duration,
                send,
                addr,
            } => {
                wfgen::cmd_bench::run(scenario, ws, wfl, duration, send, addr)
                    .map_err(into_cli_error_from_wfgen)?;
            }
        },
        Commands::Rule { command } => match command {
            RuleCommands::Explain { file, schemas, var } => {
                wfl::cmd_explain::run(file, schemas, var).map_err(into_cli_error_from_wfl)?;
            }
            RuleCommands::Lint { file, schemas, var } => {
                wfl::cmd_lint::run(file, schemas, var).map_err(into_cli_error_from_wfl)?;
            }
            RuleCommands::Fmt {
                files,
                write,
                check,
            } => {
                wfl::cmd_fmt::run(files, write, check).map_err(into_cli_error_from_wfl)?;
            }
            RuleCommands::Replay {
                file,
                schemas,
                input,
                var,
            } => {
                wfl::cmd_replay::run(file, schemas, input, var).map_err(into_cli_error_from_wfl)?;
            }
            RuleCommands::ReplayVerify {
                file,
                case,
                data_dir,
                schemas,
                input,
                var,
                expected,
                score_tolerance,
                time_tolerance,
                meta,
                format,
            } => {
                wfl::cmd_replay_verify::run(
                    file,
                    case,
                    data_dir,
                    schemas,
                    input,
                    var,
                    expected,
                    score_tolerance,
                    time_tolerance,
                    meta,
                    format,
                )
                .map_err(into_cli_error_from_wfl)?;
            }
            RuleCommands::Test {
                file,
                schemas,
                var,
                shuffle,
                runs,
            } => {
                wfl::cmd_test::run(file, schemas, var, shuffle, runs)
                    .map_err(into_cli_error_from_wfl)?;
            }
        },
    }

    Ok(())
}
