//! Admin API server — minimal HTTP API for engine status.
//!
//! Protected by bearer token authentication.

use std::convert::Infallible;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Bytes;
use hyper::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as AutoBuilder;
use rustls::ServerConfig;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_rustls::TlsAcceptor;
use uuid::Uuid;
use wf_config::{
    AdminApiConf, ConfigVarContext, FusionConfig, FusionConfigLoader, FusionMode, MetricsConfig,
    RawFusionConfigTree, project_remote::ProjectRemoteConf,
};
use wf_project_remote::{
    ProjectRemoteLockGuard, ProjectRemoteMode, ProjectRemoteSnapshot, ProjectRemoteUpdateResult,
    ProjectRuntimeArtifactSnapshot, RemoteGroup,
};
use wf_runtime::lifecycle::{ReloadOutcome, RuntimeControlHandle};

const DEFAULT_AUTH_MODE: &str = "bearer_token";

// ── AdminApiRuntime ───────────────────────────────────────────────────

#[derive(Debug)]
#[allow(dead_code)]
pub struct AdminApiRuntime {
    local_addr: SocketAddr,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl AdminApiRuntime {
    #[allow(dead_code)]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }
    #[allow(dead_code)]
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        let _ = self.task.await;
    }
}

// ── AppState ──────────────────────────────────────────────────────────

struct AppState {
    bearer_token: String,
    instance_id: String,
    version: String,
    /// Handle to the running Reactor: drives reload requests and exposes the
    /// root cancellation token (for the `accepting` status field). Replaces the
    /// scheme-A bare `CancellationToken`.
    control: RuntimeControlHandle,
    /// The exact config source the engine booted from, so a reload re-reads
    /// the *same* `--config` path + `--overlay` files with the *same* `--var`
    /// context — not a hard-coded `wfusion.toml` with empty vars.
    config_source: ReloadConfigSource,
    /// True while a reload (and optional remote sync) is in flight. Exposed via
    /// `/runtime/status` as `reloading`, so callers can tell a reload is
    /// ongoing even though `accepting` stays true.
    reloading: Arc<AtomicBool>,
    reload_gate: Mutex<()>,
    reload_state: Mutex<ReloadState>,
    request_timeout: Duration,
    max_body_bytes: usize,
    work_root: PathBuf,
}

#[derive(Debug, Default)]
struct ReloadState {
    current_request_id: Option<String>,
    last_reload_request_id: Option<String>,
    last_reload_result: Option<&'static str>,
    last_reload_started_at: Option<SystemTime>,
    last_reload_finished_at: Option<SystemTime>,
}

struct ProjectRemoteReloadContext {
    _lock_guard: ProjectRemoteLockGuard,
    snapshot: ProjectRemoteSnapshot,
    runtime_snapshot: ProjectRuntimeArtifactSnapshot,
    update_result: Option<ProjectRemoteUpdateResult>,
}

/// What `POST /admin/v1/reloads/model` re-reads on each request. Captured at
/// boot so reloads honour the original `--config` / `--overlay` / `--var`
/// rather than guessing `wfusion.toml` in the work root.
///
/// It also carries the CLI overrides (`--mode`, `--metrics*`) that were
/// applied to the *effective* config at boot. A reload re-applies them to the
/// freshly-loaded `next_config`, otherwise `prepare_reload` would see a
/// spurious diff (e.g. `mode` changing) and wrongly return `Blocked`.
#[derive(Clone)]
pub struct ReloadConfigSource {
    config_path: PathBuf,
    overlay_paths: Vec<PathBuf>,
    config_ctx: ConfigVarContext,
    /// Work dir passed to the loader (resolves relative paths in config). The
    /// engine's runtime base dir (project root for `conf/wfusion.toml`, config
    /// file's parent otherwise, or explicit `--work-dir`).
    work_dir: PathBuf,
    /// CLI override for `mode`, if the engine was launched via a subcommand
    /// (`daemon`/`batch`) that differs from the TOML value.
    mode_override: Option<FusionMode>,
    /// CLI overrides for metrics, if any `--metrics*` flag was passed.
    metrics_override: Option<MetricsConfig>,
}

impl ReloadConfigSource {
    /// Capture the boot-time config source for later reloads.
    pub fn new(
        config_path: PathBuf,
        overlay_paths: Vec<PathBuf>,
        config_ctx: ConfigVarContext,
        work_dir: PathBuf,
    ) -> Self {
        Self {
            config_path,
            overlay_paths,
            config_ctx,
            work_dir,
            mode_override: None,
            metrics_override: None,
        }
    }

    /// Record the CLI `--mode` override applied at boot, so reloads re-apply it.
    pub fn with_mode_override(mut self, mode: FusionMode) -> Self {
        self.mode_override = Some(mode);
        self
    }

    /// Record the CLI `--metrics*` overrides applied at boot, so reloads
    /// re-apply them.
    pub fn with_metrics_override(mut self, metrics: MetricsConfig) -> Self {
        self.metrics_override = Some(metrics);
        self
    }

    /// Re-apply the captured CLI overrides to a freshly-loaded config, so that
    /// `prepare_reload` compares apples to apples with the running engine.
    fn apply_overrides(&self, config: &mut FusionConfig) {
        if let Some(mode) = self.mode_override {
            config.mode = mode;
        }
        if let Some(metrics) = &self.metrics_override {
            config.metrics = metrics.clone();
        }
    }
}

// ── Start ─────────────────────────────────────────────────────────────

fn conf_err(detail: impl Into<String>) -> String {
    detail.into()
}

pub async fn start_if_enabled(
    work_root: &Path,
    config: &AdminApiConf,
    control: RuntimeControlHandle,
    config_source: ReloadConfigSource,
) -> Result<Option<AdminApiRuntime>, String> {
    if !config.enabled {
        return Ok(None);
    }

    let bind: SocketAddr = config
        .bind
        .parse()
        .map_err(|e| conf_err(format!("invalid admin_api.bind \"{}\": {e}", config.bind)))?;
    if config.max_body_bytes == 0 {
        return Err(conf_err("admin_api.max_body_bytes must be > 0"));
    }
    let auth_mode = config.auth.mode.trim().to_ascii_lowercase();
    if auth_mode != DEFAULT_AUTH_MODE {
        return Err(conf_err(format!(
            "unsupported admin_api.auth.mode '{}', expected '{}'",
            config.auth.mode, DEFAULT_AUTH_MODE
        )));
    }

    // Resolve TLS before binding so certificate/key errors are reported before
    // the admin listener is exposed.
    let tls = if config.tls.enabled {
        Some(load_tls_config(
            &resolve_path(work_root, &config.tls.cert_file),
            &resolve_path(work_root, &config.tls.key_file),
        )?)
    } else {
        None
    };

    let listener = TcpListener::bind(bind)
        .await
        .map_err(|e| conf_err(format!("bind admin api on {bind}: {e}")))?;

    let local_addr = listener
        .local_addr()
        .map_err(|e| conf_err(format!("read admin api local addr: {e}")))?;

    let token_path = work_root.join(&config.auth.token_file);
    validate_token_file(&token_path)?;
    let bearer_token = fs::read_to_string(&token_path)
        .map_err(|e| conf_err(format!("read token file {}: {e}", token_path.display())))?
        .trim()
        .to_string();

    if bearer_token.is_empty() {
        return Err(conf_err("admin_api token file is empty"));
    }
    if !bind.ip().is_loopback() && tls.is_none() {
        return Err(conf_err(format!(
            "non-loopback admin_api.bind '{}' requires admin_api.tls.enabled=true",
            bind
        )));
    }

    let instance_id = format!("fusion:{}", std::process::id());

    let state = Arc::new(AppState {
        bearer_token,
        instance_id,
        version: env!("CARGO_PKG_VERSION").to_string(),
        control,
        config_source,
        reloading: Arc::new(AtomicBool::new(false)),
        reload_gate: Mutex::new(()),
        reload_state: Mutex::new(ReloadState::default()),
        request_timeout: Duration::from_millis(config.request_timeout_ms),
        max_body_bytes: config.max_body_bytes,
        work_root: work_root.to_path_buf(),
    });

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task = match tls {
        Some(server_config) => {
            tracing::info!(
                domain = "sys",
                "admin api listening on https://{}",
                local_addr
            );
            tokio::spawn(run_tls(
                listener,
                TlsAcceptor::from(Arc::new(server_config)),
                state,
                shutdown_rx,
            ))
        }
        None => {
            tracing::info!(
                domain = "sys",
                "admin api listening on http://{}",
                local_addr
            );
            tokio::spawn(run_plain(listener, state, shutdown_rx))
        }
    };

    Ok(Some(AdminApiRuntime {
        local_addr,
        shutdown_tx: Some(shutdown_tx),
        task,
    }))
}

/// Resolve a config path: absolute paths are kept as-is, relative paths are
/// joined against `work_root` (matching wparse's conf_absolutize behavior).
fn resolve_path(work_root: &Path, p: &str) -> PathBuf {
    let path = PathBuf::from(p);
    if path.is_absolute() {
        path
    } else {
        work_root.join(path)
    }
}

/// Build a rustls `ServerConfig` from PEM-encoded cert/key files.
fn load_tls_config(cert_path: &Path, key_path: &Path) -> Result<ServerConfig, String> {
    install_tls_crypto_provider()?;
    if cert_path.as_os_str().is_empty() || key_path.as_os_str().is_empty() {
        return Err(conf_err(
            "admin_api.tls.cert_file and admin_api.tls.key_file must be set when TLS is enabled",
        ));
    }
    let cert_pem = fs::read(cert_path)
        .map_err(|e| conf_err(format!("read cert file {}: {e}", cert_path.display())))?;
    let key_pem = fs::read(key_path)
        .map_err(|e| conf_err(format!("read key file {}: {e}", key_path.display())))?;

    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(&cert_pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| conf_err(format!("parse PEM certs from {}: {e}", cert_path.display())))?;
    if certs.is_empty() {
        return Err(conf_err(format!(
            "no certificates found in {}",
            cert_path.display()
        )));
    }
    let key = PrivateKeyDer::from_pem_slice(&key_pem)
        .map_err(|e| conf_err(format!("parse PEM key from {}: {e}", key_path.display())))?;

    let mut server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| conf_err(format!("build TLS server config: {e}")))?;
    server_config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(server_config)
}

fn install_tls_crypto_provider() -> Result<(), String> {
    if rustls::crypto::CryptoProvider::get_default().is_some() {
        return Ok(());
    }
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
    if rustls::crypto::CryptoProvider::get_default().is_some() {
        Ok(())
    } else {
        Err(conf_err("install rustls ring crypto provider"))
    }
}

/// Validate the bearer token file: must be a regular file, and on Unix its
/// permissions must be owner-only (group/other bits clear). Mirrors wparse's
/// `validate_token_file`.
fn validate_token_file(path: &Path) -> Result<(), String> {
    let meta = fs::metadata(path)
        .map_err(|e| conf_err(format!("stat token file {}: {e}", path.display())))?;
    if !meta.is_file() {
        return Err(conf_err(format!(
            "token file {} is not a regular file",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(conf_err(format!(
                "token file {} permissions {:o} are too permissive; require owner-only access",
                path.display(),
                mode
            )));
        }
    }
    Ok(())
}

// ── Server ────────────────────────────────────────────────────────────

async fn run_plain(
    listener: TcpListener,
    state: Arc<AppState>,
    shutdown_rx: oneshot::Receiver<()>,
) {
    run_accept_loop(listener, state, shutdown_rx, None).await;
}

async fn run_tls(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    state: Arc<AppState>,
    shutdown_rx: oneshot::Receiver<()>,
) {
    run_accept_loop(listener, state, shutdown_rx, Some(acceptor)).await;
}

async fn run_accept_loop(
    listener: TcpListener,
    state: Arc<AppState>,
    mut shutdown_rx: oneshot::Receiver<()>,
    tls_acceptor: Option<TlsAcceptor>,
) {
    loop {
        tokio::select! {
            _ = &mut shutdown_rx => {
                tracing::info!(domain = "sys", "admin api shutting down");
                break;
            }
            accept_res = listener.accept() => {
                let (stream, remote_addr) = match accept_res {
                    Ok(pair) => pair,
                    Err(e) => {
                        tracing::warn!(domain = "sys", "admin api accept error: {e}");
                        continue;
                    }
                };
                let state = state.clone();
                let tls_acceptor = tls_acceptor.clone();
                tokio::spawn(async move {
                    if let Some(acceptor) = tls_acceptor {
                        match acceptor.accept(stream).await {
                            Ok(tls_stream) => {
                                serve_connection(tls_stream, remote_addr, state).await
                            }
                            Err(err) => {
                                tracing::warn!(
                                    domain = "sys",
                                    "admin api TLS handshake failed from {}: {err}",
                                    remote_addr
                                );
                            }
                        }
                    } else {
                        serve_connection(stream, remote_addr, state).await;
                    }
                });
            }
        }
    }
}

async fn serve_connection<IO>(stream: IO, remote_addr: SocketAddr, state: Arc<AppState>)
where
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let io = TokioIo::new(stream);
    let svc = service_fn(move |req| handle_request(req, remote_addr, state.clone()));
    if let Err(err) = AutoBuilder::new(TokioExecutor::new())
        .serve_connection_with_upgrades(io, svc)
        .await
    {
        tracing::warn!(domain = "sys", "admin api connection error: {err}");
    }
}

// ── Request handling ──────────────────────────────────────────────────

async fn handle_request(
    req: Request<hyper::body::Incoming>,
    remote_addr: SocketAddr,
    state: Arc<AppState>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let request_id = request_id(req.headers());
    let path = req.uri().path().to_string();
    let method = req.method().clone();

    if !authorized(req.headers(), &state.bearer_token) {
        return Ok(json_response(
            StatusCode::UNAUTHORIZED,
            &ErrorResponse {
                request_id,
                accepted: false,
                result: "unauthorized",
                error: "invalid bearer token".to_string(),
            },
        ));
    }

    match (method.clone(), path.as_str()) {
        (Method::GET, "/admin/v1/runtime/status") => {
            Ok(status_response(&request_id, remote_addr, &state).await)
        }
        (Method::POST, "/admin/v1/reloads/model") => {
            Ok(handle_reload(req, request_id, remote_addr, state).await)
        }
        _ => Ok(json_response(
            StatusCode::NOT_FOUND,
            &ErrorResponse {
                request_id,
                accepted: false,
                result: "not_found",
                error: format!("unsupported route {}", path),
            },
        )),
    }
}

#[derive(Serialize)]
struct RuntimeStatusResponse {
    instance_id: String,
    version: String,
    project_version: Option<serde_json::Value>,
    accepting_commands: bool,
    reloading: bool,
    current_request_id: Option<String>,
    last_reload_request_id: Option<String>,
    last_reload_result: Option<&'static str>,
    last_reload_started_at: Option<String>,
    last_reload_finished_at: Option<String>,
}

#[derive(Serialize)]
struct ErrorResponse {
    request_id: String,
    accepted: bool,
    result: &'static str,
    error: String,
}

#[derive(Serialize)]
struct ReloadResponse {
    request_id: String,
    accepted: bool,
    result: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    update: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    requested_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolved_tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    force_replaced: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    warning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

async fn status_response(
    request_id: &str,
    remote_addr: SocketAddr,
    state: &AppState,
) -> Response<Full<Bytes>> {
    let accepting_commands = !state.control.cancel_token().is_cancelled();
    let reloading = state.reloading.load(Ordering::Relaxed);
    let project_version = match read_project_version(&state.work_root) {
        Ok(version) => version,
        Err(err) => {
            tracing::warn!(
                domain = "sys",
                "admin api status project version read failed request_id={} remote={} error={}",
                request_id,
                remote_addr,
                err
            );
            None
        }
    };
    let reload_state = state.reload_state.lock().await;
    tracing::info!(
        domain = "sys",
        "admin api status request_id={} remote={} accepting={} reloading={}",
        request_id,
        remote_addr,
        accepting_commands,
        reloading
    );
    json_response(
        StatusCode::OK,
        &RuntimeStatusResponse {
            instance_id: state.instance_id.clone(),
            version: state.version.clone(),
            project_version,
            accepting_commands,
            reloading,
            current_request_id: reload_state.current_request_id.clone(),
            last_reload_request_id: reload_state.last_reload_request_id.clone(),
            last_reload_result: reload_state.last_reload_result,
            last_reload_started_at: reload_state
                .last_reload_started_at
                .map(system_time_to_rfc3339),
            last_reload_finished_at: reload_state
                .last_reload_finished_at
                .map(system_time_to_rfc3339),
        },
    )
}

/// `POST /admin/v1/reloads/model` — publish/update project content and reload
/// the running rule set.
async fn handle_reload(
    req: Request<hyper::body::Incoming>,
    request_id: String,
    remote_addr: SocketAddr,
    state: Arc<AppState>,
) -> Response<Full<Bytes>> {
    let _reload_guard = match state.reload_gate.try_lock() {
        Ok(guard) => guard,
        Err(_) => {
            return json_response(
                StatusCode::CONFLICT,
                &ReloadResponse {
                    request_id,
                    accepted: false,
                    result: "reload_in_progress",
                    update: None,
                    requested_version: None,
                    current_version: None,
                    resolved_tag: None,
                    group: None,
                    force_replaced: None,
                    warning: None,
                    error: None,
                },
            );
        }
    };

    let reload_req =
        match read_json_body::<ReloadRequest>(req.into_body(), state.max_body_bytes).await {
            Ok(payload) => payload,
            Err(ReadBodyError::TooLarge(limit)) => {
                return json_response(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    &ErrorResponse {
                        request_id,
                        accepted: false,
                        result: "payload_too_large",
                        error: format!("request body exceeds {} bytes", limit),
                    },
                );
            }
            Err(ReadBodyError::InvalidJson(err)) | Err(ReadBodyError::Read(err)) => {
                return json_response(
                    StatusCode::BAD_REQUEST,
                    &ErrorResponse {
                        request_id,
                        accepted: false,
                        result: "invalid_request",
                        error: err,
                    },
                );
            }
        };

    let reason = reload_req.reason.as_deref().unwrap_or("");
    if !reload_req.update && reload_req.version.is_some() {
        return json_response(
            StatusCode::BAD_REQUEST,
            &ErrorResponse {
                request_id,
                accepted: false,
                result: "invalid_request",
                error: "version requires update=true".to_string(),
            },
        );
    }
    if !reload_req.update && reload_req.group.as_deref().is_some_and(|g| !g.is_empty()) {
        return json_response(
            StatusCode::BAD_REQUEST,
            &ErrorResponse {
                request_id,
                accepted: false,
                result: "invalid_request",
                error: "group requires update=true".to_string(),
            },
        );
    }
    let update_group = match reload_req.group.as_deref() {
        None | Some("") => None,
        Some(raw) => match raw.parse::<RemoteGroup>() {
            Ok(group) => Some(group),
            Err(err) => {
                return json_response(
                    StatusCode::BAD_REQUEST,
                    &ErrorResponse {
                        request_id,
                        accepted: false,
                        result: "invalid_request",
                        error: err,
                    },
                );
            }
        },
    };

    let remote_conf = if reload_req.update {
        match load_remote_conf(&state.config_source) {
            Ok(remote_conf) => {
                if !remote_conf.enabled {
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &ErrorResponse {
                            request_id,
                            accepted: false,
                            result: "update_failed",
                            error: "update requested but [project_remote] is disabled in config"
                                .to_string(),
                        },
                    );
                }
                match wf_project_remote::resolve_project_remote_mode(&remote_conf) {
                    Ok(ProjectRemoteMode::Dual { .. }) if update_group.is_none() => {
                        return json_response(
                            StatusCode::BAD_REQUEST,
                            &ErrorResponse {
                                request_id,
                                accepted: false,
                                result: "invalid_request",
                                error:
                                    "dual-repo mode requires group (models|infra) with update=true"
                                        .to_string(),
                            },
                        );
                    }
                    Ok(_) => Some(remote_conf),
                    Err(err) => {
                        return json_response(
                            StatusCode::BAD_REQUEST,
                            &ErrorResponse {
                                request_id,
                                accepted: false,
                                result: "invalid_request",
                                error: err,
                            },
                        );
                    }
                }
            }
            Err(err) => {
                return json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &ErrorResponse {
                        request_id,
                        accepted: false,
                        result: "update_failed",
                        error: err,
                    },
                );
            }
        }
    } else {
        None
    };

    if state.control.cancel_token().is_cancelled() {
        return json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &ErrorResponse {
                request_id,
                accepted: false,
                result: "runtime_not_ready",
                error: "runtime command receiver not ready".to_string(),
            },
        );
    }
    if state.reloading.load(Ordering::Relaxed) {
        return json_response(
            StatusCode::CONFLICT,
            &ReloadResponse {
                request_id,
                accepted: false,
                result: "reload_in_progress",
                update: None,
                requested_version: None,
                current_version: None,
                resolved_tag: None,
                group: None,
                force_replaced: None,
                warning: None,
                error: None,
            },
        );
    }

    tracing::info!(
        domain = "sys",
        "admin api reload request_id={} remote={} wait={} update={} version={} group={} reason={}",
        request_id,
        remote_addr,
        reload_req.wait,
        reload_req.update,
        reload_req.version.as_deref().unwrap_or("(auto)"),
        reload_req.group.as_deref().unwrap_or("-"),
        reason
    );

    mark_reload_started(&state, &request_id).await;

    let src = &state.config_source;
    let lock_guard = match wf_project_remote::acquire_project_remote_lock(&src.work_dir) {
        Ok(lock_guard) => lock_guard,
        Err(err) => {
            mark_reload_finished(&state, &request_id, "update_in_progress").await;
            return json_response(
                StatusCode::CONFLICT,
                &ReloadResponse {
                    request_id,
                    accepted: false,
                    result: "update_in_progress",
                    update: Some(reload_req.update),
                    requested_version: reload_req.version.clone(),
                    current_version: None,
                    resolved_tag: None,
                    group: reload_req.group.clone(),
                    force_replaced: None,
                    warning: None,
                    error: Some(err),
                },
            );
        }
    };
    let snapshot = match wf_project_remote::capture_project_remote_snapshot_with_group(
        &src.work_dir,
        update_group,
    ) {
        Ok(snapshot) => snapshot,
        Err(err) => {
            mark_reload_finished(&state, &request_id, "update_failed").await;
            return json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &ErrorResponse {
                    request_id,
                    accepted: false,
                    result: "update_failed",
                    error: err,
                },
            );
        }
    };
    let runtime_snapshot = match wf_project_remote::capture_runtime_artifact_snapshot(&src.work_dir)
    {
        Ok(snapshot) => snapshot,
        Err(err) => {
            mark_reload_finished(&state, &request_id, "update_failed").await;
            return json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &ErrorResponse {
                    request_id,
                    accepted: false,
                    result: "update_failed",
                    error: err,
                },
            );
        }
    };

    let update_result = if let Some(remote_conf) = remote_conf {
        let update_result = match run_remote_sync(
            src,
            &remote_conf,
            reload_req.version.as_deref(),
            update_group,
            &lock_guard,
            &snapshot,
        ) {
            Ok(result) => {
                tracing::info!(
                    domain = "sys",
                    "admin api reload update sync done request_id={} current_version={} resolved_tag={}",
                    request_id,
                    result.current_version,
                    result.resolved_tag
                );
                result
            }
            Err(err) => {
                tracing::warn!(
                    domain = "sys",
                    "admin api reload update sync failed request_id={}: {err}",
                    request_id
                );
                mark_reload_finished(&state, &request_id, "update_failed").await;
                return json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &ErrorResponse {
                        request_id,
                        accepted: false,
                        result: "update_failed",
                        error: err,
                    },
                );
            }
        };
        Some(update_result)
    } else {
        None
    };
    let reload_ctx = Some(ProjectRemoteReloadContext {
        _lock_guard: lock_guard,
        snapshot,
        runtime_snapshot,
        update_result,
    });
    let update_result = reload_ctx
        .as_ref()
        .and_then(|ctx| ctx.update_result.as_ref())
        .cloned();

    let loader = FusionConfigLoader::new(
        &src.config_path,
        &src.overlay_paths,
        &src.config_ctx,
        Some(&src.work_dir),
    );
    let (next_raw, mut next_config): (RawFusionConfigTree, FusionConfig) =
        match (loader.load_raw(), loader.load()) {
            (Ok(raw), Ok(config)) => (raw, config),
            (Err(err), _) | (_, Err(err)) => {
                let msg = err.to_string();
                tracing::warn!(domain = "sys", "reload config load failed: {msg}");
                mark_reload_finished(&state, &request_id, "reload_failed").await;
                let rollback_warning = rollback_updated_project(
                    &state.config_source.work_dir,
                    reload_ctx.as_ref(),
                    &request_id,
                    remote_addr,
                    "config_load_failed",
                );
                return json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &ReloadResponse {
                        request_id,
                        accepted: false,
                        result: "reload_failed",
                        update: Some(reload_req.update),
                        requested_version: update_result
                            .as_ref()
                            .and_then(|result| result.requested_version.clone()),
                        current_version: update_result
                            .as_ref()
                            .map(|result| result.current_version.clone()),
                        resolved_tag: update_result
                            .as_ref()
                            .map(|result| result.resolved_tag.clone()),
                        group: update_result
                            .as_ref()
                            .and_then(|result| result.group.clone()),
                        force_replaced: None,
                        warning: rollback_warning,
                        error: Some(format!("failed to load config: {msg}")),
                    },
                );
            }
        };
    state.config_source.apply_overrides(&mut next_config);

    let reload_state = state.clone();
    let reload_request_id = request_id.clone();
    let reload_reason = reason.to_string();
    let reload_control = state.control.clone();
    let mut task =
        tokio::spawn(async move { reload_control.apply_reload(next_raw, next_config).await });

    if reload_req.wait {
        let wait_timeout = Duration::from_millis(
            reload_req
                .timeout_ms
                .unwrap_or(state.request_timeout.as_millis() as u64),
        );
        match timeout(wait_timeout, &mut task).await {
            Ok(Ok(result)) => {
                return map_reload_result(
                    result,
                    reload_state,
                    reload_request_id,
                    remote_addr,
                    reload_reason,
                    reload_ctx,
                )
                .await;
            }
            Ok(Err(err)) => {
                tracing::warn!(
                    domain = "sys",
                    "admin api reload task failed request_id={} remote={} error={}",
                    request_id,
                    remote_addr,
                    err
                );
                mark_reload_finished(&state, &request_id, "reload_failed").await;
                let rollback_warning = rollback_updated_project(
                    &state.config_source.work_dir,
                    reload_ctx.as_ref(),
                    &request_id,
                    remote_addr,
                    "reload_task_join_failed",
                );
                return json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &ReloadResponse {
                        request_id,
                        accepted: true,
                        result: "reload_failed",
                        update: Some(reload_req.update),
                        requested_version: update_result
                            .as_ref()
                            .and_then(|result| result.requested_version.clone()),
                        current_version: update_result
                            .as_ref()
                            .map(|result| result.current_version.clone()),
                        resolved_tag: update_result
                            .as_ref()
                            .map(|result| result.resolved_tag.clone()),
                        group: update_result
                            .as_ref()
                            .and_then(|result| result.group.clone()),
                        force_replaced: None,
                        warning: rollback_warning,
                        error: Some("runtime reload task failed".to_string()),
                    },
                );
            }
            Err(_) => {
                tracing::info!(
                    domain = "sys",
                    "admin api reload still running request_id={} remote={} timeout_ms={} reason={}",
                    request_id,
                    remote_addr,
                    wait_timeout.as_millis(),
                    reason
                );
                tokio::spawn(monitor_reload_task(
                    task,
                    state.clone(),
                    request_id.clone(),
                    remote_addr,
                    reason.to_string(),
                    reload_ctx,
                ));
                return running_response(request_id, reload_req.update, update_result.as_ref());
            }
        }
    }

    tokio::spawn(monitor_reload_task(
        task,
        state.clone(),
        request_id.clone(),
        remote_addr,
        reason.to_string(),
        reload_ctx,
    ));
    running_response(request_id, reload_req.update, update_result.as_ref())
}

fn running_response(
    request_id: String,
    update: bool,
    update_result: Option<&ProjectRemoteUpdateResult>,
) -> Response<Full<Bytes>> {
    json_response(
        StatusCode::ACCEPTED,
        &ReloadResponse {
            request_id,
            accepted: true,
            result: "running",
            update: Some(update),
            requested_version: update_result.and_then(|result| result.requested_version.clone()),
            current_version: update_result.map(|result| result.current_version.clone()),
            resolved_tag: update_result.map(|result| result.resolved_tag.clone()),
            group: update_result.and_then(|result| result.group.clone()),
            force_replaced: None,
            warning: None,
            error: None,
        },
    )
}

async fn monitor_reload_task(
    task: JoinHandle<wf_runtime::error::RuntimeResult<ReloadOutcome>>,
    state: Arc<AppState>,
    request_id: String,
    remote_addr: SocketAddr,
    reason: String,
    reload_ctx: Option<ProjectRemoteReloadContext>,
) {
    match task.await {
        Ok(result) => {
            let response = map_reload_result(
                result,
                state,
                request_id.clone(),
                remote_addr,
                reason.clone(),
                reload_ctx,
            )
            .await;
            tracing::info!(
                domain = "sys",
                "admin api background reload finished request_id={} remote={} status={} reason={}",
                request_id,
                remote_addr,
                response.status(),
                reason
            );
        }
        Err(err) => {
            tracing::warn!(
                domain = "sys",
                "admin api background reload task failed request_id={} remote={} reason={} error={}",
                request_id,
                remote_addr,
                reason,
                err
            );
            mark_reload_finished(&state, &request_id, "reload_failed").await;
            let _ = rollback_updated_project(
                &state.config_source.work_dir,
                reload_ctx.as_ref(),
                &request_id,
                remote_addr,
                "background_reload_task_join_failed",
            );
        }
    }
}

async fn map_reload_result(
    result: wf_runtime::error::RuntimeResult<ReloadOutcome>,
    state: Arc<AppState>,
    request_id: String,
    remote_addr: SocketAddr,
    reason: String,
    reload_ctx: Option<ProjectRemoteReloadContext>,
) -> Response<Full<Bytes>> {
    let update_result = reload_ctx
        .as_ref()
        .and_then(|ctx| ctx.update_result.as_ref());
    match result {
        Ok(ReloadOutcome::Applied(_plan)) => {
            mark_reload_finished(&state, &request_id, "reload_done").await;
            tracing::info!(
                domain = "sys",
                "admin api reload done request_id={} remote={} reason={}",
                request_id,
                remote_addr,
                reason
            );
            json_response(
                StatusCode::OK,
                &ReloadResponse {
                    request_id,
                    accepted: true,
                    result: "reload_done",
                    update: update_result.map(|_| true),
                    requested_version: update_result
                        .and_then(|result| result.requested_version.clone()),
                    current_version: update_result.map(|result| result.current_version.clone()),
                    resolved_tag: update_result.map(|result| result.resolved_tag.clone()),
                    group: update_result.and_then(|result| result.group.clone()),
                    force_replaced: Some(false),
                    warning: None,
                    error: None,
                },
            )
        }
        Ok(ReloadOutcome::Blocked(plan)) => {
            mark_reload_finished(&state, &request_id, "restart_required").await;
            let blockers = plan.requires_restart.len();
            tracing::info!(
                domain = "sys",
                "admin api reload restart required request_id={} remote={} blockers={} reason={}",
                request_id,
                remote_addr,
                blockers,
                reason
            );
            json_response(
                StatusCode::OK,
                &ReloadResponse {
                    request_id,
                    accepted: true,
                    result: "restart_required",
                    update: update_result.map(|_| true),
                    requested_version: update_result
                        .and_then(|result| result.requested_version.clone()),
                    current_version: update_result.map(|result| result.current_version.clone()),
                    resolved_tag: update_result.map(|result| result.resolved_tag.clone()),
                    group: update_result.and_then(|result| result.group.clone()),
                    force_replaced: None,
                    warning: Some(format!(
                        "reload requires restart because {} restart-required changes were found; synced project content was kept",
                        blockers
                    )),
                    error: None,
                },
            )
        }
        Err(err) => {
            mark_reload_finished(&state, &request_id, "reload_failed").await;
            let msg = err.to_string();
            let rollback_warning = rollback_updated_project(
                &state.config_source.work_dir,
                reload_ctx.as_ref(),
                &request_id,
                remote_addr,
                "reload_failed",
            );
            tracing::warn!(
                domain = "sys",
                "admin api reload failed request_id={} remote={} reason={} error={}",
                request_id,
                remote_addr,
                reason,
                msg
            );
            json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &ReloadResponse {
                    request_id,
                    accepted: false,
                    result: "reload_failed",
                    update: update_result.map(|_| true),
                    requested_version: update_result
                        .and_then(|result| result.requested_version.clone()),
                    current_version: update_result.map(|result| result.current_version.clone()),
                    resolved_tag: update_result.map(|result| result.resolved_tag.clone()),
                    group: update_result.and_then(|result| result.group.clone()),
                    force_replaced: None,
                    warning: rollback_warning,
                    error: Some(msg),
                },
            )
        }
    }
}

#[derive(Debug, Deserialize, Default)]
struct ReloadRequest {
    #[serde(default = "default_wait")]
    wait: bool,
    #[serde(default)]
    update: bool,
    version: Option<String>,
    #[serde(default)]
    group: Option<String>,
    timeout_ms: Option<u64>,
    reason: Option<String>,
}

fn load_remote_conf(src: &ReloadConfigSource) -> Result<ProjectRemoteConf, String> {
    let loader = FusionConfigLoader::new(
        &src.config_path,
        &src.overlay_paths,
        &src.config_ctx,
        Some(&src.work_dir),
    );
    Ok(loader
        .load()
        .map_err(|e| format!("failed to load config for [project_remote]: {e}"))?
        .project_remote)
}

fn run_remote_sync(
    src: &ReloadConfigSource,
    remote_conf: &ProjectRemoteConf,
    version: Option<&str>,
    group: Option<RemoteGroup>,
    lock_guard: &ProjectRemoteLockGuard,
    snapshot: &ProjectRemoteSnapshot,
) -> Result<ProjectRemoteUpdateResult, String> {
    wf_project_remote::run_remote_update_locked(
        &src.work_dir,
        version,
        group,
        lock_guard,
        snapshot,
        |work_root, ver, group| match group {
            Some(group) => {
                wf_project_remote::sync_project_remote_group(work_root, group, remote_conf, ver)
            }
            None => wf_project_remote::sync_project_remote(work_root, remote_conf, ver),
        },
    )
}

fn rollback_updated_project(
    work_root: &Path,
    reload_ctx: Option<&ProjectRemoteReloadContext>,
    request_id: &str,
    remote_addr: SocketAddr,
    stage: &str,
) -> Option<String> {
    let ctx = reload_ctx?;
    let update_result = ctx.update_result.as_ref()?;

    let mut warnings = Vec::new();
    if let Err(err) = wf_project_remote::restore_project_remote_update(
        work_root,
        &ctx.snapshot,
        update_result.changed,
    ) {
        warnings.push(format!("restore project failed: {}", err));
    }

    if let Err(err) =
        wf_project_remote::restore_runtime_artifact_snapshot(work_root, &ctx.runtime_snapshot)
    {
        warnings.push(format!("restore runtime artifacts failed: {}", err));
    }

    if warnings.is_empty() {
        tracing::info!(
            domain = "sys",
            "admin api project rollback done request_id={} remote={} stage={} target_version={} changed={}",
            request_id,
            remote_addr,
            stage,
            update_result.current_version,
            update_result.changed
        );
        None
    } else {
        let warning = warnings.join("; ");
        tracing::warn!(
            domain = "sys",
            "admin api project rollback warning request_id={} remote={} stage={} warning={}",
            request_id,
            remote_addr,
            stage,
            warning
        );
        Some(warning)
    }
}

/// Escape a string for safe embedding in a JSON string literal.
///
/// Handles the characters JSON requires to escape: `\`, `"`, and the
/// control characters U+0000–U+001F (incl. `\n`/`\r`/`\t`/`\b`/`\f`). Used
/// for client-facing error messages and the `X-Request-Id` echo, so it must
/// never produce invalid JSON or allow field injection.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str(r"\\"),
            '"' => out.push_str(r#"\""#),
            '\n' => out.push_str(r"\n"),
            '\r' => out.push_str(r"\r"),
            '\t' => out.push_str(r"\t"),
            '\u{0008}' => out.push_str(r"\b"),
            '\u{000C}' => out.push_str(r"\f"),
            c if c <= '\u{001F}' => {
                // Remaining control chars use the \u00XX form.
                out.push_str(&format!(r"\u{:04x}", c as u32));
            }
            other => out.push(other),
        }
    }
    out
}

/// Resolve a request id: prefer the `X-Request-Id` header (non-empty after
/// trim), otherwise generate a fresh UUID. Mirrors wparse's `request_id`.
///
/// The header value is client-controlled, so it is run through
/// [`json_escape`] before being interpolated into JSON response bodies —
/// otherwise a malicious `X-Request-Id` could inject fields / break the JSON
/// structure of every response.
fn request_id(headers: &hyper::header::HeaderMap) -> String {
    headers
        .get("X-Request-Id")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string())
}

#[derive(Debug)]
enum ReadBodyError {
    TooLarge(usize),
    InvalidJson(String),
    Read(String),
}

async fn read_json_body<T>(
    body: hyper::body::Incoming,
    max_body_bytes: usize,
) -> Result<T, ReadBodyError>
where
    T: for<'de> Deserialize<'de>,
{
    let collected = Limited::new(body, max_body_bytes)
        .collect()
        .await
        .map_err(|err| {
            if err.to_string().contains("length limit exceeded") {
                ReadBodyError::TooLarge(max_body_bytes)
            } else {
                ReadBodyError::Read(format!("read request body failed: {err}"))
            }
        })?;
    let bytes = collected.to_bytes();
    serde_json::from_slice(&bytes)
        .map_err(|e| ReadBodyError::InvalidJson(format!("invalid JSON body: {}", e)))
}

async fn mark_reload_started(state: &AppState, request_id: &str) {
    state.reloading.store(true, Ordering::Relaxed);
    let mut reload_state = state.reload_state.lock().await;
    reload_state.current_request_id = Some(request_id.to_string());
    reload_state.last_reload_request_id = Some(request_id.to_string());
    reload_state.last_reload_started_at = Some(SystemTime::now());
    reload_state.last_reload_finished_at = None;
    reload_state.last_reload_result = None;
}

async fn mark_reload_finished(state: &AppState, request_id: &str, result: &'static str) {
    state.reloading.store(false, Ordering::Relaxed);
    let mut reload_state = state.reload_state.lock().await;
    if reload_state.current_request_id.as_deref() == Some(request_id) {
        reload_state.current_request_id = None;
    }
    reload_state.last_reload_request_id = Some(request_id.to_string());
    reload_state.last_reload_result = Some(result);
    reload_state.last_reload_finished_at = Some(SystemTime::now());
}

fn read_project_version(work_root: &Path) -> Result<Option<serde_json::Value>, String> {
    match wf_project_remote::current_project_group_versions(work_root)? {
        Some(group_versions) => Ok(Some(group_versions)),
        None => {
            Ok(wf_project_remote::current_project_version(work_root)?
                .map(serde_json::Value::String))
        }
    }
}

fn authorized(headers: &hyper::HeaderMap<HeaderValue>, token: &str) -> bool {
    let Some(value) = headers.get(AUTHORIZATION) else {
        return false;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    let Some(token_part) = value.strip_prefix("Bearer ") else {
        return false;
    };
    token_part == token
}

fn json_response<T: Serialize>(status: StatusCode, value: &T) -> Response<Full<Bytes>> {
    let body = match serde_json::to_vec(value) {
        Ok(body) => body,
        Err(err) => format!(
            r#"{{"accepted":false,"result":"internal_error","error":"{}"}}"#,
            json_escape(&err.to_string())
        )
        .into_bytes(),
    };
    let mut resp = Response::new(Full::new(Bytes::from(body)));
    *resp.status_mut() = status;
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    resp
}

fn system_time_to_rfc3339(time: SystemTime) -> String {
    let dt: DateTime<Utc> = time.into();
    dt.to_rfc3339()
}

fn default_wait() -> bool {
    true
}

#[cfg(test)]
#[path = "admin_api_tests.rs"]
mod tests;
