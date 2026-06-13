// ---------------------------------------------------------------------------
// warp-fusion CLI config handling
// Extracted from wf_runtime::cli::mod.rs
// ---------------------------------------------------------------------------

use std::path::PathBuf;
use std::str::FromStr;

use clap::{Args, Subcommand};
use orion_error::conversion::{ConvErr, ConvStructError, SourceErr};

use wf_config::{ConfigVarContext, FusionConfig, FusionConfigLoader, HumanDuration, parse_vars};
use wf_runtime::{
    cli::error::{EngineError, EngineReason, EngineResult},
    error::RuntimeError,
    lifecycle::{Reactor, ShutdownTrigger, wait_for_signal},
    tracing_init::init_tracing,
};

use crate::error::{CliResult, into_cli_error};

// -- CLI argument types ------------------------------------------------------

#[derive(::moju_derive::MoJu, Args, Clone)]
#[moju(
    kind = "struct",
    domain = "Orchestra",
    module = "Orchestra.EngineEntry"
)]
pub struct ConfigLoadArgs {
    #[arg(short, long, default_value = "conf/wfusion.toml")]
    pub config: PathBuf,
    #[arg(long)]
    pub overlay: Vec<PathBuf>,
    #[arg(long)]
    pub var: Vec<String>,
    #[arg(long)]
    pub work_dir: Option<PathBuf>,
}

#[derive(::moju_derive::MoJu, Args, Clone, Default)]
#[moju(
    kind = "struct",
    domain = "Orchestra",
    module = "Orchestra.EngineEntry"
)]
pub struct CompareConfigLoadArgs {
    #[arg(long = "to-config")]
    pub to_config: Option<PathBuf>,
    #[arg(long = "to-overlay")]
    pub to_overlay: Vec<PathBuf>,
    #[arg(long = "to-var")]
    pub to_var: Vec<String>,
    #[arg(long = "to-work-dir")]
    pub to_work_dir: Option<PathBuf>,
}

#[derive(::moju_derive::MoJu, Args, Clone, Default)]
#[moju(
    kind = "struct",
    domain = "Orchestra",
    module = "Orchestra.EngineEntry"
)]
pub struct PathFilterArgs {
    #[arg(long = "path-prefix")]
    pub path_prefix: Vec<String>,
}

#[derive(::moju_derive::MoJu, Args, Clone, Default)]
#[moju(
    kind = "struct",
    domain = "Orchestra",
    module = "Orchestra.EngineEntry"
)]
pub struct VarFilterArgs {
    #[arg(long = "var-prefix")]
    pub var_prefix: Vec<String>,
}

#[derive(::moju_derive::MoJu)]
#[moju(
    kind = "struct",
    domain = "Orchestra",
    module = "Orchestra.EngineEntry"
)]
struct ResolvedConfigLoad {
    config_path: PathBuf,
    overlay_paths: Vec<PathBuf>,
    runtime_base_dir: PathBuf,
    config_ctx: ConfigVarContext,
}

#[derive(::moju_derive::MoJu, Subcommand, Clone)]
#[moju(kind = "state", domain = "Orchestra", module = "Orchestra.EngineEntry")]
pub enum ConfigCommands {
    Render {
        #[command(flatten)]
        load: ConfigLoadArgs,
        #[arg(long)]
        raw: bool,
    },
    Origins {
        #[command(flatten)]
        load: ConfigLoadArgs,
        #[command(flatten)]
        filter: PathFilterArgs,
    },
    Vars {
        #[command(flatten)]
        load: ConfigLoadArgs,
        #[command(flatten)]
        filter: VarFilterArgs,
    },
    Diff {
        #[command(flatten)]
        load: ConfigLoadArgs,
        #[command(flatten)]
        compare: CompareConfigLoadArgs,
        #[command(flatten)]
        filter: PathFilterArgs,
        #[arg(long)]
        expanded: bool,
    },
}

// -- Config resolution (uses EngineResult internally) ------------------------

fn resolve_config_load(load: ConfigLoadArgs) -> EngineResult<ResolvedConfigLoad> {
    resolve_config_load_parts(load.config, load.overlay, load.var, load.work_dir)
}

fn resolve_config_load_parts(
    config: PathBuf,
    overlay: Vec<PathBuf>,
    var: Vec<String>,
    work_dir: Option<PathBuf>,
) -> EngineResult<ResolvedConfigLoad> {
    let config_path = config.canonicalize().source_err(
        EngineReason::Cli,
        format!("config path '{}'", config.display()),
    )?;
    let overlay_paths: Vec<PathBuf> = overlay
        .into_iter()
        .map(|path| {
            path.canonicalize().source_err(
                EngineReason::Cli,
                format!("overlay path '{}'", path.display()),
            )
        })
        .collect::<Result<_, _>>()?;
    let default_base_dir = config_path
        .parent()
        .expect("config path must have a parent directory");
    let runtime_base_dir = if let Some(work_dir) = work_dir {
        let path = work_dir.canonicalize().source_err(
            EngineReason::Cli,
            format!("work-dir path '{}'", work_dir.display()),
        )?;
        if !path.is_dir() {
            return EngineReason::Cli.fail(format!(
                "work-dir path '{}' is not a directory",
                path.display()
            ));
        }
        path
    } else {
        default_base_dir.to_path_buf()
    };
    let cli_vars = parse_vars(&var).conv_err()?;
    let config_ctx = ConfigVarContext::from_explicit_vars(cli_vars);

    Ok(ResolvedConfigLoad {
        config_path,
        overlay_paths,
        runtime_base_dir,
        config_ctx,
    })
}

fn resolve_compare_config_load(
    base: &ResolvedConfigLoad,
    compare: CompareConfigLoadArgs,
) -> EngineResult<ResolvedConfigLoad> {
    resolve_config_load_parts(
        compare
            .to_config
            .unwrap_or_else(|| base.config_path.clone()),
        compare.to_overlay,
        compare.to_var,
        compare.to_work_dir,
    )
}

// -- Prefix matching helpers -------------------------------------------------

fn format_value<T: std::fmt::Display>(value: &T) -> String {
    value.to_string()
}

fn matches_any_prefix(path: &str, prefixes: &[String]) -> bool {
    prefixes.is_empty()
        || prefixes
            .iter()
            .any(|prefix| path_matches_prefix(path, prefix))
}

fn path_matches_prefix(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('.') || rest.starts_with('['))
}

fn matches_any_var_prefix(key: &str, prefixes: &[String]) -> bool {
    prefixes.is_empty() || prefixes.iter().any(|prefix| key.starts_with(prefix))
}

fn render_runtime_error(err: RuntimeError) -> EngineError {
    err.conv()
}

// -- Public command handlers (convert EngineError -> CliError) ---------------

pub async fn run_config_command(command: ConfigCommands) -> CliResult<()> {
    run_config_inner(command).await.map_err(into_cli_error)
}

async fn run_config_inner(command: ConfigCommands) -> EngineResult<()> {
    match command {
        ConfigCommands::Render { load, raw } => {
            let resolved = resolve_config_load(load)?;
            let loader = FusionConfigLoader::new(
                &resolved.config_path,
                &resolved.overlay_paths,
                &resolved.config_ctx,
                Some(&resolved.runtime_base_dir),
            );
            let rendered = if raw {
                loader.load_merged_toml().conv_err()?
            } else {
                loader.load_expanded_toml().conv_err()?
            };
            print!("{rendered}");
        }
        ConfigCommands::Origins { load, filter } => {
            let resolved = resolve_config_load(load)?;
            let raw = FusionConfigLoader::new(
                &resolved.config_path,
                &resolved.overlay_paths,
                &resolved.config_ctx,
                Some(&resolved.runtime_base_dir),
            )
            .load_raw()
            .conv_err()?;
            let mut matched = 0usize;
            for (path, origin) in raw.origin_entries() {
                if !matches_any_prefix(&path, &filter.path_prefix) {
                    continue;
                }
                matched += 1;
                println!("{path}\t{}", origin.display());
            }
            if matched == 0 {
                println!("no matching paths");
            }
        }
        ConfigCommands::Vars { load, filter } => {
            let resolved = resolve_config_load(load)?;
            let vars = FusionConfigLoader::new(
                &resolved.config_path,
                &resolved.overlay_paths,
                &resolved.config_ctx,
                Some(&resolved.runtime_base_dir),
            )
            .load_effective_vars()
            .conv_err()?;
            let mut matched = 0usize;
            for entry in vars {
                if !matches_any_var_prefix(&entry.key, &filter.var_prefix) {
                    continue;
                }
                matched += 1;
                println!("{}\t{}\t{}", entry.key, entry.value, entry.source);
            }
            if matched == 0 {
                println!("no matching vars");
            }
        }
        ConfigCommands::Diff {
            load,
            compare,
            filter,
            expanded,
        } => {
            let resolved = resolve_config_load(load)?;
            let compare_resolved = resolve_compare_config_load(&resolved, compare)?;
            let left_loader = FusionConfigLoader::new(
                &resolved.config_path,
                &resolved.overlay_paths,
                &resolved.config_ctx,
                Some(&resolved.runtime_base_dir),
            );
            let right_loader = FusionConfigLoader::new(
                &compare_resolved.config_path,
                &compare_resolved.overlay_paths,
                &compare_resolved.config_ctx,
                Some(&compare_resolved.runtime_base_dir),
            );
            let left = if expanded {
                left_loader.load_expanded_raw().conv_err()?
            } else {
                left_loader.load_raw().conv_err()?
            };
            let right = if expanded {
                right_loader.load_expanded_raw().conv_err()?
            } else {
                right_loader.load_raw().conv_err()?
            };

            let changes: Vec<_> = left
                .diff(&right)
                .into_iter()
                .filter(|change| matches_any_prefix(&change.path, &filter.path_prefix))
                .collect();
            if changes.is_empty() {
                println!("no changes");
                return Ok(());
            }

            for change in changes {
                println!("path: {}", change.path);
                println!(
                    "  old: {}",
                    change
                        .old_value
                        .as_ref()
                        .map(format_value)
                        .unwrap_or_else(|| "<none>".to_string())
                );
                println!(
                    "  new: {}",
                    change
                        .new_value
                        .as_ref()
                        .map(format_value)
                        .unwrap_or_else(|| "<none>".to_string())
                );
                println!(
                    "  old_origin: {}",
                    change
                        .old_origin
                        .as_deref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "<none>".to_string())
                );
                println!(
                    "  new_origin: {}",
                    change
                        .new_origin
                        .as_deref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "<none>".to_string())
                );
            }
        }
    }
    Ok(())
}

pub async fn run_engine_command(
    load: ConfigLoadArgs,
    metrics: bool,
    metrics_interval: Option<String>,
    metrics_listen: Option<String>,
) -> CliResult<()> {
    run_engine_inner(load, metrics, metrics_interval, metrics_listen)
        .await
        .map_err(into_cli_error)
}

async fn run_engine_inner(
    load: ConfigLoadArgs,
    metrics: bool,
    metrics_interval: Option<String>,
    metrics_listen: Option<String>,
) -> EngineResult<()> {
    let resolved = resolve_config_load(load)?;
    let mut fusion_config = FusionConfig::load_with_overlays(
        &resolved.config_path,
        &resolved.overlay_paths,
        &resolved.config_ctx,
        Some(&resolved.runtime_base_dir),
    )
    .conv_err()?;
    if metrics || metrics_interval.is_some() || metrics_listen.is_some() {
        fusion_config.metrics.enabled = true;
    }
    if let Some(interval) = metrics_interval {
        fusion_config.metrics.report_interval = HumanDuration::from_str(&interval).conv_err()?;
    }
    if let Some(listen) = metrics_listen {
        fusion_config.metrics.prometheus_listen = listen;
    }
    let metrics_enabled = fusion_config.metrics.enabled;
    let metrics_interval = fusion_config.metrics.report_interval;
    let metrics_listen = fusion_config.metrics.prometheus_listen.clone();

    let _guard = init_tracing(&fusion_config.logging, &resolved.runtime_base_dir).conv_err()?;

    let reactor = match Reactor::start(fusion_config, &resolved.runtime_base_dir).await {
        Ok(reactor) => reactor,
        Err(err) => return Err(render_runtime_error(err)),
    };
    if let Some(listen_addr) = reactor.listen_addr() {
        tracing::info!(domain = "sys", listen = %listen_addr, "WarpFusion reactor started");
    } else {
        tracing::info!(
            domain = "sys",
            "WarpFusion reactor started without tcp listener"
        );
    }
    if metrics_enabled {
        tracing::info!(
            domain = "res",
            interval = %metrics_interval,
            listen = %metrics_listen,
            "runtime metrics enabled"
        );
    }

    if wait_for_signal(reactor.cancel_token()).await == ShutdownTrigger::Signal {
        reactor.shutdown();
    }
    if let Err(err) = reactor.wait().await {
        return Err(render_runtime_error(err));
    }
    Ok(())
}
