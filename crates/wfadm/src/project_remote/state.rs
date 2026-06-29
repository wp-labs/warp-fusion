use std::fs;
use std::path::Path;

use super::managed::{managed_dirs_for, restore_managed_dirs};
use super::{
    conf_err_source, GroupState, ProjectRemoteLockGuard, ProjectRemoteSnapshot, ProjectRemoteState,
    ProjectRemoteUpdateResult, ProjectRuntimeArtifactSnapshot, RemoteGroup, AUTHORITY_DB_PATH,
    LOCK_PATH, RULE_MAPPING_PATH, STATE_PATH,
};

pub fn acquire_project_remote_lock<P: AsRef<Path>>(
    work_root: P,
) -> Result<ProjectRemoteLockGuard, String> {
    let work_root = work_root.as_ref();
    let lock_path = work_root.join(LOCK_PATH);
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| conf_err_source(format!("create {} failed", parent.display()), e))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|e| conf_err_source(format!("open {} failed", lock_path.display()), e))?;
    try_lock_file(&file, &lock_path)?;
    tracing::info!(
        domain = "sys",
        "project remote lock acquired work_root={} lock_path={}",
        work_root.display(),
        lock_path.display()
    );
    Ok(ProjectRemoteLockGuard { file })
}

pub fn capture_project_remote_snapshot<P: AsRef<Path>>(
    work_root: P,
) -> Result<ProjectRemoteSnapshot, String> {
    capture_project_remote_snapshot_with_group(work_root, None)
}

pub fn capture_project_remote_snapshot_with_group<P: AsRef<Path>>(
    work_root: P,
    group: Option<RemoteGroup>,
) -> Result<ProjectRemoteSnapshot, String> {
    let work_root = work_root.as_ref();
    let state_path = work_root.join(STATE_PATH);
    let state_file = match fs::read(&state_path) {
        Ok(bytes) => Some(bytes),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            return Err(conf_err_source(
                format!("read {} failed", state_path.display()),
                err,
            ))
        }
    };
    Ok(ProjectRemoteSnapshot { state_file, group })
}

pub fn restore_project_remote_snapshot<P: AsRef<Path>>(
    work_root: P,
    snapshot: &ProjectRemoteSnapshot,
) -> Result<(), String> {
    restore_project_remote_update(work_root, snapshot, true)
}

pub fn restore_project_remote_update<P: AsRef<Path>>(
    work_root: P,
    snapshot: &ProjectRemoteSnapshot,
    changed: bool,
) -> Result<(), String> {
    let work_root = work_root.as_ref();
    if changed {
        let dirs = managed_dirs_for(snapshot.group);
        restore_managed_dirs(work_root, dirs)?;
    }
    restore_state_file_bytes(work_root, snapshot.state_file.as_deref())?;
    Ok(())
}

pub fn capture_runtime_artifact_snapshot<P: AsRef<Path>>(
    work_root: P,
) -> Result<ProjectRuntimeArtifactSnapshot, String> {
    let work_root = work_root.as_ref();
    Ok(ProjectRuntimeArtifactSnapshot {
        rule_mapping: read_optional_file(&work_root.join(RULE_MAPPING_PATH))?,
        authority_db: read_optional_file(&work_root.join(AUTHORITY_DB_PATH))?,
    })
}

pub fn restore_runtime_artifact_snapshot<P: AsRef<Path>>(
    work_root: P,
    snapshot: &ProjectRuntimeArtifactSnapshot,
) -> Result<(), String> {
    let work_root = work_root.as_ref();
    restore_optional_file(
        &work_root.join(RULE_MAPPING_PATH),
        snapshot.rule_mapping.as_deref(),
    )?;
    restore_optional_file(
        &work_root.join(AUTHORITY_DB_PATH),
        snapshot.authority_db.as_deref(),
    )?;
    Ok(())
}

pub(super) fn load_state(work_root: &Path) -> Result<Option<ProjectRemoteState>, String> {
    let path = work_root.join(STATE_PATH);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(conf_err_source(
                format!("read {} failed", path.display()),
                err,
            ))
        }
    };
    let state = serde_json::from_slice(&bytes)
        .map_err(|e| conf_err_source(format!("parse {} failed", path.display()), e))?;
    Ok(Some(state))
}

pub(super) fn restore_project_remote_state(
    work_root: &Path,
    previous_state: Option<&ProjectRemoteState>,
) -> Result<(), String> {
    match previous_state {
        Some(state) => {
            let body = serde_json::to_vec_pretty(state)
                .map_err(|e| conf_err_source("encode project remote state failed", e))?;
            restore_state_file_bytes(work_root, Some(body.as_slice()))
        }
        None => restore_state_file_bytes(work_root, None),
    }
}

fn restore_state_file_bytes(work_root: &Path, bytes: Option<&[u8]>) -> Result<(), String> {
    let state_path = work_root.join(STATE_PATH);
    restore_optional_file(&state_path, bytes)
}

fn atomic_write_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| conf_err_source(format!("create {} failed", parent.display()), e))?;
    }
    let tmp_path = path.with_extension(".tmp");
    fs::write(&tmp_path, bytes)
        .map_err(|e| conf_err_source(format!("write {} failed", tmp_path.display()), e))?;
    fs::rename(&tmp_path, path).map_err(|e| {
        let _ = fs::remove_file(&tmp_path);
        conf_err_source(
            format!("rename {} -> {} failed", tmp_path.display(), path.display()),
            e,
        )
    })
}

fn restore_optional_file(path: &Path, bytes: Option<&[u8]>) -> Result<(), String> {
    match bytes {
        Some(bytes) => atomic_write_file(path, bytes)?,
        None => {
            if let Err(err) = fs::remove_file(path) {
                if err.kind() != std::io::ErrorKind::NotFound {
                    return Err(conf_err_source(
                        format!("remove {} failed", path.display()),
                        err,
                    ));
                }
            }
        }
    }
    Ok(())
}

fn read_optional_file(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(conf_err_source(
            format!("read {} failed", path.display()),
            err,
        )),
    }
}

fn try_lock_file(file: &fs::File, lock_path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;

        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        match err.kind() {
            std::io::ErrorKind::WouldBlock => Err(project_remote_lock_busy_err(
                lock_path.display().to_string(),
            )),
            _ => Err(conf_err_source(
                format!("lock {} failed", lock_path.display()),
                err,
            )),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (file, lock_path);
        Ok(())
    }
}

impl Drop for ProjectRemoteLockGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;

            let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}

fn project_remote_lock_busy_err(lock_path: impl Into<String>) -> String {
    format!(
        "project remote update already in progress ({})",
        lock_path.into()
    )
}

pub(super) fn persist_state(work_root: &Path, result: &ProjectRemoteUpdateResult) -> Result<(), String> {
    // Prevent overwriting dual-repo state with single-repo state
    if let Some(ProjectRemoteState::Dual { .. }) = load_state(work_root)? {
        return Err(
            "cannot persist single-repo state over dual-repo state; use persist_group_state"
                .to_string(),
        );
    }
    let state = ProjectRemoteState::Single {
        current_version: result.current_version.clone(),
        resolved_tag: result.resolved_tag.clone(),
        revision: result.to_revision.clone(),
    };
    let path = work_root.join(STATE_PATH);
    let body = serde_json::to_vec_pretty(&state)
        .map_err(|e| conf_err_source("encode project remote state failed", e))?;
    atomic_write_file(&path, &body)
}

pub(super) fn persist_group_state(
    work_root: &Path,
    group: RemoteGroup,
    result: &ProjectRemoteUpdateResult,
) -> Result<(), String> {
    let new_group = GroupState {
        current_version: result.current_version.clone(),
        resolved_tag: result.resolved_tag.clone(),
        revision: result.to_revision.clone(),
    };
    let state = match load_state(work_root)? {
        Some(ProjectRemoteState::Dual { models, infra }) => match group {
            RemoteGroup::Models => ProjectRemoteState::Dual {
                models: Some(new_group),
                infra,
            },
            RemoteGroup::Infra => ProjectRemoteState::Dual {
                models,
                infra: Some(new_group),
            },
        },
        _ => match group {
            RemoteGroup::Models => ProjectRemoteState::Dual {
                models: Some(new_group),
                infra: None,
            },
            RemoteGroup::Infra => ProjectRemoteState::Dual {
                models: None,
                infra: Some(new_group),
            },
        },
    };
    let path = work_root.join(STATE_PATH);
    let body = serde_json::to_vec_pretty(&state)
        .map_err(|e| conf_err_source("encode project remote state failed", e))?;
    atomic_write_file(&path, &body)
}
