//! Test fixtures for project_remote — local git remote + work_root helpers.
//!
//! Mirrors wparse `test_support`, but tests construct `ProjectRemoteConf`
//! directly (wfusion's `sync_project_remote` receives the config from the
//! caller) instead of writing/loading a conf file.

use std::fs;
use std::path::{Path, PathBuf};

use git2::{Oid, Repository, Signature};
use tempfile::{tempdir, TempDir};

use wf_config::project_remote::{ProjectRemoteConf, RepoGroupConf};

pub(super) struct RemoteFixture {
    _temp: TempDir,
    remote_path: PathBuf,
}

impl RemoteFixture {
    pub(super) fn repo_url(&self) -> &str {
        self.remote_path.to_str().expect("repo path utf8")
    }
}

/// Single-repo remote with two releases: v1.4.2 (init) and v1.4.3 (latest).
pub(super) fn create_remote_fixture() -> RemoteFixture {
    let temp = tempdir().expect("tempdir");
    let repo = Repository::init(temp.path()).expect("init remote repo");
    fs::create_dir_all(temp.path().join("models")).expect("create models dir");
    fs::create_dir_all(temp.path().join("topology")).expect("create topology dir");
    fs::create_dir_all(temp.path().join("connectors")).expect("create connectors dir");
    fs::write(temp.path().join("models/version.txt"), "1.4.2\n").expect("write v1.4.2");
    commit_all(&repo, "release 1.4.2");
    tag_head(&repo, "v1.4.2");

    fs::write(temp.path().join("models/version.txt"), "1.4.3\n").expect("write v1.4.3");
    commit_all(&repo, "release 1.4.3");
    tag_head(&repo, "v1.4.3");

    RemoteFixture {
        remote_path: temp.path().to_path_buf(),
        _temp: temp,
    }
}

/// Remote with no release tags — exercises HEAD fallback.
pub(super) fn create_remote_fixture_without_tags() -> RemoteFixture {
    let temp = tempdir().expect("tempdir");
    let repo = Repository::init(temp.path()).expect("init remote repo");
    fs::create_dir_all(temp.path().join("models")).expect("create models dir");
    fs::create_dir_all(temp.path().join("topology")).expect("create topology dir");
    fs::create_dir_all(temp.path().join("connectors")).expect("create connectors dir");
    fs::write(temp.path().join("models/version.txt"), "head\n").expect("write head marker");
    commit_all(&repo, "initial head");

    RemoteFixture {
        remote_path: temp.path().to_path_buf(),
        _temp: temp,
    }
}

pub(super) fn create_work_root(_remote: &RemoteFixture) -> TempDir {
    tempdir().expect("tempdir")
}

/// Single-repo `ProjectRemoteConf` pointing at `repo_url` with `init_version`.
pub(super) fn single_conf(repo_url: &str, init_version: &str) -> ProjectRemoteConf {
    ProjectRemoteConf {
        enabled: true,
        repo: repo_url.to_string(),
        init_version: init_version.to_string(),
        models: None,
        infra: None,
    }
}

/// Dual-repo `ProjectRemoteConf`.
pub(super) fn dual_conf(
    models_repo_url: &str,
    infra_repo_url: &str,
) -> ProjectRemoteConf {
    ProjectRemoteConf {
        enabled: true,
        repo: String::new(),
        init_version: String::new(),
        models: Some(RepoGroupConf {
            repo: models_repo_url.to_string(),
            init_version: "1.4.2".to_string(),
        }),
        infra: Some(RepoGroupConf {
            repo: infra_repo_url.to_string(),
            init_version: "1.0.0".to_string(),
        }),
    }
}

pub(super) fn write_model_version(work_root: &Path, version: &str) {
    fs::create_dir_all(work_root.join("models")).expect("create current models");
    fs::write(work_root.join("models/version.txt"), format!("{version}\n"))
        .expect("write current version");
}

pub(super) fn create_empty_managed_dirs(work_root: &Path) {
    fs::create_dir_all(work_root.join("conf")).expect("create conf dir");
    fs::create_dir_all(work_root.join("models")).expect("create current models");
    fs::create_dir_all(work_root.join("topology")).expect("create topology dir");
    fs::create_dir_all(work_root.join("connectors")).expect("create connectors dir");
}

pub(super) fn write_runtime_local_dirs(work_root: &Path) {
    fs::create_dir_all(work_root.join("runtime")).expect("create runtime");
    fs::create_dir_all(work_root.join("data")).expect("create data");
    fs::write(work_root.join("runtime/admin_api.token"), "token\n").expect("write token");
    fs::write(work_root.join("data/local.dat"), "local\n").expect("write data");
}

fn commit_all(repo: &Repository, message: &str) -> Oid {
    let mut index = repo.index().expect("open index");
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .expect("add all");
    index.write().expect("write index");
    let tree_id = index.write_tree().expect("write tree");
    let tree = repo.find_tree(tree_id).expect("find tree");
    let sig = Signature::now("wfadm-test", "wfadm@test.local").expect("signature");
    let parent = repo
        .head()
        .ok()
        .and_then(|head| head.target())
        .and_then(|oid| repo.find_commit(oid).ok());
    match parent.as_ref() {
        Some(parent) => repo
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &[parent])
            .expect("commit with parent"),
        None => repo
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &[])
            .expect("initial commit"),
    }
}

fn tag_head(repo: &Repository, tag: &str) {
    let obj = repo
        .head()
        .expect("head")
        .peel(git2::ObjectType::Commit)
        .expect("peel head");
    repo.tag_lightweight(tag, &obj, false)
        .expect("create lightweight tag");
}

// ── dual-repo fixtures ────────────────────────────────────────────────

pub(super) fn create_models_remote_fixture() -> RemoteFixture {
    let temp = tempdir().expect("tempdir");
    let repo = Repository::init(temp.path()).expect("init models remote repo");
    fs::create_dir_all(temp.path().join("models")).expect("create models dir");
    fs::write(temp.path().join("models/version.txt"), "1.4.2\n").expect("write v1.4.2");
    commit_all(&repo, "models release 1.4.2");
    tag_head(&repo, "v1.4.2");

    fs::write(temp.path().join("models/version.txt"), "1.4.3\n").expect("write v1.4.3");
    commit_all(&repo, "models release 1.4.3");
    tag_head(&repo, "v1.4.3");

    RemoteFixture {
        remote_path: temp.path().to_path_buf(),
        _temp: temp,
    }
}

pub(super) fn create_infra_remote_fixture() -> RemoteFixture {
    let temp = tempdir().expect("tempdir");
    let repo = Repository::init(temp.path()).expect("init infra remote repo");
    fs::create_dir_all(temp.path().join("conf")).expect("create conf dir");
    fs::create_dir_all(temp.path().join("topology")).expect("create topology dir");
    fs::create_dir_all(temp.path().join("connectors")).expect("create connectors dir");
    fs::write(
        temp.path().join("conf/infra.toml"),
        "[infra]\nversion = \"1.0.0\"\n",
    )
    .expect("write v1.0.0");
    commit_all(&repo, "infra release 1.0.0");
    tag_head(&repo, "v1.0.0");

    fs::write(
        temp.path().join("conf/infra.toml"),
        "[infra]\nversion = \"1.1.0\"\n",
    )
    .expect("write v1.1.0");
    commit_all(&repo, "infra release 1.1.0");
    tag_head(&repo, "v1.1.0");

    RemoteFixture {
        remote_path: temp.path().to_path_buf(),
        _temp: temp,
    }
}

pub(super) fn create_dual_work_root(
    _models_remote: &RemoteFixture,
    _infra_remote: &RemoteFixture,
) -> TempDir {
    tempdir().expect("tempdir")
}
