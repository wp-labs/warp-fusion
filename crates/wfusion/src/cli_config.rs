// ---------------------------------------------------------------------------
// warp-fusion CLI config handling
// Extracted from wf_runtime::cli::mod.rs
// ---------------------------------------------------------------------------

use std::path::PathBuf;
use std::str::FromStr;

use clap::Args;
use orion_error::conversion::{ConvErr, ConvStructError, SourceErr, ToStructError};

use wf_config::{ConfigVarContext, FusionConfigLoader, FusionMode, HumanDuration, parse_vars};
use wf_runtime::{
    cli::error::{EngineError, EngineReason, EngineResult},
    error::{RuntimeError, RuntimeReason},
    lifecycle::{RESTART_EXIT_CODE, Reactor, RunOutcome},
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
    let default_base_dir = default_runtime_base_dir(&config_path);
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

fn default_runtime_base_dir(config_path: &std::path::Path) -> &std::path::Path {
    let config_dir = config_path
        .parent()
        .expect("config path must have a parent directory");
    if config_path.file_name().and_then(|n| n.to_str()) == Some("wfusion.toml")
        && config_dir.file_name().and_then(|n| n.to_str()) == Some("conf")
        && let Some(project_dir) = config_dir.parent()
    {
        return project_dir;
    }
    config_dir
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
    // Build a loader once so we can obtain both the raw config tree (the reload
    // baseline) and the effective config.
    let loader = FusionConfigLoader::new(
        &resolved.config_path,
        &resolved.overlay_paths,
        &resolved.config_ctx,
        Some(&resolved.runtime_base_dir),
    );
    let mut fusion_config = loader.load().conv_err()?;
    // Remember whether any CLI metrics flag was passed (before the local
    // `metrics_interval`/`metrics_listen` names get shadowed by config values).
    let has_metrics_cli_override =
        metrics || metrics_interval.is_some() || metrics_listen.is_some();
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
    // Snapshot the post-override metrics config for reload re-application.
    // (Computed before `metrics_listen`/`metrics_interval` are shadowed above.)
    let metrics_cli_override = if has_metrics_cli_override {
        Some(fusion_config.metrics.clone())
    } else {
        None
    };

    let _guard = init_tracing(&fusion_config.logging, &resolved.runtime_base_dir).conv_err()?;

    // Register external connector factories (mirrors warp-parse feats.rs pattern)
    crate::register::register_connectors();

    // Extract admin_api config before fusion_config is moved into Reactor
    let admin_api_config = fusion_config.admin_api.clone();

    // Build the raw config tree alongside the effective config so the Reactor
    // has a reload baseline to diff against.
    let raw = loader.load_raw().conv_err()?;
    let reactor = match Reactor::start(fusion_config, raw, &resolved.runtime_base_dir).await {
        Ok(reactor) => reactor,
        Err(err) => return Err(render_runtime_error(err)),
    };
    tracing::info!(domain = "sys", "WarpFusion reactor started");

    // Hand the admin API a control handle (reload + status) instead of a bare
    // cancel token. Capture the exact config source so reloads re-read the same
    // `--config`/`--overlay`/`--var` the engine booted with (not a guessed
    // `wfusion.toml`).
    let control = reactor.control_handle();
    let mut config_source = crate::admin_api::ReloadConfigSource::new(
        resolved.config_path.clone(),
        resolved.overlay_paths.clone(),
        resolved.config_ctx.clone(),
        resolved.runtime_base_dir.clone(),
    );
    // Carry the CLI overrides so reloads re-apply them to the freshly-loaded
    // config (otherwise `--mode`/`--metrics*` would look like a change → 409).
    if let Some(mode) = mode {
        config_source = config_source.with_mode_override(mode);
    }
    if let Some(metrics_override) = metrics_cli_override {
        config_source = config_source.with_metrics_override(metrics_override);
    }
    let _admin_api = crate::admin_api::start_if_enabled(
        &resolved.runtime_base_dir,
        &admin_api_config,
        control,
        config_source,
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

    // Drive the reactor (signal handling + reload control loop + graceful
    // shutdown) until it exits.
    match reactor.run().await {
        Ok(RunOutcome::RestartRequested) => {
            tracing::info!(
                domain = "sys",
                "restart requested — exiting with code {}",
                RESTART_EXIT_CODE
            );
            std::process::exit(RESTART_EXIT_CODE);
        }
        Ok(RunOutcome::Normal) => {}
        Err(err) => return Err(render_runtime_error(err)),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::default_runtime_base_dir;
    use std::path::Path;

    #[test]
    fn default_runtime_base_dir_uses_project_root_for_conf_wfusion() {
        let config = Path::new("/tmp/project/conf/wfusion.toml");
        assert_eq!(default_runtime_base_dir(config), Path::new("/tmp/project"));
    }

    #[test]
    fn default_runtime_base_dir_keeps_config_dir_for_plain_wfusion() {
        let config = Path::new("/tmp/project/examples/case/wfusion.toml");
        assert_eq!(
            default_runtime_base_dir(config),
            Path::new("/tmp/project/examples/case")
        );
    }
}
