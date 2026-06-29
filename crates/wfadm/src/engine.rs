//! wfadm engine — manage the running wfusion engine via admin API

use std::path::{Path, PathBuf};

use clap::Subcommand;

// ── CLI subcommands ────────────────────────────────────────────────────

#[derive(Subcommand, Clone)]
pub enum EngineCommands {
    /// Query engine runtime status
    Status {
        #[arg(short, long, default_value = "conf/wfusion.toml")]
        config: PathBuf,
        #[arg(long)]
        admin_url: Option<String>,
        #[arg(long)]
        token_file: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Trigger model reload (TODO: not yet implemented in engine)
    Reload {
        #[arg(short, long, default_value = "conf/wfusion.toml")]
        config: PathBuf,
        #[arg(long)]
        admin_url: Option<String>,
        #[arg(long)]
        token_file: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
}

// ── Runner ────────────────────────────────────────────────────────────

pub fn run(command: EngineCommands) -> Result<(), String> {
    match command {
        EngineCommands::Status {
            config,
            admin_url,
            token_file,
            json,
        } => cmd_status(&config, admin_url.as_deref(), token_file.as_deref(), json),
        EngineCommands::Reload {
            config,
            admin_url,
            token_file,
            json,
        } => cmd_reload(&config, admin_url.as_deref(), token_file.as_deref(), json),
    }
}

// ── Helpers ───────────────────────────────────────────────────────────

struct AdminApiTarget {
    base_url: String,
    token: String,
}

fn resolve_target(
    config_path: &Path,
    admin_url: Option<&str>,
    token_file: Option<&Path>,
) -> Result<AdminApiTarget, String> {
    // If admin_url and token_file are explicitly provided, use them
    if let (Some(url), Some(tf)) = (admin_url, token_file) {
        let token = std::fs::read_to_string(tf)
            .map_err(|e| format!("read token file '{}': {e}", tf.display()))?
            .trim()
            .to_string();
        if token.is_empty() {
            return Err(format!("token file '{}' is empty", tf.display()));
        }
        return Ok(AdminApiTarget {
            base_url: url.trim_end_matches('/').to_string(),
            token,
        });
    }

    // Otherwise, read from config file
    let content = std::fs::read_to_string(config_path)
        .map_err(|e| format!("read config '{}': {e}", config_path.display()))?;
    let val: toml::Value = content
        .parse()
        .map_err(|e| format!("parse config TOML: {e}"))?;

    let admin_api = val
        .get("admin_api")
        .ok_or_else(|| "admin_api section not found in config (is it enabled?)".to_string())?;

    let enabled = admin_api
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !enabled {
        return Err("admin_api is not enabled in config".to_string());
    }

    let bind = admin_api
        .get("bind")
        .and_then(|v| v.as_str())
        .unwrap_or("127.0.0.1:19090");
    let base_url = if admin_url.is_none_or(|u| u.is_empty()) {
        format!("http://{bind}")
    } else {
        admin_url.unwrap().trim_end_matches('/').to_string()
    };

    let token_path = admin_api
        .get("auth")
        .and_then(|a| a.get("token_file"))
        .and_then(|v| v.as_str())
        .unwrap_or("${HOME}/.wfusion/admin_api.token");

    // Expand ${HOME} in token path
    let token_path = token_path.replace("${HOME}", &std::env::var("HOME").unwrap_or_default());
    let token = std::fs::read_to_string(&token_path)
        .map_err(|e| format!("read token file '{}': {e}", token_path))?
        .trim()
        .to_string();
    if token.is_empty() {
        return Err(format!("token file '{}' is empty", token_path));
    }

    Ok(AdminApiTarget { base_url, token })
}

// ── Status ────────────────────────────────────────────────────────────

fn cmd_status(
    config_path: &Path,
    admin_url: Option<&str>,
    token_file: Option<&Path>,
    json: bool,
) -> Result<(), String> {
    let target = resolve_target(config_path, admin_url, token_file)?;
    let url = format!("{}/admin/v1/runtime/status", target.base_url);

    let resp = ureq::get(&url)
        .header("Authorization", &format!("Bearer {}", target.token))
        .header("Accept", "application/json")
        .call()
        .map_err(|e| format!("request failed: {e}"))?;

    let status = resp.status();
    let body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("read response: {e}"))?;

    if status != 200 {
        return Err(format!("HTTP {status}: {body}"));
    }

    if json {
        println!("{body}");
        return Ok(());
    }

    // Parse and display nicely
    let val: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("parse response JSON: {e}"))?;

    println!("Engine status");
    println!("  Endpoint  : {}", target.base_url);
    if let Some(id) = val.get("instance_id").and_then(|v| v.as_str()) {
        println!("  Instance  : {id}");
    }
    if let Some(ver) = val.get("version").and_then(|v| v.as_str()) {
        println!("  Version   : {ver}");
    }
    if let Some(acc) = val.get("accepting").and_then(|v| v.as_bool()) {
        println!("  Accepting : {acc}");
    }
    Ok(())
}

// ── Reload (stub) ─────────────────────────────────────────────────────

fn cmd_reload(
    config_path: &Path,
    admin_url: Option<&str>,
    token_file: Option<&Path>,
    _json: bool,
) -> Result<(), String> {
    let _target = resolve_target(config_path, admin_url, token_file)?;
    Err("engine reload not yet implemented (engine admin API reload endpoint pending)".to_string())
}
