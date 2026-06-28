//! wfadm config — diff wfusion configuration

use std::path::{Path, PathBuf};

use clap::Subcommand;
use wf_config::{ConfigVarContext, FusionConfigLoader, parse_vars};

// ── CLI subcommand ────────────────────────────────────────────────────

#[derive(Subcommand, Clone)]
pub enum ConfigCommands {
    /// Diff two configuration sets
    Diff {
        #[arg(short, long, default_value = "conf/wfusion.toml")]
        config: PathBuf,
        #[arg(long)]
        overlay: Vec<PathBuf>,
        #[arg(long)]
        var: Vec<String>,
        #[arg(long)]
        work_dir: Option<PathBuf>,
        #[arg(long = "to-config")]
        to_config: Option<PathBuf>,
        #[arg(long = "to-overlay")]
        to_overlay: Vec<PathBuf>,
        #[arg(long = "to-var")]
        to_var: Vec<String>,
        #[arg(long = "to-work-dir")]
        to_work_dir: Option<PathBuf>,
        #[arg(long = "path-prefix")]
        path_prefix: Vec<String>,
        #[arg(long)]
        expanded: bool,
    },
}

// ── Runner ────────────────────────────────────────────────────────────

pub fn run(command: ConfigCommands) -> Result<(), String> {
    match command {
        ConfigCommands::Diff {
            config,
            overlay,
            var,
            work_dir,
            to_config,
            to_overlay,
            to_var,
            to_work_dir,
            path_prefix,
            expanded,
        } => cmd_diff(
            &config,
            &overlay,
            &var,
            work_dir.as_deref(),
            to_config.as_deref(),
            &to_overlay,
            &to_var,
            to_work_dir.as_deref(),
            &path_prefix,
            expanded,
        ),
    }
}

// ── Helpers ───────────────────────────────────────────────────────────

struct LoadCtx {
    config_path: PathBuf,
    overlay_paths: Vec<PathBuf>,
    config_ctx: ConfigVarContext,
    base_dir: PathBuf,
}

fn resolve_load(
    config: &Path,
    overlays: &[PathBuf],
    vars: &[String],
    work_dir: Option<&Path>,
) -> Result<LoadCtx, String> {
    let config_path = config
        .canonicalize()
        .map_err(|e| format!("config path '{}': {e}", config.display()))?;
    let overlay_paths: Vec<PathBuf> = overlays
        .iter()
        .map(|p| {
            p.canonicalize()
                .map_err(|e| format!("overlay path '{}': {e}", p.display()))
        })
        .collect::<Result<_, _>>()?;
    let base_dir = if let Some(wd) = work_dir {
        let path = wd
            .canonicalize()
            .map_err(|e| format!("work-dir '{}': {e}", wd.display()))?;
        if !path.is_dir() {
            return Err(format!("work-dir '{}' is not a directory", path.display()));
        }
        path
    } else {
        config_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    };
    let cli_vars = parse_vars(vars).map_err(|e| format!("parse vars: {e}"))?;
    let config_ctx = ConfigVarContext::from_explicit_vars(cli_vars);
    Ok(LoadCtx {
        config_path,
        overlay_paths,
        config_ctx,
        base_dir,
    })
}

// ── Diff ──────────────────────────────────────────────────────────────

fn cmd_diff(
    config: &Path,
    overlays: &[PathBuf],
    vars: &[String],
    work_dir: Option<&Path>,
    to_config: Option<&Path>,
    to_overlays: &[PathBuf],
    to_vars: &[String],
    to_work_dir: Option<&Path>,
    path_prefix: &[String],
    expanded: bool,
) -> Result<(), String> {
    let ctx = resolve_load(config, overlays, vars, work_dir)?;
    let cmp_config = to_config.unwrap_or(&ctx.config_path);
    let cmp_ctx = resolve_load(cmp_config, to_overlays, to_vars, to_work_dir)?;

    let l = FusionConfigLoader::new(
        &ctx.config_path,
        &ctx.overlay_paths,
        &ctx.config_ctx,
        Some(&ctx.base_dir),
    );
    let r = FusionConfigLoader::new(
        &cmp_ctx.config_path,
        &cmp_ctx.overlay_paths,
        &cmp_ctx.config_ctx,
        Some(&cmp_ctx.base_dir),
    );

    let left = if expanded {
        l.load_expanded_raw().map_err(|e| format!("{e}"))?
    } else {
        l.load_raw().map_err(|e| format!("{e}"))?
    };
    let right = if expanded {
        r.load_expanded_raw().map_err(|e| format!("{e}"))?
    } else {
        r.load_raw().map_err(|e| format!("{e}"))?
    };

    let changes: Vec<_> = left
        .diff(&right)
        .into_iter()
        .filter(|c| matches_any_prefix(&c.path, path_prefix))
        .collect();

    if changes.is_empty() {
        println!("(no changes)");
        return Ok(());
    }
    for c in &changes {
        println!("path: {}", c.path);
        println!(
            "  old: {}",
            c.old_value
                .as_ref()
                .map(|v| v.to_string())
                .unwrap_or_else(|| "<none>".to_string())
        );
        println!(
            "  new: {}",
            c.new_value
                .as_ref()
                .map(|v| v.to_string())
                .unwrap_or_else(|| "<none>".to_string())
        );
        println!(
            "  old_origin: {}",
            c.old_origin
                .as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<none>".to_string())
        );
        println!(
            "  new_origin: {}",
            c.new_origin
                .as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<none>".to_string())
        );
    }
    Ok(())
}

fn matches_any_prefix(path: &str, prefixes: &[String]) -> bool {
    prefixes.is_empty() || prefixes.iter().any(|p| path_matches_prefix(path, p))
}

fn path_matches_prefix(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('.') || rest.starts_with('['))
}
