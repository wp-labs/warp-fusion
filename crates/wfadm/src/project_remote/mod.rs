//! Project remote rule-source sync — mirrors warp-parse `project_remote`.
//!
//! Syncs managed dirs (`models`/`conf`/`topology`/`connectors`) from a remote
//! git repo at a given version tag, persists version state, and rolls back on
//! failure. See `docs/design/project_remote_alignment.md`.
//!
//! Unlike wparse, config loading + env expansion is done by the caller; these
//! entry points receive an already-loaded `&ProjectRemoteConf`. Errors are
//! `String` (not wparse's `RunError`), logging uses `tracing`.

use std::fs;
use std::path::Path;

use git2::Oid;
use serde::{Deserialize, Serialize};
use wf_config::project_remote::{ProjectRemoteConf, RepoGroupConf};

mod managed;
mod repo;
mod state;

use self::managed::{
    backup_managed_dirs, managed_dirs_differ, managed_dirs_for, restore_managed_dirs,
    sync_managed_dirs,
};
use self::repo::{
    checkout_commit, fetch_remote_tags, prepare_remote_repo, resolve_default_target,
    resolve_tag_for_version,
};
pub use self::state::{
    acquire_project_remote_lock, capture_project_remote_snapshot_with_group,
    restore_project_remote_update,
};
#[cfg(test)]
use self::state::{capture_project_remote_snapshot, restore_project_remote_snapshot};
use self::state::{load_state, persist_group_state, persist_state, restore_project_remote_state};

const ENGINE_CONF_PATH: &str = "conf/wfusion.toml";
const STATE_PATH: &str = ".run/project_remote_state.json";
const REMOTE_CACHE_PATH: &str = ".run/project_remote/remote";
const REMOTE_CACHE_PATH_MODELS: &str = ".run/project_remote/remote-models";
const REMOTE_CACHE_PATH_INFRA: &str = ".run/project_remote/remote-infra";
const BACKUP_PATH: &str = ".run/project_remote/backup";
const BACKUP_MANIFEST_PATH: &str = ".run/project_remote/backup/manifest.json";
const LOCK_PATH: &str = ".run/project_remote.lock";
#[allow(dead_code)]
const RULE_MAPPING_PATH: &str = ".run/rule_mapping.dat";
#[allow(dead_code)]
const AUTHORITY_DB_PATH: &str = ".run/authority.sqlite";

#[derive(Debug, Clone, Serialize)]
pub struct ProjectRemoteUpdateResult {
    pub requested_version: Option<String>,
    pub current_version: String,
    pub resolved_tag: String,
    pub from_revision: Option<String>,
    pub to_revision: String,
    pub changed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProjectRemoteSnapshot {
    pub(super) state_file: Option<Vec<u8>>,
    pub group: Option<RemoteGroup>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ProjectRuntimeArtifactSnapshot {
    pub(super) rule_mapping: Option<Vec<u8>>,
    authority_db: Option<Vec<u8>>,
}

#[derive(Debug)]
pub struct ProjectRemoteLockGuard {
    pub(super) file: fs::File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteGroup {
    Models,
    Infra,
}

impl std::str::FromStr for RemoteGroup {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "models" => Ok(RemoteGroup::Models),
            "infra" => Ok(RemoteGroup::Infra),
            other => Err(format!(
                "invalid group '{}': expected 'models' or 'infra'",
                other
            )),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GroupState {
    #[serde(rename = "version")]
    current_version: String,
    #[serde(rename = "tag")]
    resolved_tag: String,
    revision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum ProjectRemoteState {
    Single {
        current_version: String,
        resolved_tag: String,
        revision: String,
    },
    Dual {
        models: Option<GroupState>,
        infra: Option<GroupState>,
    },
}

impl ProjectRemoteState {
    #[allow(dead_code)]
    fn single_version(&self) -> Option<&str> {
        match self {
            ProjectRemoteState::Single {
                current_version, ..
            } => Some(current_version.as_str()),
            ProjectRemoteState::Dual { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BackupManifest {
    existing_dirs: Vec<String>,
}

pub(crate) enum ProjectRemoteMode {
    Single {
        repo: String,
        init_version: String,
    },
    Dual {
        models: RepoGroupConf,
        infra: RepoGroupConf,
    },
}

struct ResolvedTag {
    tag: String,
    version: String,
    commit_id: Oid,
}

// ── Entry points ─────────────────────────────────────────────────────

/// Sync managed dirs from a single-repo `[project_remote]` source.
/// `remote_conf` is the already-loaded, env-expanded config (caller loads it).
pub fn sync_project_remote<P: AsRef<Path>>(
    work_root: P,
    remote_conf: &ProjectRemoteConf,
    requested_version: Option<&str>,
) -> Result<ProjectRemoteUpdateResult, String> {
    let work_root = work_root.as_ref();
    if !remote_conf.enabled {
        return Err(project_remote_disabled_err(
            work_root.join(ENGINE_CONF_PATH).display().to_string(),
        ));
    }
    let mode = resolve_project_remote_mode(remote_conf)?;
    match mode {
        ProjectRemoteMode::Single { repo, init_version } => sync_project_remote_with_repo_inner(
            work_root,
            &repo,
            requested_version,
            Some(init_version.as_str()),
            None,
        ),
        ProjectRemoteMode::Dual { .. } => Err(project_remote_dual_requires_group_err()),
    }
}

/// Sync one group (models|infra) from a dual-repo `[project_remote]` source.
pub fn sync_project_remote_group<P: AsRef<Path>>(
    work_root: P,
    group: RemoteGroup,
    remote_conf: &ProjectRemoteConf,
    requested_version: Option<&str>,
) -> Result<ProjectRemoteUpdateResult, String> {
    let work_root = work_root.as_ref();
    if !remote_conf.enabled {
        return Err(project_remote_disabled_err(
            work_root.join(ENGINE_CONF_PATH).display().to_string(),
        ));
    }
    let mode = resolve_project_remote_mode(remote_conf)?;
    match mode {
        ProjectRemoteMode::Dual { models, infra } => {
            let group_conf = match group {
                RemoteGroup::Models => &models,
                RemoteGroup::Infra => &infra,
            };
            sync_project_remote_with_repo_inner(
                work_root,
                &group_conf.repo,
                requested_version,
                Some(group_conf.init_version.as_str()),
                Some(group),
            )
        }
        ProjectRemoteMode::Single { .. } => Err(project_remote_single_no_group_err()),
    }
}

/// Sync from an explicit repo URL (not from config) — used by `init --repo`.
pub fn sync_project_remote_from_repo<P: AsRef<Path>>(
    work_root: P,
    repo_url: &str,
    requested_version: Option<&str>,
) -> Result<ProjectRemoteUpdateResult, String> {
    let work_root = work_root.as_ref();
    if repo_url.trim().is_empty() {
        return Err(project_remote_repo_required_err());
    }
    sync_project_remote_with_repo_inner(work_root, repo_url, requested_version, None, None)
}

#[allow(dead_code)]
pub fn current_project_version<P: AsRef<Path>>(work_root: P) -> Result<Option<String>, String> {
    Ok(
        load_state(work_root.as_ref())?
            .and_then(|state| state.single_version().map(str::to_string)),
    )
}

#[allow(dead_code)]
pub fn current_project_group_versions<P: AsRef<Path>>(
    work_root: P,
) -> Result<Option<serde_json::Value>, String> {
    let state = load_state(work_root.as_ref())?;
    match state {
        Some(ProjectRemoteState::Dual { models, infra }) => {
            let mut map = serde_json::Map::new();
            if let Some(m) = models {
                map.insert(
                    "models".to_string(),
                    serde_json::json!({
                        "version": m.current_version,
                        "tag": m.resolved_tag,
                    }),
                );
            }
            if let Some(i) = infra {
                map.insert(
                    "infra".to_string(),
                    serde_json::json!({
                        "version": i.current_version,
                        "tag": i.resolved_tag,
                    }),
                );
            }
            Ok(Some(serde_json::Value::Object(map)))
        }
        _ => Ok(None),
    }
}

pub(crate) fn resolve_project_remote_mode(
    conf: &ProjectRemoteConf,
) -> Result<ProjectRemoteMode, String> {
    let has_single = !conf.repo.trim().is_empty();
    let has_models = conf.models.is_some();
    let has_infra = conf.infra.is_some();

    match (has_single, has_models, has_infra) {
        (true, false, false) => Ok(ProjectRemoteMode::Single {
            repo: conf.repo.clone(),
            init_version: conf.init_version.clone(),
        }),
        (false, true, true) => {
            let models = conf.models.as_ref().unwrap();
            let infra = conf.infra.as_ref().unwrap();
            if models.repo.trim().is_empty() {
                return Err(project_remote_repo_required_err_for("models"));
            }
            if infra.repo.trim().is_empty() {
                return Err(project_remote_repo_required_err_for("infra"));
            }
            Ok(ProjectRemoteMode::Dual {
                models: models.clone(),
                infra: infra.clone(),
            })
        }
        (false, true, false) => Err(project_remote_dual_partial_err("infra")),
        (false, false, true) => Err(project_remote_dual_partial_err("models")),
        _ => Err(project_remote_ambiguous_mode_err()),
    }
}

fn remote_cache_path_for(group: Option<RemoteGroup>) -> &'static str {
    match group {
        Some(RemoteGroup::Models) => REMOTE_CACHE_PATH_MODELS,
        Some(RemoteGroup::Infra) => REMOTE_CACHE_PATH_INFRA,
        None => REMOTE_CACHE_PATH,
    }
}

fn sync_project_remote_with_repo_inner(
    work_root: &Path,
    repo_url: &str,
    requested_version: Option<&str>,
    init_version: Option<&str>,
    group: Option<RemoteGroup>,
) -> Result<ProjectRemoteUpdateResult, String> {
    let dirs = managed_dirs_for(group);
    let group_label = group.map(|g| match g {
        RemoteGroup::Models => "models",
        RemoteGroup::Infra => "infra",
    });
    tracing::info!(
        domain = "sys",
        "project remote sync start work_root={} requested_version={} repo={} group={}",
        work_root.display(),
        requested_version.unwrap_or("(auto)"),
        repo_url,
        group_label.unwrap_or("-")
    );

    let remote_root = work_root.join(remote_cache_path_for(group));
    let repo = prepare_remote_repo(&remote_root, repo_url)?;
    fetch_remote_tags(&repo, repo_url)?;

    let previous_state = load_state(work_root)?;
    let resolved = match requested_version {
        Some(version) if !version.trim().is_empty() => {
            let target_version = version.trim().to_string();
            tracing::info!(
                domain = "sys",
                "project remote sync target resolved work_root={} requested_version={} target_version={} init_version={} state_exists={}",
                work_root.display(),
                requested_version.unwrap_or("(auto)"),
                target_version,
                init_version.unwrap_or("-"),
                previous_state.is_some()
            );
            resolve_tag_for_version(&repo, &target_version)?
                .ok_or_else(|| requested_version_not_found_err(&target_version))?
        }
        _ => {
            let resolved =
                resolve_default_target(work_root, &repo, init_version.map(str::trim), group)?;
            tracing::info!(
                domain = "sys",
                "project remote sync target resolved work_root={} requested_version={} target_version={} init_version={} state_exists={}",
                work_root.display(),
                requested_version.unwrap_or("(auto)"),
                resolved.version,
                init_version.unwrap_or("-"),
                previous_state.is_some()
            );
            resolved
        }
    };
    tracing::info!(
        domain = "sys",
        "project remote sync tag resolved work_root={} requested_version={} current_version={} resolved_tag={} to_revision={}",
        work_root.display(),
        requested_version.unwrap_or("(auto)"),
        resolved.version,
        resolved.tag,
        resolved.commit_id
    );

    checkout_commit(&repo, resolved.commit_id, &resolved.tag)?;

    let changed = managed_dirs_differ(&remote_root, work_root, dirs)?;
    let from_revision = previous_state.as_ref().and_then(|ps| match ps {
        ProjectRemoteState::Single { revision, .. } => Some(revision.as_str()),
        ProjectRemoteState::Dual { models, infra } => match group {
            Some(RemoteGroup::Models) => models.as_ref().map(|m| m.revision.as_str()),
            Some(RemoteGroup::Infra) => infra.as_ref().map(|i| i.revision.as_str()),
            None => None,
        },
    });
    tracing::info!(
        domain = "sys",
        "project remote sync diff work_root={} requested_version={} changed={} from_revision={} to_revision={}",
        work_root.display(),
        requested_version.unwrap_or("(auto)"),
        changed,
        from_revision.unwrap_or("-"),
        resolved.commit_id
    );
    if changed {
        tracing::info!(
            domain = "sys",
            "project remote sync backup managed dirs work_root={} dirs={}",
            work_root.display(),
            dirs.join(",")
        );
        backup_managed_dirs(work_root, dirs)?;
    }

    let result = ProjectRemoteUpdateResult {
        requested_version: requested_version.map(str::to_string),
        current_version: resolved.version,
        resolved_tag: resolved.tag,
        from_revision: from_revision.map(str::to_string),
        to_revision: oid_to_string(resolved.commit_id),
        changed,
        group: group_label.map(str::to_string),
    };
    let apply_result = (|| {
        if changed {
            tracing::info!(
                domain = "sys",
                "project remote sync apply managed dirs work_root={} remote_cache={}",
                work_root.display(),
                remote_root.display()
            );
            sync_managed_dirs(&remote_root, work_root, dirs)?;
        }
        match group {
            Some(g) => persist_group_state(work_root, g, &result)?,
            None => persist_state(work_root, &result)?,
        }
        Ok(())
    })();
    if let Err(err) = apply_result {
        tracing::warn!(
            domain = "sys",
            "project remote sync apply failed work_root={} requested_version={} current_version={} resolved_tag={} changed={} error={}",
            work_root.display(),
            requested_version.unwrap_or("(auto)"),
            result.current_version,
            result.resolved_tag,
            result.changed,
            err
        );
        rollback_partial_update(work_root, previous_state.as_ref(), changed, dirs)
            .map_err(|rollback_err| format!("{}; rollback failed: {rollback_err}", err))?;
        tracing::warn!(
            domain = "sys",
            "project remote sync rollback done work_root={} requested_version={} current_version={} resolved_tag={} changed={}",
            work_root.display(),
            requested_version.unwrap_or("(auto)"),
            result.current_version,
            result.resolved_tag,
            changed
        );
        return Err(err);
    }
    tracing::info!(
        domain = "sys",
        "project remote sync done work_root={} requested_version={} current_version={} resolved_tag={} from_revision={} to_revision={} changed={}",
        work_root.display(),
        requested_version.unwrap_or("(auto)"),
        result.current_version,
        result.resolved_tag,
        result.from_revision.as_deref().unwrap_or("-"),
        result.to_revision,
        result.changed
    );
    Ok(result)
}

fn rollback_partial_update(
    work_root: &Path,
    previous_state: Option<&ProjectRemoteState>,
    changed: bool,
    dirs: &[&str],
) -> Result<(), String> {
    if changed {
        restore_managed_dirs(work_root, dirs)?;
    }
    restore_project_remote_state(work_root, previous_state)
}

fn oid_to_string(oid: Oid) -> String {
    oid.to_string()
}

// ── Error constructors (String, not wparse's RunError) ───────────────

fn project_remote_disabled_err(path: impl Into<String>) -> String {
    format!("project_remote is disabled in {}", path.into())
}

fn project_remote_repo_required_err() -> String {
    "project_remote.repo must not be empty".to_string()
}

fn project_remote_repo_required_err_for(group: &str) -> String {
    format!("project_remote.{}.repo must not be empty", group)
}

fn project_remote_dual_partial_err(missing: &str) -> String {
    format!(
        "dual-repo mode requires both [project_remote.models] and [project_remote.infra]; missing '{}'",
        missing
    )
}

fn project_remote_ambiguous_mode_err() -> String {
    "ambiguous project_remote config: use either 'repo' (single-repo) or both 'models' + 'infra' (dual-repo), not a mix".to_string()
}

fn project_remote_dual_requires_group_err() -> String {
    "dual-repo mode requires --group (models|infra); use sync_project_remote_group".to_string()
}

fn project_remote_single_no_group_err() -> String {
    "single-repo mode does not support --group; use sync_project_remote".to_string()
}

fn requested_version_not_found_err(version: &str) -> String {
    format!("requested version '{}' was not found", version)
}

pub(super) fn conf_err_source<E>(message: impl Into<String>, source: E) -> String
where
    E: std::error::Error + Send + Sync + 'static,
{
    format!("{}: {source}", message.into())
}

#[cfg(test)]
mod test_support;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
