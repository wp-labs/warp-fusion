mod check;
mod connectors;
pub(crate) mod init_tpl;
mod init;
mod self_update;
mod sink;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "wfadm",
    version,
    about = "WarpFusion admin CLI — project management for wf-rules",
    propagate_version = true
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new wf-rules project
    #[command(disable_version_flag = true)]
    Init {
        /// Project name
        #[arg(long)]
        name: Option<String>,
        /// Project directory
        #[arg(long)]
        dir: Option<String>,
        /// Init mode: full, normal, rules, conf (default: normal)
        #[arg(long, conflicts_with = "repo")]
        mode: Option<String>,
        /// Remote project repo URL; enables first-time remote bootstrap
        #[arg(long, conflicts_with = "mode")]
        repo: Option<String>,
        /// Target version for remote bootstrap
        #[arg(long, requires = "repo")]
        version: Option<String>,
    },
    /// Check project integrity
    Check,
    /// Validate sink configuration
    Sink,
    /// Self-update binary
    #[command(name = "self-update")]
    SelfUpdate,
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Init {
            name,
            dir,
            mode,
            repo,
            version,
        } => cmd_init(name, dir, mode, repo, version),
        Commands::Check => check::run(),
        Commands::Sink => sink::run(),
        Commands::SelfUpdate => self_update::run(),
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

fn cmd_init(
    name: Option<String>,
    dir: Option<String>,
    mode: Option<String>,
    repo: Option<String>,
    version: Option<String>,
) -> Result<(), String> {
    let project_dir = dir.unwrap_or_else(|| ".".to_string());
    let project_name = name.unwrap_or_else(|| {
        std::path::Path::new(&project_dir)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "wf-rules".to_string())
    });

    if let Some(remote) = repo {
        return init::init_from_remote(&project_dir, &remote, version.as_deref());
    }

    let scope = mode.as_deref().unwrap_or("normal");
    init::init_project(&project_dir, &project_name, scope)
}
