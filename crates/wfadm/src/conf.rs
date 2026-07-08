//! `wfadm conf` — config diff + remote rule-source version sync.
//!
//! * `conf diff` — compare two wfusion configuration sets.
//! * `conf update` — sync managed dirs from remote git (mirrors wparse
//!   `wproj conf update`). See `docs/design/project_remote_alignment.md`.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use wf_config::{ConfigVarContext, FusionConfig, FusionConfigLoader, parse_vars};
use wf_project_remote::{self, RemoteGroup};

const CONF_REL_PATH: &str = "conf/wfusion.toml";

#[derive(Subcommand, Debug)]
pub enum ConfCmd {
    /// Diff two wfusion configuration sets
    Diff(ConfDiffArgs),
    /// Update managed dirs (models/conf/topology/connectors) from the remote
    /// git repo configured in `[project_remote]`, at a given version tag.
    #[command(visible_alias = "更新", disable_version_flag = true)]
    Update(ConfUpdateArgs),
}

#[derive(Args, Debug, Clone)]
pub struct ConfDiffArgs {
    #[arg(short, long, default_value = "conf/wfusion.toml")]
    pub config: PathBuf,
    #[arg(long)]
    pub overlay: Vec<PathBuf>,
    #[arg(long)]
    pub var: Vec<String>,
    #[arg(long)]
    pub work_dir: Option<PathBuf>,
    #[arg(long = "to-config")]
    pub to_config: Option<PathBuf>,
    #[arg(long = "to-overlay")]
    pub to_overlay: Vec<PathBuf>,
    #[arg(long = "to-var")]
    pub to_var: Vec<String>,
    #[arg(long = "to-work-dir")]
    pub to_work_dir: Option<PathBuf>,
    #[arg(long = "path-prefix")]
    pub path_prefix: Vec<String>,
    #[arg(long)]
    pub expanded: bool,
}

#[derive(Args, Debug, Clone)]
pub struct ConfUpdateArgs {
    /// Work directory (default: current directory)
    #[arg(short, long, default_value = ".")]
    pub work_root: String,

    /// Target version for this update (default: auto-resolve)
    #[arg(long)]
    pub version: Option<String>,

    /// Target group for dual-repo mode (models | infra)
    #[arg(long, value_parser = ["models", "infra"])]
    pub group: Option<String>,

    /// Emit JSON output
    #[arg(long, default_value_t = false)]
    pub json: bool,
}

pub fn run(command: ConfCmd) -> Result<(), String> {
    match command {
        ConfCmd::Diff(args) => run_conf_diff(args),
        ConfCmd::Update(args) => run_conf_update(args),
    }
}

fn run_conf_update(args: ConfUpdateArgs) -> Result<(), String> {
    let work_root = resolve_work_root(&args.work_root)?;
    let group = parse_group(args.group.as_deref())?;

    // Load config (env-expanded) and extract [project_remote].
    let remote_conf = load_project_remote_conf(&work_root)?;
    let version = args.version.as_deref();
    let json = args.json;

    run_conf_update_with_sync(
        &work_root,
        version,
        json,
        group,
        |wr, ver, _group| match group {
            Some(g) => wf_project_remote::sync_project_remote_group(wr, g, &remote_conf, ver),
            None => wf_project_remote::sync_project_remote(wr, &remote_conf, ver),
        },
    )
}

/// `init --repo` bootstrap: sync managed dirs from an explicit repo URL
/// (not from `[project_remote]` config), then validate + rollback like a
/// normal update. Mirrors wparse `run_conf_update_from_repo`.
pub fn run_conf_update_from_repo(
    work_root: &Path,
    repo_url: &str,
    requested_version: Option<&str>,
) -> Result<(), String> {
    let work_root = resolve_work_root(&work_root.to_string_lossy())?;
    tracing::info!(
        domain = "sys",
        "wfadm init --repo bootstrap work_root={} requested_version={} repo={}",
        work_root.display(),
        requested_version.unwrap_or("(auto)"),
        repo_url
    );
    run_conf_update_with_sync(
        &work_root,
        requested_version,
        false,
        None,
        |wr, ver, _group| wf_project_remote::sync_project_remote_from_repo(wr, repo_url, ver),
    )
}

/// CLI wrapper around `wf_project_remote::run_remote_update`: runs the full
/// safe sync (lock → snapshot → sync → validate → rollback) and prints the
/// result. `sync_fn` decides how managed dirs are synced (from
/// `[project_remote]` config or from an explicit `--repo` URL).
fn run_conf_update_with_sync<F>(
    work_root: &Path,
    requested_version: Option<&str>,
    json: bool,
    group: Option<RemoteGroup>,
    sync_fn: F,
) -> Result<(), String>
where
    F: FnOnce(
        &Path,
        Option<&str>,
        Option<RemoteGroup>,
    ) -> Result<wf_project_remote::ProjectRemoteUpdateResult, String>,
{
    let result = wf_project_remote::run_remote_update(work_root, requested_version, group, sync_fn)?;

    if json {
        let body = serde_json::to_string_pretty(&result)
            .map_err(|e| format!("encode update result: {e}"))?;
        println!("{body}");
        return Ok(());
    }

    println!("Project remote update");
    println!("  Work Root : {}", work_root.display());
    println!(
        "  Request   : {}",
        result.requested_version.as_deref().unwrap_or("(auto)")
    );
    println!("  Version   : {}", result.current_version);
    println!("  Tag       : {}", result.resolved_tag);
    println!(
        "  From      : {}",
        result.from_revision.as_deref().unwrap_or("-")
    );
    println!("  To        : {}", result.to_revision);
    println!("  Changed   : {}", result.changed);
    Ok(())
}

/// Load `conf/wfusion.toml` and return the `[project_remote]` config.
/// Errors if the config is missing or `[project_remote]` is disabled.
///
/// `work_dir` is set to `work_root` so that relative paths in the config
/// (e.g. `sources_dir`, `rules`) resolve against the project root, not the
/// process cwd.
fn load_project_remote_conf(
    work_root: &Path,
) -> Result<wf_config::project_remote::ProjectRemoteConf, String> {
    let conf_path = work_root.join(CONF_REL_PATH);
    let config =
        FusionConfig::load_with_context(&conf_path, &ConfigVarContext::new(), Some(work_root))
            .map_err(|e| format!("load {} failed: {e}", conf_path.display()))?;
    Ok(config.project_remote)
}


fn parse_group(raw: Option<&str>) -> Result<Option<RemoteGroup>, String> {
    match raw {
        None | Some("") => Ok(None),
        Some(s) => s.parse::<RemoteGroup>().map(Some),
    }
}

fn resolve_work_root(raw: &str) -> Result<PathBuf, String> {
    std::fs::canonicalize(raw).map_err(|e| format!("resolve work root '{}' failed: {e}", raw))
}

// ── Diff ──────────────────────────────────────────────────────────────

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

fn run_conf_diff(args: ConfDiffArgs) -> Result<(), String> {
    let ctx = resolve_load(
        &args.config,
        &args.overlay,
        &args.var,
        args.work_dir.as_deref(),
    )?;
    let cmp_config = args.to_config.as_deref().unwrap_or(&ctx.config_path);
    let cmp_ctx = resolve_load(
        cmp_config,
        &args.to_overlay,
        &args.to_var,
        args.to_work_dir.as_deref(),
    )?;

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

    let left = if args.expanded {
        l.load_expanded_raw().map_err(|e| format!("{e}"))?
    } else {
        l.load_raw().map_err(|e| format!("{e}"))?
    };
    let right = if args.expanded {
        r.load_expanded_raw().map_err(|e| format!("{e}"))?
    } else {
        r.load_raw().map_err(|e| format!("{e}"))?
    };

    let changes: Vec<_> = left
        .diff(&right)
        .into_iter()
        .filter(|c| matches_any_prefix(&c.path, &args.path_prefix))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_exact_path() {
        assert!(path_matches_prefix("mode", "mode"));
        assert!(matches_any_prefix("mode", &["mode".into()]));
    }

    #[test]
    fn match_child_path() {
        assert!(path_matches_prefix("runtime.executor", "runtime"));
        assert!(path_matches_prefix("window[0].name", "window"));
    }

    #[test]
    fn no_match_unrelated() {
        assert!(!path_matches_prefix("mode", "runtime"));
        assert!(!path_matches_prefix("runtime", "mode"));
    }

    #[test]
    fn empty_prefixes_match_all() {
        assert!(matches_any_prefix("anything", &[]));
    }
}
