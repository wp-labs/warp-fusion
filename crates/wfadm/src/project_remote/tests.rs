use super::test_support::{
    create_dual_work_root, create_empty_managed_dirs, create_infra_remote_fixture,
    create_models_remote_fixture, create_remote_fixture, create_remote_fixture_without_tags,
    create_work_root, dual_conf, single_conf, write_model_version, write_runtime_local_dirs,
};
use super::*;
use std::fs;
use tempfile::tempdir;

#[test]
fn sync_project_remote_updates_to_requested_version_and_persists_state() {
    let fixture = create_remote_fixture();
    let work_root = create_work_root(&fixture);
    let conf = single_conf(fixture.repo_url(), "1.4.2");
    write_model_version(work_root.path(), "1.4.2");
    write_runtime_local_dirs(work_root.path());

    let result = sync_project_remote(work_root.path(), &conf, Some("1.4.3")).expect("sync remote");

    assert_eq!(result.requested_version.as_deref(), Some("1.4.3"));
    assert_eq!(result.current_version, "1.4.3");
    assert_eq!(result.resolved_tag, "v1.4.3");
    assert!(result.changed);
    assert_eq!(
        fs::read_to_string(work_root.path().join("models/version.txt")).expect("read version"),
        "1.4.3\n"
    );
    // runtime/local dirs are not managed — must be preserved.
    assert_eq!(
        fs::read_to_string(work_root.path().join("runtime/admin_api.token")).expect("read token"),
        "token\n"
    );

    let state: serde_json::Value = serde_json::from_slice(
        &fs::read(work_root.path().join(STATE_PATH)).expect("read state file"),
    )
    .expect("parse state json");
    assert_eq!(state["current_version"], "1.4.3");
    assert_eq!(state["resolved_tag"], "v1.4.3");
    assert_eq!(state["revision"], result.to_revision);
}

#[test]
fn sync_project_remote_uses_init_version_when_state_file_is_missing() {
    let fixture = create_remote_fixture();
    let work_root = create_work_root(&fixture);
    let conf = single_conf(fixture.repo_url(), "1.4.2");
    create_empty_managed_dirs(work_root.path());

    let result = sync_project_remote(work_root.path(), &conf, None).expect("sync remote");

    assert_eq!(result.requested_version, None);
    assert_eq!(result.current_version, "1.4.2");
    assert_eq!(result.resolved_tag, "v1.4.2");
}

#[test]
fn sync_project_remote_uses_latest_release_when_state_file_exists() {
    let fixture = create_remote_fixture();
    let work_root = create_work_root(&fixture);
    let conf = single_conf(fixture.repo_url(), "1.4.2");
    write_model_version(work_root.path(), "1.4.2");
    persist_state(
        work_root.path(),
        &ProjectRemoteUpdateResult {
            requested_version: Some("1.4.2".to_string()),
            current_version: "1.4.2".to_string(),
            resolved_tag: "v1.4.2".to_string(),
            from_revision: None,
            to_revision: "old-revision".to_string(),
            changed: false,
            group: None,
        },
    )
    .expect("persist prior state");

    let result = sync_project_remote(work_root.path(), &conf, None).expect("sync remote");

    assert_eq!(result.requested_version, None);
    assert_eq!(result.current_version, "1.4.3");
    assert_eq!(result.resolved_tag, "v1.4.3");
}

#[test]
fn sync_project_remote_falls_back_to_remote_head_when_no_release_tags_exist() {
    let fixture = create_remote_fixture_without_tags();
    let work_root = create_work_root(&fixture);
    // empty init_version → no init target; no release tags → HEAD fallback
    let conf = single_conf(fixture.repo_url(), "");
    create_empty_managed_dirs(work_root.path());

    let result = sync_project_remote(work_root.path(), &conf, None).expect("sync remote");

    assert_eq!(result.requested_version, None);
    assert!(result.resolved_tag.starts_with("HEAD@"));
    assert_eq!(
        result.current_version,
        result.resolved_tag.trim_start_matches("HEAD@")
    );
    assert_eq!(
        fs::read_to_string(work_root.path().join("models/version.txt")).expect("read version"),
        "head\n"
    );
}

#[test]
fn sync_project_remote_preserves_runtime_local_dirs() {
    let fixture = create_remote_fixture();
    let work_root = create_work_root(&fixture);
    let conf = single_conf(fixture.repo_url(), "1.4.2");
    write_model_version(work_root.path(), "1.4.2");
    write_runtime_local_dirs(work_root.path());

    sync_project_remote(work_root.path(), &conf, Some("1.4.3")).expect("sync remote");

    assert_eq!(
        fs::read_to_string(work_root.path().join("runtime/admin_api.token")).expect("read token"),
        "token\n"
    );
    assert_eq!(
        fs::read_to_string(work_root.path().join("data/local.dat")).expect("read local data"),
        "local\n"
    );
}

#[test]
fn sync_project_remote_initializes_non_git_work_root() {
    let fixture = create_remote_fixture();
    let work_root = create_work_root(&fixture);
    let conf = single_conf(fixture.repo_url(), "1.4.2");
    write_runtime_local_dirs(work_root.path());

    let result = sync_project_remote(work_root.path(), &conf, Some("1.4.2"))
        .expect("sync should initialize");

    assert_eq!(result.current_version, "1.4.2");
    assert_eq!(result.resolved_tag, "v1.4.2");
    // work_root itself must not become a git repo
    assert!(!work_root.path().join(".git").exists());
    // the remote cache is a git repo
    assert!(
        work_root
            .path()
            .join(REMOTE_CACHE_PATH)
            .join(".git")
            .exists()
    );
    assert_eq!(
        fs::read_to_string(work_root.path().join("models/version.txt")).expect("read version"),
        "1.4.2\n"
    );
}

#[test]
fn acquire_project_remote_lock_rejects_second_holder() {
    let work_root = tempdir().expect("tempdir");
    let _first = acquire_project_remote_lock(work_root.path()).expect("first lock");
    let second = acquire_project_remote_lock(work_root.path());
    assert!(second.is_err(), "expected second lock to be rejected");
    let err = second.unwrap_err();
    assert!(
        err.contains("already in progress"),
        "unexpected error: {err}"
    );
}

#[test]
fn dual_sync_models_only_updates_models_dir() {
    let models = create_models_remote_fixture();
    let infra = create_infra_remote_fixture();
    let work_root = create_dual_work_root(&models, &infra);
    let conf = dual_conf(models.repo_url(), infra.repo_url());
    create_empty_managed_dirs(work_root.path());

    let result =
        sync_project_remote_group(work_root.path(), RemoteGroup::Models, &conf, Some("1.4.3"))
            .expect("sync models");

    assert_eq!(result.group.as_deref(), Some("models"));
    assert_eq!(result.current_version, "1.4.3");
    assert_eq!(
        fs::read_to_string(work_root.path().join("models/version.txt"))
            .expect("read models version"),
        "1.4.3\n"
    );
}

#[test]
fn dual_sync_infra_only_updates_infra_dirs() {
    let models = create_models_remote_fixture();
    let infra = create_infra_remote_fixture();
    let work_root = create_dual_work_root(&models, &infra);
    let conf = dual_conf(models.repo_url(), infra.repo_url());
    create_empty_managed_dirs(work_root.path());

    let result =
        sync_project_remote_group(work_root.path(), RemoteGroup::Infra, &conf, Some("1.1.0"))
            .expect("sync infra");

    assert_eq!(result.group.as_deref(), Some("infra"));
    assert_eq!(result.current_version, "1.1.0");
    assert_eq!(
        fs::read_to_string(work_root.path().join("conf/infra.toml")).expect("read infra config"),
        "[infra]\nversion = \"1.1.0\"\n"
    );
}

#[test]
fn restore_project_remote_snapshot_restores_managed_dirs_only() {
    let fixture = create_remote_fixture();
    let work_root = create_work_root(&fixture);
    let conf = single_conf(fixture.repo_url(), "1.4.2");
    write_model_version(work_root.path(), "1.4.2");
    write_runtime_local_dirs(work_root.path());

    let snapshot = capture_project_remote_snapshot(work_root.path()).expect("capture snapshot");
    sync_project_remote(work_root.path(), &conf, Some("1.4.3")).expect("sync remote");
    restore_project_remote_snapshot(work_root.path(), &snapshot).expect("restore snapshot");

    assert_eq!(
        fs::read_to_string(work_root.path().join("models/version.txt")).expect("read version"),
        "1.4.2\n"
    );
    assert_eq!(
        fs::read_to_string(work_root.path().join("runtime/admin_api.token")).expect("read token"),
        "token\n"
    );
}

#[test]
fn restore_project_remote_snapshot_without_backup_manifest_restores_state_only() {
    let fixture = create_remote_fixture();
    let work_root = create_work_root(&fixture);
    let conf = single_conf(fixture.repo_url(), "1.4.2");
    write_model_version(work_root.path(), "1.4.2");

    let snapshot = capture_project_remote_snapshot(work_root.path()).expect("capture snapshot");
    let result = sync_project_remote(work_root.path(), &conf, Some("1.4.2")).expect("sync");
    assert!(!result.changed);
    assert!(work_root.path().join(STATE_PATH).exists());

    restore_project_remote_snapshot(work_root.path(), &snapshot).expect("restore snapshot");

    assert!(!work_root.path().join(STATE_PATH).exists());
    assert_eq!(
        fs::read_to_string(work_root.path().join("models/version.txt")).expect("read version"),
        "1.4.2\n"
    );
}

#[test]
fn restore_project_remote_update_skips_stale_backup_when_update_did_not_change_dirs() {
    let fixture = create_remote_fixture();
    let work_root = create_work_root(&fixture);
    let conf = single_conf(fixture.repo_url(), "1.4.2");
    write_model_version(work_root.path(), "1.4.2");

    sync_project_remote(work_root.path(), &conf, Some("1.4.3")).expect("sync to latest");
    assert_eq!(
        fs::read_to_string(work_root.path().join("models/version.txt")).expect("read version"),
        "1.4.3\n"
    );

    let snapshot = capture_project_remote_snapshot(work_root.path()).expect("capture snapshot");
    let result =
        sync_project_remote(work_root.path(), &conf, Some("1.4.3")).expect("sync unchanged");
    assert!(!result.changed);

    restore_project_remote_update(work_root.path(), &snapshot, result.changed)
        .expect("restore snapshot");

    assert_eq!(
        fs::read_to_string(work_root.path().join("models/version.txt")).expect("read version"),
        "1.4.3\n"
    );
}

#[test]
fn sync_project_remote_rolls_back_when_persist_state_fails() {
    let fixture = create_remote_fixture();
    let work_root = create_work_root(&fixture);
    let conf = single_conf(fixture.repo_url(), "1.4.2");
    write_model_version(work_root.path(), "1.4.2");
    fs::create_dir_all(work_root.path().join(".run")).expect("create run dir");
    // Dual state file → persist_state (single-repo) refuses Dual→Single downgrade,
    // triggering the rollback path.
    fs::write(
            work_root.path().join(STATE_PATH),
            r#"{"models":{"version":"1.4.2","tag":"v1.4.2","revision":"old-revision"},"infra":{"version":"1.0.0","tag":"v1.0.0","revision":"infra-rev"}}"#,
        )
        .expect("write dual state");

    let err =
        sync_project_remote(work_root.path(), &conf, Some("1.4.3")).expect_err("sync should fail");
    assert!(
        err.contains("cannot persist single-repo state over dual-repo state"),
        "unexpected error: {}",
        err
    );
    assert_eq!(
        fs::read_to_string(work_root.path().join("models/version.txt")).expect("read version"),
        "1.4.2\n"
    );
    let state: serde_json::Value = serde_json::from_slice(
        &fs::read(work_root.path().join(STATE_PATH)).expect("read state file"),
    )
    .expect("parse state json");
    assert_eq!(state["models"]["version"], "1.4.2");
}

// ============ Dual-Repo Tests ============

#[test]
fn dual_sync_uses_init_version_when_no_state_and_no_requested_version() {
    let models = create_models_remote_fixture();
    let infra = create_infra_remote_fixture();
    let work_root = create_dual_work_root(&models, &infra);
    let conf = dual_conf(models.repo_url(), infra.repo_url());
    create_empty_managed_dirs(work_root.path());

    let result = sync_project_remote_group(work_root.path(), RemoteGroup::Models, &conf, None)
        .expect("sync models with init_version");

    assert_eq!(result.current_version, "1.4.2");
    assert_eq!(result.resolved_tag, "v1.4.2");
}

#[test]
fn dual_sync_rollback_preserves_other_group_state() {
    let models = create_models_remote_fixture();
    let infra = create_infra_remote_fixture();
    let work_root = create_dual_work_root(&models, &infra);
    let conf = dual_conf(models.repo_url(), infra.repo_url());
    create_empty_managed_dirs(work_root.path());

    sync_project_remote_group(work_root.path(), RemoteGroup::Models, &conf, Some("1.4.2"))
        .expect("sync models v1.4.2");

    persist_group_state(
        work_root.path(),
        RemoteGroup::Infra,
        &ProjectRemoteUpdateResult {
            requested_version: Some("1.0.0".to_string()),
            current_version: "1.0.0".to_string(),
            resolved_tag: "v1.0.0".to_string(),
            from_revision: None,
            to_revision: "infra-rev".to_string(),
            changed: false,
            group: Some("infra".to_string()),
        },
    )
    .expect("inject infra state");

    sync_project_remote_group(work_root.path(), RemoteGroup::Models, &conf, Some("1.4.3"))
        .expect("sync models v1.4.3");

    let state = load_state(work_root.path())
        .expect("load state")
        .expect("state exists");
    match state {
        ProjectRemoteState::Dual { models, infra } => {
            let models = models.expect("models synced");
            let infra = infra.expect("infra synced");
            assert_eq!(models.current_version, "1.4.3");
            assert_eq!(infra.current_version, "1.0.0");
        }
        _ => panic!("expected Dual state"),
    }
}

#[test]
fn dual_sync_persists_group_versions_independently() {
    let models = create_models_remote_fixture();
    let infra = create_infra_remote_fixture();
    let work_root = create_dual_work_root(&models, &infra);
    let conf = dual_conf(models.repo_url(), infra.repo_url());
    create_empty_managed_dirs(work_root.path());

    sync_project_remote_group(work_root.path(), RemoteGroup::Models, &conf, Some("1.4.2"))
        .expect("sync models");
    sync_project_remote_group(work_root.path(), RemoteGroup::Infra, &conf, Some("1.0.0"))
        .expect("sync infra");

    let state_json: serde_json::Value =
        serde_json::from_slice(&fs::read(work_root.path().join(STATE_PATH)).expect("read state"))
            .expect("parse state");
    assert_eq!(state_json["models"]["version"], "1.4.2");
    assert_eq!(state_json["models"]["tag"], "v1.4.2");
    assert_eq!(state_json["infra"]["version"], "1.0.0");
    assert_eq!(state_json["infra"]["tag"], "v1.0.0");
}

#[test]
fn dual_sync_single_repo_with_group_errors() {
    let fixture = create_remote_fixture();
    let work_root = create_work_root(&fixture);
    let conf = single_conf(fixture.repo_url(), "1.4.2");

    let err = sync_project_remote_group(work_root.path(), RemoteGroup::Models, &conf, None)
        .expect_err("should reject group on single repo");
    assert!(err.contains("single-repo"), "unexpected error: {}", err);
}

#[test]
fn dual_sync_dual_repo_without_group_errors() {
    let models = create_models_remote_fixture();
    let infra = create_infra_remote_fixture();
    let work_root = create_dual_work_root(&models, &infra);
    let conf = dual_conf(models.repo_url(), infra.repo_url());

    let err = sync_project_remote(work_root.path(), &conf, None)
        .expect_err("should require group on dual repo");
    assert!(err.contains("--group"), "unexpected error: {}", err);
}

#[test]
fn state_backward_compat_reads_old_single_format() {
    let work_root = tempdir().expect("tempdir");
    let run_dir = work_root.path().join(".run");
    fs::create_dir_all(&run_dir).expect("create .run");
    fs::write(
        work_root.path().join(STATE_PATH),
        r#"{"current_version":"1.4.2","resolved_tag":"v1.4.2","revision":"abc123"}"#,
    )
    .expect("write old state");

    let state = load_state(work_root.path())
        .expect("load state")
        .expect("state exists");
    match state {
        ProjectRemoteState::Single {
            current_version,
            resolved_tag,
            revision,
        } => {
            assert_eq!(current_version, "1.4.2");
            assert_eq!(resolved_tag, "v1.4.2");
            assert_eq!(revision, "abc123");
        }
        _ => panic!("expected Single state, got Dual"),
    }

    let version = current_project_version(work_root.path()).expect("read version");
    assert_eq!(version, Some("1.4.2".to_string()));
}

#[test]
fn state_dual_format_roundtrip() {
    let models = create_models_remote_fixture();
    let infra = create_infra_remote_fixture();
    let work_root = create_dual_work_root(&models, &infra);
    let conf = dual_conf(models.repo_url(), infra.repo_url());
    create_empty_managed_dirs(work_root.path());

    sync_project_remote_group(work_root.path(), RemoteGroup::Models, &conf, Some("1.4.3"))
        .expect("sync models");
    sync_project_remote_group(work_root.path(), RemoteGroup::Infra, &conf, Some("1.1.0"))
        .expect("sync infra");

    let state = load_state(work_root.path())
        .expect("load state")
        .expect("state exists");
    match state {
        ProjectRemoteState::Dual { models, infra } => {
            let models = models.expect("models synced");
            let infra = infra.expect("infra synced");
            assert_eq!(models.current_version, "1.4.3");
            assert_eq!(models.resolved_tag, "v1.4.3");
            assert!(!models.revision.is_empty());
            assert_eq!(infra.current_version, "1.1.0");
            assert_eq!(infra.resolved_tag, "v1.1.0");
            assert!(!infra.revision.is_empty());
        }
        _ => panic!("expected Dual state"),
    }
}

#[test]
fn dual_sync_preserves_runtime_local_dirs() {
    let models = create_models_remote_fixture();
    let infra = create_infra_remote_fixture();
    let work_root = create_dual_work_root(&models, &infra);
    let conf = dual_conf(models.repo_url(), infra.repo_url());
    create_empty_managed_dirs(work_root.path());
    write_runtime_local_dirs(work_root.path());

    sync_project_remote_group(work_root.path(), RemoteGroup::Models, &conf, Some("1.4.3"))
        .expect("sync models");

    assert_eq!(
        fs::read_to_string(work_root.path().join("runtime/admin_api.token")).expect("read token"),
        "token\n"
    );
    assert_eq!(
        fs::read_to_string(work_root.path().join("data/local.dat")).expect("read local data"),
        "local\n"
    );
}

#[test]
fn dual_snapshot_rollback_restores_only_affected_group() {
    let models = create_models_remote_fixture();
    let infra = create_infra_remote_fixture();
    let work_root = create_dual_work_root(&models, &infra);
    let conf = dual_conf(models.repo_url(), infra.repo_url());
    create_empty_managed_dirs(work_root.path());
    fs::write(work_root.path().join("models/local.txt"), "local-data\n")
        .expect("write local models file");

    let snapshot =
        capture_project_remote_snapshot_with_group(work_root.path(), Some(RemoteGroup::Models))
            .expect("capture snapshot");

    sync_project_remote_group(work_root.path(), RemoteGroup::Models, &conf, Some("1.4.3"))
        .expect("sync models v1.4.3");
    assert_eq!(
        fs::read_to_string(work_root.path().join("models/version.txt")).expect("read version"),
        "1.4.3\n"
    );
    assert!(!work_root.path().join("models/local.txt").exists());

    restore_project_remote_update(work_root.path(), &snapshot, true).expect("rollback models");

    assert!(work_root.path().join("models/local.txt").exists());
    assert_eq!(
        fs::read_to_string(work_root.path().join("models/local.txt")).expect("read local"),
        "local-data\n"
    );
    assert!(!work_root.path().join("models/version.txt").exists());
    // conf dir (infra group) must be untouched by models rollback
    assert!(work_root.path().join("conf").exists());
}

#[test]
fn dual_sync_initializes_cache_for_each_group_separately() {
    let models = create_models_remote_fixture();
    let infra = create_infra_remote_fixture();
    let work_root = create_dual_work_root(&models, &infra);
    let conf = dual_conf(models.repo_url(), infra.repo_url());
    create_empty_managed_dirs(work_root.path());

    sync_project_remote_group(work_root.path(), RemoteGroup::Models, &conf, Some("1.4.2"))
        .expect("sync models");
    sync_project_remote_group(work_root.path(), RemoteGroup::Infra, &conf, Some("1.0.0"))
        .expect("sync infra");

    assert!(
        work_root
            .path()
            .join(REMOTE_CACHE_PATH_MODELS)
            .join(".git")
            .exists()
    );
    assert!(
        work_root
            .path()
            .join(REMOTE_CACHE_PATH_INFRA)
            .join(".git")
            .exists()
    );
    assert!(!work_root.path().join(REMOTE_CACHE_PATH).exists());
}

#[test]
fn dual_sync_second_group_uses_init_version_when_first_group_already_synced() {
    let models = create_models_remote_fixture();
    let infra = create_infra_remote_fixture();
    let work_root = create_dual_work_root(&models, &infra);
    let conf = dual_conf(models.repo_url(), infra.repo_url());
    create_empty_managed_dirs(work_root.path());

    sync_project_remote_group(work_root.path(), RemoteGroup::Models, &conf, None)
        .expect("sync models first");

    // Infra has no state yet → must use its own init_version (1.0.0), not latest (1.1.0).
    let result = sync_project_remote_group(work_root.path(), RemoteGroup::Infra, &conf, None)
        .expect("sync infra second");

    assert_eq!(result.current_version, "1.0.0");
    assert_eq!(result.resolved_tag, "v1.0.0");
    assert_eq!(result.group.as_deref(), Some("infra"));
}

#[test]
fn restore_managed_dirs_cleans_up_dirs_created_during_failed_update() {
    let work_root = tempdir().expect("tempdir");
    let dirs: &[&str] = &["models", "conf"];
    let backup_root = work_root.path().join(BACKUP_PATH);
    let manifest_path = work_root.path().join(BACKUP_MANIFEST_PATH);

    fs::create_dir_all(work_root.path().join("models")).expect("create models");
    fs::write(work_root.path().join("models/version.txt"), "1.4.2\n").expect("write version");

    fs::create_dir_all(&backup_root).expect("create backup root");
    fs::create_dir_all(backup_root.join("models")).expect("create backup models");
    fs::write(backup_root.join("models/version.txt"), "1.4.2\n").expect("write backup version");
    let manifest = BackupManifest {
        existing_dirs: vec!["models".to_string()],
    };
    let body = serde_json::to_vec_pretty(&manifest).expect("encode manifest");
    fs::write(&manifest_path, body).expect("write manifest");

    // Failed update created conf/ (not in backup manifest)
    fs::create_dir_all(work_root.path().join("conf")).expect("create conf during update");
    fs::write(work_root.path().join("conf/new.toml"), "[new]\n").expect("write new conf");

    restore_managed_dirs(work_root.path(), dirs).expect("restore");

    assert!(
        !work_root.path().join("conf").exists(),
        "conf/ should be removed (not in backup manifest)"
    );
    assert_eq!(
        fs::read_to_string(work_root.path().join("models/version.txt")).expect("read version"),
        "1.4.2\n"
    );
}

#[test]
fn restore_managed_dirs_no_manifest_is_noop() {
    let work_root = tempdir().expect("tempdir");
    let dirs: &[&str] = &["models"];

    fs::create_dir_all(work_root.path().join("models")).expect("create models");
    fs::write(work_root.path().join("models/version.txt"), "data\n").expect("write data");

    restore_managed_dirs(work_root.path(), dirs).expect("restore without manifest");

    assert_eq!(
        fs::read_to_string(work_root.path().join("models/version.txt")).expect("read version"),
        "data\n"
    );
}
