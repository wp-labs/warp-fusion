//! Admin API server — minimal HTTP API for engine status.
//!
//! Protected by bearer token authentication.

use std::convert::Infallible;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Bytes;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as AutoBuilder;
use rustls::ServerConfig;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;
use uuid::Uuid;
use wf_config::{
    AdminApiConf, ConfigVarContext, FusionConfig, FusionConfigLoader, FusionMode, MetricsConfig,
    RawFusionConfigTree,
};
use wf_runtime::lifecycle::{ReloadOutcome, RuntimeControlHandle};

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

    // Resolve TLS before binding so non-loopback binds can be rejected early.
    let tls = if config.tls.enabled {
        Some(load_tls_config(
            &resolve_path(work_root, &config.tls.cert_file),
            &resolve_path(work_root, &config.tls.key_file),
        )?)
    } else {
        None
    };

    if !bind.ip().is_loopback() && tls.is_none() {
        return Err(conf_err(format!(
            "non-loopback admin_api.bind '{}' requires admin_api.tls.enabled=true",
            bind
        )));
    }

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

    let instance_id = format!("fusion:{}", std::process::id());

    let state = Arc::new(AppState {
        bearer_token,
        instance_id,
        version: env!("CARGO_PKG_VERSION").to_string(),
        control,
        config_source,
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

    // Bearer token auth
    let auth_header = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let expected = format!("Bearer {}", state.bearer_token);
    if auth_header != expected {
        let body = format!(
            r#"{{"request_id":"{}","accepted":false,"result":"unauthorized","error":"invalid bearer token"}}"#,
            request_id
        );
        return Ok(json_response(StatusCode::UNAUTHORIZED, &body));
    }

    match (method.clone(), path.as_str()) {
        (Method::GET, "/admin/v1/runtime/status") => {
            let accepting = !state.control.cancel_token().is_cancelled();
            tracing::info!(
                domain = "sys",
                "admin api status request_id={} remote={} accepting={}",
                request_id,
                remote_addr,
                accepting
            );
            let body = format!(
                r#"{{"instance_id":"{}","version":"{}","accepting":{}}}"#,
                state.instance_id, state.version, accepting
            );
            Ok(json_response(StatusCode::OK, &body))
        }
        (Method::POST, "/admin/v1/reloads/model") => {
            Ok(handle_reload(req, request_id, remote_addr, state).await)
        }
        _ => {
            let body = format!(
                r#"{{"request_id":"{}","accepted":false,"result":"not_found","error":"unsupported route {} {}"}}"#,
                request_id, method, path
            );
            Ok(json_response(StatusCode::NOT_FOUND, &body))
        }
    }
}

/// `POST /admin/v1/reloads/model` — hot-reload the rule set.
///
/// Re-resolves the fusion config (raw + effective) from `work_root` and asks
/// the running Reactor to apply it. The request body is ignored for now
/// (reload always reads the on-disk config under `work_root`); a future
/// `update_remote` field will gate an optional remote sync first.
async fn handle_reload(
    req: Request<hyper::body::Incoming>,
    request_id: String,
    remote_addr: SocketAddr,
    state: Arc<AppState>,
) -> Response<Full<Bytes>> {
    // Read the body (capped at 1 MiB), then look for `"full": true`.
    const RELOAD_BODY_LIMIT: usize = 1024 * 1024;
    let full = match Limited::new(req.into_body(), RELOAD_BODY_LIMIT)
        .collect()
        .await
        .map(|collected| {
            let bytes = collected.to_bytes();
            std::str::from_utf8(&bytes)
                .ok()
                .map(|s| s.contains("\"full\": true") || s.contains("\"full\":true"))
                .unwrap_or(false)
        }) {
        Ok(full) => full,
        Err(_) => {
            let body = format!(
                r#"{{"request_id":"{}","accepted":false,"result":"error","error":"request body exceeds {} bytes"}}"#,
                request_id, RELOAD_BODY_LIMIT
            );
            return json_response(StatusCode::PAYLOAD_TOO_LARGE, &body);
        }
    };

    tracing::info!(
        domain = "sys",
        "admin api reload request_id={} remote={}",
        request_id,
        remote_addr
    );

    // Re-resolve the config the engine booted from, using the *same* config
    // path / overlays / var context captured at boot — NOT a hard-coded
    // `wfusion.toml`. This keeps reload consistent with `--config`/`--overlay`/
    // `--var` (e.g. the default `conf/wfusion.toml`).
    let src = &state.config_source;
    let loader = FusionConfigLoader::new(
        &src.config_path,
        &src.overlay_paths,
        &src.config_ctx,
        Some(&src.work_dir),
    );
    let (next_raw, mut next_config): (RawFusionConfigTree, FusionConfig) = match (
        loader.load_raw(),
        loader.load(),
    ) {
        (Ok(raw), Ok(config)) => (raw, config),
        (Err(err), _) | (_, Err(err)) => {
            let msg = err.to_string();
            tracing::warn!(domain = "sys", "reload config load failed: {msg}");
            let body = format!(
                r#"{{"request_id":"{}","accepted":false,"result":"error","error":"failed to load config: {}"}}"#,
                request_id,
                json_escape(&msg)
            );
            return json_response(StatusCode::INTERNAL_SERVER_ERROR, &body);
        }
    };
    // Re-apply the CLI overrides captured at boot (mode/metrics) so the reload
    // baseline comparison isn't thrown off by `--mode`/`--metrics*`.
    state.config_source.apply_overrides(&mut next_config);

    match state.control.apply_reload(next_raw, next_config).await {
        Ok(ReloadOutcome::Applied(plan)) => {
            tracing::info!(domain = "sys", "reload applied request_id={}", request_id);
            let body = format!(
                r#"{{"request_id":"{}","accepted":true,"result":"applied","hot_reload":{},"requires_restart":{}}}"#,
                request_id,
                plan.hot_reload.len(),
                plan.requires_restart.len()
            );
            json_response(StatusCode::OK, &body)
        }
        Ok(ReloadOutcome::Blocked(plan)) => {
            if full {
                // L4: full reload requested. Verify the new config compiles
                // (prevent crash loop), then request a graceful shutdown with
                // restart exit code.
                if compile_reload_check(&loader, &state.config_source).is_err() {
                    let body = format!(
                        r#"{{"request_id":"{}","accepted":false,"result":"error","error":"full reload refused: new config fails to compile"}}"#,
                        request_id
                    );
                    return json_response(StatusCode::UNPROCESSABLE_ENTITY, &body);
                }
                tracing::info!(
                    domain = "sys",
                    "reload blocked request_id={} full=true — requesting restart",
                    request_id
                );
                if let Err(e) = state.control.request_restart().await {
                    tracing::warn!(domain = "sys", "restart request failed: {e}");
                    let body = format!(
                        r#"{{"request_id":"{}","accepted":false,"result":"error","error":"failed to initiate restart"}}"#,
                        request_id
                    );
                    return json_response(StatusCode::INTERNAL_SERVER_ERROR, &body);
                }
                let blockers = plan.requires_restart.len();
                let body = format!(
                    r#"{{"request_id":"{}","accepted":true,"result":"restarting","requires_restart":{}}}"#,
                    request_id, blockers
                );
                return json_response(StatusCode::ACCEPTED, &body);
            }
            let blockers = plan.requires_restart.len();
            tracing::info!(
                domain = "sys",
                "reload blocked request_id={} blockers={}",
                request_id,
                blockers
            );
            let body = format!(
                r#"{{"request_id":"{}","accepted":false,"result":"blocked","requires_restart":{}}}"#,
                request_id, blockers
            );
            json_response(StatusCode::CONFLICT, &body)
        }
        Err(err) => {
            let msg = err.to_string();
            tracing::warn!(
                domain = "sys",
                "reload failed request_id={}: {msg}",
                request_id
            );
            let body = format!(
                r#"{{"request_id":"{}","accepted":false,"result":"error","error":"{}"}}"#,
                request_id,
                json_escape(&msg)
            );
            json_response(StatusCode::INTERNAL_SERVER_ERROR, &body)
        }
    }
}

/// Quick compile-check: can the loader successfully produce a valid config?
/// Used by L4 `full=true` reload before committing to a restart, to avoid
/// a crash loop (restart → broken config → crash → restart → …).
fn compile_reload_check(
    loader: &FusionConfigLoader<'_>,
    _source: &ReloadConfigSource,
) -> Result<(), String> {
    // Re-use the same loader that already read the on-disk config.
    let _raw = loader.load_raw().map_err(|e| e.to_string())?;
    let _config = loader.load().map_err(|e| e.to_string())?;
    Ok(())
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
    let raw = headers
        .get("X-Request-Id")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    json_escape(&raw)
}

fn json_response(status: StatusCode, body: &str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(Full::from(body.to_string()))
        .unwrap()
}

#[cfg(test)]
#[path = "admin_api_tests.rs"]
mod tests;
