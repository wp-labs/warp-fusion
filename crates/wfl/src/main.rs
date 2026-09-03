use std::path::PathBuf;

use clap::{Parser, Subcommand};

use wfl::error::WflResult;

#[derive(Parser)]
#[command(name = "wfl", about = "WarpFusion project tools for rule developers")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Explain compiled rules in human-readable form
    Explain {
        /// Path to the .wfl rule file
        file: PathBuf,

        /// Schema file glob patterns (e.g. "schemas/*.wfs")
        #[arg(short, long, default_value = "schemas/*.wfs")]
        schemas: Vec<String>,

        /// Variable substitutions in KEY=VALUE format
        #[arg(long)]
        var: Vec<String>,
    },

    /// Run lint checks on a .wfl rule file
    Lint {
        /// Path to the .wfl rule file
        file: PathBuf,

        /// Schema file glob patterns (e.g. "schemas/*.wfs")
        #[arg(short, long, default_value = "schemas/*.wfs")]
        schemas: Vec<String>,

        /// Variable substitutions in KEY=VALUE format
        #[arg(long)]
        var: Vec<String>,
    },

    /// Format .wfl rule files
    Fmt {
        /// Input .wfl files to format
        files: Vec<PathBuf>,

        /// Write formatted output back to the files (in-place)
        #[arg(short, long)]
        write: bool,

        /// Check if files are already formatted (exit 1 if not)
        #[arg(long)]
        check: bool,
    },

    /// Replay NDJSON data through a compiled rule for offline debugging
    Replay {
        /// Path to the .wfl rule file
        file: PathBuf,

        /// Schema file glob patterns (e.g. "schemas/*.wfs")
        #[arg(short, long, default_value = "schemas/*.wfs")]
        schemas: Vec<String>,

        /// NDJSON input data file
        #[arg(short, long)]
        input: PathBuf,

        /// Variable substitutions in KEY=VALUE format
        #[arg(long)]
        var: Vec<String>,
    },

    /// Replay NDJSON and verify against oracle expected alerts in one step
    #[command(name = "verify")]
    ReplayVerify {
        /// Path to the .wfl rule file (optional when --case can auto-resolve)
        file: Option<PathBuf>,

        /// Case name to auto-resolve paths from data dir:
        /// data/<case>.jsonl and data/<case>.except.jsonl
        #[arg(long)]
        case: Option<String>,

        /// Base directory used with --case (default: data)
        #[arg(long, default_value = "data")]
        data_dir: PathBuf,

        /// Schema file glob patterns (e.g. "schemas/*.wfs")
        #[arg(short, long, default_value = "schemas/*.wfs")]
        schemas: Vec<String>,

        /// NDJSON input data file
        #[arg(short, long)]
        input: Option<PathBuf>,

        /// Variable substitutions in KEY=VALUE format
        #[arg(long)]
        var: Vec<String>,

        /// Path to oracle (expected) JSONL file
        #[arg(long)]
        expected: Option<PathBuf>,

        /// Score tolerance for matching (overrides meta if set)
        #[arg(long)]
        score_tolerance: Option<f64>,

        /// Time tolerance for matching in seconds (overrides meta if set)
        #[arg(long)]
        time_tolerance: Option<f64>,

        /// Path to oracle meta JSON with tolerances
        #[arg(long)]
        meta: Option<PathBuf>,

        /// Output format: "json" or "markdown" (default: markdown)
        #[arg(long, default_value = "markdown")]
        format: String,
    },

    /// Run contract tests against compiled rules
    Test {
        /// Path to the .wfl rule file (must contain contract blocks)
        file: PathBuf,

        /// Schema file glob patterns (e.g. "schemas/*.wfs")
        #[arg(short, long, default_value = "schemas/*.wfs")]
        schemas: Vec<String>,

        /// Variable substitutions in KEY=VALUE format
        #[arg(long)]
        var: Vec<String>,

        /// Enable shuffle permutation mode for all tests in this run
        #[arg(long)]
        shuffle: bool,

        /// Number of runs for conformance testing (requires > 0)
        #[arg(long)]
        runs: Option<usize>,

        /// Generate bind-guard negative cases (L2): for each test row that
        /// hits a simple `field == literal` / `field != literal` bind guard,
        /// append a violating row and assert hits stay unchanged.
        #[arg(long)]
        gen_negatives: bool,

        /// Output format: "human" (default) or "json"
        #[arg(long, default_value = "human")]
        format: String,
    },

    /// Run detection-intent samples (.wfi) against compiled rules (L3)
    ///
    /// 意图文件 = 纯 test 块集合：expect { hits >= 1 } 是正样本（漏报检查：
    /// 该检出），expect { hits == 0 } 是负样本（误报检查：不该检出）。
    #[command(name = "intent")]
    Intent {
        /// Path to the .wfl rule file (the rules under test)
        file: PathBuf,

        /// Path to the .wfi intent samples file (test-block-only subset of .wfl)
        #[arg(long)]
        intent: PathBuf,

        /// Schema file glob patterns (e.g. "schemas/*.wfs")
        #[arg(short, long, default_value = "schemas/*.wfs")]
        schemas: Vec<String>,

        /// Variable substitutions in KEY=VALUE format
        #[arg(long)]
        var: Vec<String>,

        /// Output format: "human" (default) or "json"
        #[arg(long, default_value = "human")]
        format: String,
    },
}

fn main() {
    if let Err(err) = run_cli() {
        eprintln!("{}", err.report().render());
        std::process::exit(1);
    }
}

fn run_cli() -> WflResult<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Explain { file, schemas, var } => {
            wfl::cmd_explain::run(file, schemas, var)?;
        }

        Commands::Lint { file, schemas, var } => {
            wfl::cmd_lint::run(file, schemas, var)?;
        }

        Commands::Fmt {
            files,
            write,
            check,
        } => {
            wfl::cmd_fmt::run(files, write, check)?;
        }

        Commands::Replay {
            file,
            schemas,
            input,
            var,
        } => {
            wfl::cmd_replay::run(file, schemas, input, var)?;
        }

        Commands::ReplayVerify {
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
            )?;
        }

        Commands::Test {
            file,
            schemas,
            var,
            shuffle,
            runs,
            gen_negatives,
            format,
        } => {
            wfl::cmd_test::run(file, schemas, var, shuffle, runs, gen_negatives, format)?;
        }

        Commands::Intent {
            file,
            intent,
            schemas,
            var,
            format,
        } => {
            wfl::cmd_intent::run(file, intent, schemas, var, format)?;
        }
    }

    Ok(())
}
