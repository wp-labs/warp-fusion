//! `wfadm conf` — remote rule-source version sync (`conf update`).
//!
//! Mirrors wparse `wproj conf update`: load config → lock → snapshot → sync
//! managed dirs from remote git → validate → rollback on failure → output.
//! See `docs/design/project_remote_alignment.md`.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use wf_config::FusionConfig;

use crate::project_remote::{
    self, acquire_project_remote_lock, capture_project_remote_snapshot_with_group,
    restore_project_remote_update, RemoteGroup,
};

const CONF_REL_PATH: &str = "conf/wfusion.toml";

#[derive(Subcommand, Debug)]
pub enum ConfCmd {
    /// Update managed dirs (models/conf/topology/connectors) from the remote
    /// git repo configured in `[project_remote]`, at a given version tag.
    #[command(visible_alias = "更新")]
    Update(ConfUpdateArgs),
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
        ConfCmd::Update(args) => run_conf_update(args),
    }
}

fn run_conf_update(args: ConfUpdateArgs) -> Result<(), String> {
    let work_root = resolve_work_root(&args.work_root)?;
    let group = parse_group(args.group.as_deref())?;

    tracing::info!(
        domain = "sys",
        "wfadm conf update start work_root={} requested_version={} json={} group={}",
        work_root.display(),
        args.version.as_deref().unwrap_or("(auto)"),
        args.json,
        group
            .map(|g| match g {
                RemoteGroup::Models => "models",
                RemoteGroup::Infra => "infra",
            })
            .unwrap_or("-")
    );

    // Load config (env-expanded) and extract [project_remote].
    let remote_conf = load_project_remote_conf(&work_root)?;

    let _lock_guard = acquire_project_remote_lock(&work_root)?;
    let rollback_snapshot = capture_project_remote_snapshot_with_group(&work_root, group)?;

    let result = match group {
        Some(g) => project_remote::sync_project_remote_group(
            &work_root,
            g,
            &remote_conf,
            args.version.as_deref(),
        )?,
        None => project_remote::sync_project_remote(
            &work_root,
            &remote_conf,
            args.version.as_deref(),
        )?,
    };
    tracing::info!(
        domain = "sys",
        "wfadm conf update synced work_root={} current_version={} resolved_tag={} from_revision={} to_revision={} changed={}",
        work_root.display(),
        result.current_version,
        result.resolved_tag,
        result.from_revision.as_deref().unwrap_or("-"),
        result.to_revision,
        result.changed
    );

    // Validate: the newly synced config must load. On failure, roll back.
    tracing::info!(
        domain = "sys",
        "wfadm conf update validate start work_root={} version={}",
        work_root.display(),
        result.current_version
    );
    if let Err(check_err) = validate_config_loads(&work_root) {
        tracing::warn!(
            domain = "sys",
            "wfadm conf update validate failed work_root={} current_version={} resolved_tag={} error={}",
            work_root.display(),
            result.current_version,
            result.resolved_tag,
            check_err
        );
        if let Err(rollback_err) = restore_project_remote_update(
            &work_root,
            &rollback_snapshot,
            result.changed,
        ) {
            tracing::warn!(
                domain = "sys",
                "wfadm conf update rollback failed work_root={} error={}",
                work_root.display(),
                rollback_err
            );
            return Err(format!(
                "project check failed after update: {}; rollback failed: {}",
                check_err, rollback_err
            ));
        }
        tracing::info!(
            domain = "sys",
            "wfadm conf update rollback done work_root={} reverted_from_version={}",
            work_root.display(),
            result.current_version
        );
        return Err(format!(
            "project check failed after update: {}",
            check_err
        ));
    }
    tracing::info!(
        domain = "sys",
        "wfadm conf update validate passed work_root={} current_version={} resolved_tag={}",
        work_root.display(),
        result.current_version,
        result.resolved_tag
    );

    if args.json {
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
fn load_project_remote_conf(work_root: &Path) -> Result<wf_config::project_remote::ProjectRemoteConf, String> {
    let conf_path = work_root.join(CONF_REL_PATH);
    let config = FusionConfig::load(&conf_path).map_err(|e| {
        format!(
            "load {} failed: {e}",
            conf_path.display()
        )
    })?;
    Ok(config.project_remote)
}

/// Validation gate: the synced config must be loadable.
///
/// This mirrors the intent of wparse's `validate_load_model` (ensure the new
/// rules/conf can be loaded by the engine) using wfusion's public config
/// loader. Full rule-compilation validation requires a wf-runtime validate
/// entry point (not yet public) and is tracked as a follow-up.
fn validate_config_loads(work_root: &Path) -> Result<(), String> {
    let conf_path = work_root.join(CONF_REL_PATH);
    FusionConfig::load(&conf_path)
        .map(|_| ())
        .map_err(|e| format!("validate {} failed: {e}", conf_path.display()))
}

fn parse_group(raw: Option<&str>) -> Result<Option<RemoteGroup>, String> {
    match raw {
        None | Some("") => Ok(None),
        Some(s) => s.parse::<RemoteGroup>().map(Some),
    }
}

fn resolve_work_root(raw: &str) -> Result<PathBuf, String> {
    std::fs::canonicalize(raw)
        .map_err(|e| format!("resolve work root '{}' failed: {e}", raw))
}
