use std::fs;
use std::path::{Path, PathBuf};

use rand::RngCore;

use crate::init_tpl::{Scope, templates_for};

// =====================================================================
// Init
// =====================================================================

/// Default admin API token location: a per-user file under `$HOME/.warp_fusion/`.
/// Kept outside the project so a single token can be shared across projects
/// and so it is never committed accidentally.
const DEFAULT_TOKEN_DIR: &str = ".warp_fusion";
const DEFAULT_TOKEN_FILE: &str = "admin_api.token";

pub fn init_project(project_dir: &str, _name: &str, scope: &str) -> Result<(), String> {
    let root = Path::new(project_dir);

    if root.exists()
        && root
            .read_dir()
            .ok()
            .map(|mut d| d.next().is_some())
            .unwrap_or(false)
    {
        return Err(format!(
            "directory '{}' already exists and is not empty",
            root.display()
        ));
    }

    fs::create_dir_all(root).map_err(|e| format!("create project dir: {e}"))?;

    let scope: Scope = scope.parse().map_err(|e| format!("invalid scope: {e}"))?;

    // 1. Write static templates (rules, schemas, scenarios, topology, conf)
    for (template_path, data) in templates_for(scope) {
        let full = root.join(template_path);

        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("create parent for {template_path}: {e}"))?;
        }
        fs::write(&full, data).map_err(|e| format!("write {template_path}: {e}"))?;
    }

    // 2. Generate connector templates from registry
    crate::connectors::generate_connector_templates(root)
        .map_err(|e| format!("connector generation: {e}"))?;

    // Make scripts executable (on Unix)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for script in &["test_run.sh", "smoke.sh"] {
            let path = root.join(script);
            if path.exists() {
                let mut perms = fs::metadata(&path)
                    .map_err(|e| format!("metadata {script}: {e}"))?
                    .permissions();
                perms.set_mode(0o755);
                fs::set_permissions(&path, perms).map_err(|e| format!("chmod {script}: {e}"))?;
            }
        }
    }

    println!(
        "wf-rules project created at {} (scope: {scope:?})",
        root.canonicalize().unwrap_or(root.to_path_buf()).display()
    );
    println!(
        "  cd {} && wfusion daemon --config conf/wfusion.toml",
        project_dir
    );

    // 3. Ensure a bearer token exists for the admin API. The default config
    //    points admin_api.auth.token_file at $HOME/.warp_fusion/admin_api.token;
    //    generate it (owner-only) if missing so `wfusion daemon` can start
    //    with admin_api enabled out of the box.
    match ensure_admin_api_token() {
        Ok(path) => {
            println!("  admin api token: {}", path.display());
        }
        Err(e) => {
            // Token generation is best-effort: a missing token only prevents
            // admin_api from starting, not the project itself.
            println!("  warning: could not generate admin api token: {e}");
        }
    }

    Ok(())
}

/// Resolve the default admin API token path: `$HOME/.warp_fusion/admin_api.token`.
/// Returns None if `$HOME` is not set.
fn default_token_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join(DEFAULT_TOKEN_DIR)
            .join(DEFAULT_TOKEN_FILE)
    })
}

/// Ensure the default admin API token file exists. If it does, leave it
/// untouched (so multiple projects share one token); otherwise generate a
/// fresh random token with owner-only permissions (0o600 on Unix).
fn ensure_admin_api_token() -> Result<PathBuf, String> {
    let path = default_token_path()
        .ok_or_else(|| "$HOME is not set; cannot resolve admin api token path".to_string())?;

    if path.exists() {
        return Ok(path);
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }

    // 32 random bytes → 64 hex chars.
    let mut raw = [0u8; 32];
    rand::rng().fill_bytes(&mut raw);
    let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    fs::write(&path, format!("{token}\n"))
        .map_err(|e| format!("write token {}: {e}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&path)
            .map_err(|e| format!("stat token {}: {e}", path.display()))?
            .permissions();
        perms.set_mode(0o600);
        fs::set_permissions(&path, perms)
            .map_err(|e| format!("chmod token {}: {e}", path.display()))?;
    }

    Ok(path)
}

// =====================================================================
// Remote bootstrap (`init --repo`)
// =====================================================================

/// Bootstrap a project from a remote git repo: build a local skeleton, then
/// sync managed dirs (conf/models/topology/connectors) from the remote repo
/// at the requested version via `conf update`. Mirrors wparse `wproj init
/// --repo` (WarpProject::init + run_conf_update_from_repo).
pub fn init_from_remote(
    project_dir: &str,
    repo_url: &str,
    version: Option<&str>,
) -> Result<(), String> {
    if repo_url.trim().is_empty() {
        return Err("remote bootstrap requires a non-empty --repo URL".to_string());
    }

    let project_name = std::path::Path::new(project_dir)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "wf-rules".to_string());

    // 1. Build a local skeleton (conf/topology/connectors/models + .run +
    //    admin token). Managed dirs will be replaced by the remote sync.
    init_project(project_dir, &project_name, "normal")?;

    // 2. Sync managed dirs from the remote repo at the requested version,
    //    validate, and roll back on failure.
    crate::conf::run_conf_update_from_repo(std::path::Path::new(project_dir), repo_url, version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "wfadm_test_{}_{}",
            std::process::id(),
            rand::random::<u32>()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn init_rules_creates_expected_dirs() {
        let dir = temp_dir();
        init_project(dir.to_str().unwrap(), "test", "rules").expect("init rules");
        assert!(dir.join("conf/wfusion.toml").exists());
        assert!(dir.join("models/rules").is_dir());
        assert!(dir.join("models/schemas").is_dir());
        assert!(dir.join("models/scenarios").is_dir());
        assert!(dir.join("smoke.sh").exists());
        // Rules scope should NOT have topology
        assert!(!dir.join("topology").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn init_conf_creates_expected_dirs() {
        let dir = temp_dir();
        init_project(dir.to_str().unwrap(), "test", "conf").expect("init conf");
        assert!(dir.join("conf/wfusion.toml").exists());
        assert!(dir.join("topology/sinks").is_dir());
        assert!(dir.join("topology/sources").is_dir());
        // Conf scope needs the external window config referenced by
        // conf/wfusion.toml, but should not include rules/scenarios.
        assert!(dir.join("models/schemas/windows.toml").exists());
        assert!(!dir.join("models/rules").exists());
        assert!(!dir.join("models/scenarios").exists());
        assert!(!dir.join("models/schemas/auth.wfs").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn init_rejects_nonempty_dir() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("existing.txt"), b"hello").unwrap();
        let err = init_project(dir.to_str().unwrap(), "test", "normal").unwrap_err();
        assert!(err.contains("already exists"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn init_invalid_scope() {
        let dir = temp_dir();
        let err = init_project(dir.to_str().unwrap(), "test", "bad").unwrap_err();
        assert!(err.contains("invalid scope"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
