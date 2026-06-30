// ---------------------------------------------------------------------------
// warp-fusion CLI config handling
// Extracted from wf_runtime::cli::mod.rs
// ---------------------------------------------------------------------------

use std::path::PathBuf;
use std::str::FromStr;

use clap::Args;
use orion_error::conversion::{ConvErr, ConvStructError, SourceErr, ToStructError};

use wf_config::{ConfigVarContext, FusionConfig, FusionMode, HumanDuration, parse_vars};
use wf_runtime::{
    cli::error::{EngineError, EngineReason, EngineResult},
    error::{RuntimeError, RuntimeReason},
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

// -- Config resolution (uses EngineResult internally) ------------------------

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

fn render_runtime_error(err: RuntimeError) -> EngineError {
    err.conv()
}

// -- Engine command handlers ------------------------------------------------

pub async fn run_engine_command(
    load: ConfigLoadArgs,
    mode: Option<FusionMode>,
    metrics: bool,
    metrics_interval: Option<String>,
    metrics_listen: Option<String>,
) -> CliResult<()> {
    run_engine_inner(load, mode, metrics, metrics_interval, metrics_listen)
        .await
        .map_err(into_cli_error)
}

async fn run_engine_inner(
    load: ConfigLoadArgs,
    mode: Option<FusionMode>,
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
    // Override mode from CLI command (daemon/batch always explicit)
    if let Some(m) = mode {
        fusion_config.mode = m;
    }
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

    // Register external connector factories (mirrors warp-parse feats.rs pattern)
    crate::register::register_connectors();

    // Extract admin_api config before fusion_config is moved into Reactor
    let admin_api_config = fusion_config.admin_api.clone();

    let reactor = match Reactor::start(fusion_config, &resolved.runtime_base_dir).await {
        Ok(reactor) => reactor,
        Err(err) => return Err(render_runtime_error(err)),
    };
    tracing::info!(domain = "sys", "WarpFusion reactor started");

    // Start admin API if enabled
    let _admin_api = crate::admin_api::start_if_enabled(
        &resolved.runtime_base_dir,
        &admin_api_config,
        reactor.cancel_token(),
    )
    .await
    .map_err(|e| render_runtime_error(RuntimeReason::core_conf().to_err().with_detail(e)))?;
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
