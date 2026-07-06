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
    /// engine's runtime base dir (config file's parent, or `--work-dir`).
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
mod tests {
    use super::*;

    fn test_config(enabled: bool) -> AdminApiConf {
        AdminApiConf {
            enabled,
            bind: "127.0.0.1:0".to_string(),
            ..Default::default()
        }
    }

    /// A fresh control handle bound to an uncancelled token, for tests that
    /// only exercise the `status` route. The receiver is dropped, so any
    /// reload attempt would fail with a closed-channel error — fine for status
    /// tests, which never reload.
    fn test_control_handle() -> RuntimeControlHandle {
        let cancel = tokio_util::sync::CancellationToken::new();
        let (tx, _rx) = tokio::sync::mpsc::channel::<wf_runtime::lifecycle::ReloadRequest>(1);
        RuntimeControlHandle::new(tx, cancel)
    }

    /// A placeholder config source for tests that never reload (status/auth
    /// tests). Points at a `wfusion.toml` under `dir`; the file need not exist
    /// because these tests never hit the reload path.
    fn test_config_source(dir: &Path) -> ReloadConfigSource {
        ReloadConfigSource::new(
            dir.join("wfusion.toml"),
            Vec::new(),
            ConfigVarContext::new(),
            dir.to_path_buf(),
        )
    }

    #[test]
    fn disabled_returns_none() {
        let config = test_config(false);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(start_if_enabled(
            Path::new("."),
            &config,
            test_control_handle(),
            test_config_source(Path::new(".")),
        ));
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn enabled_but_missing_token_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = test_config(true);
        config.auth.token_file = "nonexistent/token".to_string();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(start_if_enabled(
            dir.path(),
            &config,
            test_control_handle(),
            test_config_source(dir.path()),
        ));
        assert!(result.is_err());
    }

    #[test]
    fn json_response_has_correct_content_type() {
        let resp = json_response(StatusCode::OK, "{\"key\":\"value\"}");
        assert_eq!(resp.status(), 200);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/json"
        );
    }

    #[test]
    fn json_escape_handles_required_characters() {
        // Backslash and quote — must be escaped, no field injection possible.
        assert_eq!(json_escape(r#"a\b"c"#), r#"a\\b\"c"#);
        // A payload that would break out of the JSON string field if unescaped.
        let injected = r#"x","injected":"y"#;
        let escaped = json_escape(injected);
        assert_eq!(escaped, r#"x\",\"injected\":\"y"#);
        // It must round-trip-safe: re-parsing the wrapped value yields the input.
        let wrapped = format!("\"{}\"", escaped);
        let parsed: serde_json::Value =
            serde_json::from_str(&wrapped).expect("escaped output is valid JSON");
        assert_eq!(parsed.as_str().unwrap(), injected);
    }

    #[test]
    fn json_escape_escapes_all_control_chars() {
        // JSON forbids literal control chars U+0000–U+001F inside strings.
        let s: String = std::iter::once('a')
            .chain((0u32..=0x1F).map(|c| char::from_u32(c).unwrap()))
            .chain(std::iter::once('z'))
            .collect();
        let escaped = json_escape(&s);
        let wrapped = format!("\"{}\"", escaped);
        serde_json::from_str::<serde_json::Value>(&wrapped)
            .expect("escaped control chars produce valid JSON");
        // Spot-check the named escapes.
        assert_eq!(json_escape("\n"), r"\n");
        assert_eq!(json_escape("\r"), r"\r");
        assert_eq!(json_escape("\t"), r"\t");
        assert_eq!(json_escape("\u{0008}"), r"\b");
        assert_eq!(json_escape("\u{000C}"), r"\f");
        assert_eq!(json_escape("\u{0000}"), r"\u0000");
    }

    #[test]
    fn json_escape_passes_through_plain_text() {
        assert_eq!(json_escape("hello world"), "hello world");
        assert_eq!(json_escape(""), "");
        // Non-ASCII / unicode is left as-is (valid in JSON strings).
        assert_eq!(json_escape("中文 \u{1F600}"), "中文 \u{1F600}");
    }

    // ── integration tests (start server + HTTP requests) ────────────

    fn write_token_with_mode(dir: &Path, rel: &str, mode: u32) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, "test-token\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(mode);
            std::fs::set_permissions(&path, perms).unwrap();
        }
    }

    fn write_token(dir: &Path, rel: &str) {
        write_token_with_mode(dir, rel, 0o600);
    }

    #[tokio::test]
    async fn starts_successfully() {
        let temp = tempfile::tempdir().unwrap();
        write_token(temp.path(), "runtime/admin_api.token");
        let mut config = test_config(true);
        config.auth.token_file = "runtime/admin_api.token".to_string();
        let runtime = start_if_enabled(
            temp.path(),
            &config,
            test_control_handle(),
            test_config_source(temp.path()),
        )
        .await
        .expect("start")
        .expect("enabled");
        assert!(runtime.local_addr().port() > 0);
    }

    #[tokio::test]
    async fn status_requires_bearer_token() {
        let temp = tempfile::tempdir().unwrap();
        write_token(temp.path(), "runtime/admin_api.token");
        let mut config = test_config(true);
        config.auth.token_file = "runtime/admin_api.token".to_string();
        let runtime = start_if_enabled(
            temp.path(),
            &config,
            test_control_handle(),
            test_config_source(temp.path()),
        )
        .await
        .expect("start")
        .expect("enabled");
        let base = format!("http://{}", runtime.local_addr());

        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("build reqwest client");

        // No token → 401
        let resp = client
            .get(format!("{base}/admin/v1/runtime/status"))
            .send()
            .await
            .expect("send unauthorized request");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);

        // Valid token → 200
        let resp = client
            .get(format!("{base}/admin/v1/runtime/status"))
            .bearer_auth("test-token")
            .send()
            .await
            .expect("send authorized request");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("parse json");
        assert!(body["instance_id"].is_string());
        assert!(body["version"].is_string());
        assert_eq!(body["accepting"], true);

        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn status_reflects_cancel_state() {
        // `accepting` follows the Reactor's root cancellation token exposed
        // via the control handle. Before cancel → accepting=true; after → false.
        let temp = tempfile::tempdir().unwrap();
        write_token(temp.path(), "runtime/admin_api.token");
        let mut config = test_config(true);
        config.auth.token_file = "runtime/admin_api.token".to_string();
        let control = test_control_handle();
        let cancel = control.cancel_token();
        let runtime = start_if_enabled(
            temp.path(),
            &config,
            control,
            test_config_source(temp.path()),
        )
        .await
        .expect("start")
        .expect("enabled");
        let base = format!("http://{}", runtime.local_addr());

        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("build reqwest client");

        // Reactor running → accepting=true
        let resp = client
            .get(format!("{base}/admin/v1/runtime/status"))
            .bearer_auth("test-token")
            .send()
            .await
            .expect("send status request");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("parse json");
        assert_eq!(body["accepting"], true);

        // Reactor cancelled → accepting=false
        cancel.cancel();
        let resp = client
            .get(format!("{base}/admin/v1/runtime/status"))
            .bearer_auth("test-token")
            .send()
            .await
            .expect("send status request after cancel");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("parse json");
        assert_eq!(body["accepting"], false);

        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn token_file_too_permissive_is_rejected() {
        // group/other bits set → start_if_enabled must refuse to start.
        let temp = tempfile::tempdir().unwrap();
        write_token_with_mode(temp.path(), "runtime/admin_api.token", 0o644);
        let mut config = test_config(true);
        config.auth.token_file = "runtime/admin_api.token".to_string();
        let err = start_if_enabled(
            temp.path(),
            &config,
            test_control_handle(),
            test_config_source(temp.path()),
        )
        .await
        .expect_err("should reject too-permissive token file");
        assert!(err.contains("too permissive"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn request_id_echoed_from_header() {
        // X-Request-Id header is echoed back in the 401 response body.
        let temp = tempfile::tempdir().unwrap();
        write_token(temp.path(), "runtime/admin_api.token");
        let mut config = test_config(true);
        config.auth.token_file = "runtime/admin_api.token".to_string();
        let runtime = start_if_enabled(
            temp.path(),
            &config,
            test_control_handle(),
            test_config_source(temp.path()),
        )
        .await
        .expect("start")
        .expect("enabled");
        let base = format!("http://{}", runtime.local_addr());

        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("build reqwest client");

        // No auth → 401; the custom X-Request-Id should appear in the body.
        let resp = client
            .get(format!("{base}/admin/v1/runtime/status"))
            .header("X-Request-Id", "my-trace-id-123")
            .send()
            .await
            .expect("send unauthorized request");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
        let body = resp.text().await.expect("read body");
        assert!(
            body.contains(r#""request_id":"my-trace-id-123""#),
            "expected echoed request_id in body, got: {body}"
        );

        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn unknown_route_returns_404() {
        let temp = tempfile::tempdir().unwrap();
        write_token(temp.path(), "runtime/admin_api.token");
        let mut config = test_config(true);
        config.auth.token_file = "runtime/admin_api.token".to_string();
        let runtime = start_if_enabled(
            temp.path(),
            &config,
            test_control_handle(),
            test_config_source(temp.path()),
        )
        .await
        .expect("start")
        .expect("enabled");
        let base = format!("http://{}", runtime.local_addr());

        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("build reqwest client");

        let resp = client
            .get(format!("{base}/admin/v1/unknown"))
            .bearer_auth("test-token")
            .send()
            .await
            .expect("send unknown route request");
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

        runtime.shutdown().await;
    }

    // ── TLS + non-loopback bind (P0) ───────────────────────────────────

    /// Install the rustls ring crypto provider once (idempotent).
    fn init_tls_crypto() {
        use std::sync::Once;
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            rustls::crypto::ring::default_provider()
                .install_default()
                .expect("install rustls ring crypto provider");
        });
    }

    /// Generate a self-signed cert/key pair via the `openssl` CLI into `dir`.
    fn generate_self_signed_cert(dir: &Path) -> (PathBuf, PathBuf) {
        let cert_path = dir.join("cert.pem");
        let key_path = dir.join("key.pem");
        let status = std::process::Command::new("openssl")
            .args([
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-keyout",
                key_path.to_str().expect("key path is valid utf-8"),
                "-out",
                cert_path.to_str().expect("cert path is valid utf-8"),
                "-days",
                "365",
                "-nodes",
                "-subj",
                "/CN=localhost",
            ])
            .status()
            .expect("run openssl to generate self-signed cert");
        assert!(
            status.success(),
            "openssl failed to generate self-signed cert"
        );
        (cert_path, key_path)
    }

    fn tls_config(_dir: &Path, bind: &str, cert: &str, key: &str) -> AdminApiConf {
        let mut config = AdminApiConf {
            enabled: true,
            bind: bind.to_string(),
            ..Default::default()
        };
        config.auth.token_file = "runtime/admin_api.token".to_string();
        config.tls.enabled = true;
        config.tls.cert_file = cert.to_string();
        config.tls.key_file = key.to_string();
        config
    }

    #[tokio::test]
    async fn rejects_non_loopback_without_tls() {
        let temp = tempfile::tempdir().unwrap();
        write_token(temp.path(), "runtime/admin_api.token");
        let mut config = test_config(true);
        config.bind = "0.0.0.0:0".to_string();
        config.auth.token_file = "runtime/admin_api.token".to_string();
        let err = start_if_enabled(
            temp.path(),
            &config,
            test_control_handle(),
            test_config_source(temp.path()),
        )
        .await
        .expect_err("should reject non-loopback without tls");
        assert!(
            err.contains("non-loopback") && err.contains("requires admin_api.tls.enabled=true"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn tls_accepts_https_requests() {
        init_tls_crypto();
        let temp = tempfile::tempdir().unwrap();
        write_token(temp.path(), "runtime/admin_api.token");
        let (cert_path, key_path) = generate_self_signed_cert(temp.path());
        let config = tls_config(
            temp.path(),
            "127.0.0.1:0",
            cert_path.to_str().unwrap(),
            key_path.to_str().unwrap(),
        );
        let runtime = start_if_enabled(
            temp.path(),
            &config,
            test_control_handle(),
            test_config_source(temp.path()),
        )
        .await
        .expect("start")
        .expect("enabled");
        let base = format!("https://{}", runtime.local_addr());

        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .no_proxy()
            .build()
            .expect("build https client");

        // No token → 401
        let resp = client
            .get(format!("{base}/admin/v1/runtime/status"))
            .send()
            .await
            .expect("send unauthorized https request");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);

        // Valid token → 200
        let resp = client
            .get(format!("{base}/admin/v1/runtime/status"))
            .bearer_auth("test-token")
            .send()
            .await
            .expect("send authorized https request");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn tls_works_with_non_loopback() {
        init_tls_crypto();
        let temp = tempfile::tempdir().unwrap();
        write_token(temp.path(), "runtime/admin_api.token");
        let (cert_path, key_path) = generate_self_signed_cert(temp.path());
        // Non-loopback bind is allowed when TLS is enabled.
        let config = tls_config(
            temp.path(),
            "0.0.0.0:0",
            cert_path.to_str().unwrap(),
            key_path.to_str().unwrap(),
        );
        let runtime = start_if_enabled(
            temp.path(),
            &config,
            test_control_handle(),
            test_config_source(temp.path()),
        )
        .await
        .expect("start")
        .expect("enabled");

        // Connect to the bound address via 127.0.0.1 (the OS routes localhost
        // to the 0.0.0.0 listener). Self-signed cert → accept invalid certs.
        let base = format!("https://127.0.0.1:{}", runtime.local_addr().port());
        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .no_proxy()
            .build()
            .expect("build https client");
        let resp = client
            .get(format!("{base}/admin/v1/runtime/status"))
            .bearer_auth("test-token")
            .send()
            .await
            .expect("send https request to non-loopback bind");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        runtime.shutdown().await;
    }

    // ── P2: POST /admin/v1/reloads/model ────────────────────────────

    /// A complete, runnable engine fixture under `root`: wfusion.toml (file
    /// source over an empty seed), one rule, the catch-all file sink, and an
    /// admin_api token. Mirrors the wf-runtime reload-tests fixture so reload
    /// decisions line up with what the Reactor expects.
    fn write_engine_fixture(root: &Path, rule: &str, rules_glob: &str) {
        std::fs::write(
            root.join("wfusion.toml"),
            format!(
                r#"
mode = "daemon"
windows = "models/windows.toml"
sinks = "sinks"

[[sources]]
type = "file"
name = "seed"
path = "seed.ndjson"
stream = "syslog"
data_format = "ndjson"

[runtime]
executor_parallelism = 2
rule_exec_timeout = "30s"
schemas = "schemas/*.wfs"
rules = "{rules_glob}"

[vars]
FAIL_THRESHOLD = "3"
"#
            ),
        )
        .unwrap();
        // Write window config to external file.
        std::fs::create_dir_all(root.join("models")).unwrap();
        std::fs::write(
            root.join("models/windows.toml"),
            r#"
[window_defaults]
evict_interval = "30s"
max_window_bytes = "256MB"
max_total_bytes = "2GB"
evict_policy = "time_first"
watermark = "5s"
allowed_lateness = "0s"
late_policy = "drop"

[window.auth_events]
mode = "local"
max_window_bytes = "256MB"
over_cap = "30m"

[window.security_alerts]
mode = "local"
max_window_bytes = "64MB"
over_cap = "1h"
"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join("schemas")).unwrap();
        std::fs::write(root.join("schemas/security.wfs"), SECURITY_SCHEMA).unwrap();
        // Derive the rule directory from the glob so a glob like
        // `rules/v1/*.wfl` lands the rule in `rules/v1/`.
        let rule_dir = rules_glob
            .strip_suffix("/*.wfl")
            .unwrap_or("rules")
            .to_string();
        std::fs::create_dir_all(root.join(&rule_dir)).unwrap();
        std::fs::write(root.join(rule_dir).join("brute_force.wfl"), rule).unwrap();
        std::fs::write(root.join("seed.ndjson"), "").unwrap();
        write_sink_layout(root);
        std::fs::create_dir_all(root.join("runtime")).unwrap();
        write_token(root, "runtime/admin_api.token");
    }

    const SECURITY_SCHEMA: &str = r#"
window auth_events {
    stream = "syslog"
    time = event_time
    over = 5m

    fields {
        sip: ip
        username: chars
        action: chars
        event_time: time
    }
}

window security_alerts {
    over = 0
    fields {
        sip: ip
        fail_count: digit
        message: chars
    }
}
"#;

    const BRUTE_FORCE_RULE: &str = r#"
rule brute_force_then_scan {
  events { fail : auth_events && action == "failed" }
  match<sip:5m> {
    on event { fail | count >= ${FAIL_THRESHOLD:3}; }
    and close { fail | count >= 1; }
  } -> score(70.0)
  entity(ip, fail.sip)
  yield security_alerts (
    sip = fail.sip, fail_count = count(fail),
    message = fmt("{} brute force detected", fail.sip)
  )
}
"#;

    /// Same rule, different score — a rule-only change (topology unchanged).
    const BRUTE_FORCE_RULE_V2: &str = r#"
rule brute_force_then_scan {
  events { fail : auth_events && action == "failed" }
  match<sip:5m> {
    on event { fail | count >= ${FAIL_THRESHOLD:3}; }
    and close { fail | count >= 1; }
  } -> score(99.0)
  entity(ip, fail.sip)
  yield security_alerts (
    sip = fail.sip, fail_count = count(fail),
    message = fmt("{} brute force detected", fail.sip)
  )
}
"#;

    fn write_sink_layout(root: &Path) {
        std::fs::create_dir_all(root.join("connectors/sink.d")).unwrap();
        std::fs::write(
            root.join("connectors/sink.d/file_json.toml"),
            r#"
[[connectors]]
id = "file_json"
type = "file"
allow_override = ["file"]
[connectors.params]
fmt = "json"
file = "default.jsonl"
"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join("sinks/business.d")).unwrap();
        std::fs::write(root.join("sinks/defaults.toml"), "tags = [\"env:dev\"]\n").unwrap();
        std::fs::write(
            root.join("sinks/business.d/catch_all.toml"),
            r#"
[sink_group]
name = "catch_all"
windows = ["*"]
[[sink_group.sinks]]
connect = "file_json"
name = "all_alerts"
[sink_group.sinks.params]
file = "all.jsonl"
"#,
        )
        .unwrap();
    }

    /// Boot a real Reactor, serve admin_api with its control handle, and drive
    /// `run()` on a background task. Returns the http base url, the temp dir
    /// (caller cleans up), and the background run-task handle (caller cancels
    /// via the Reactor token then awaits).
    async fn boot_engine_with_admin(rule: &str) -> (tempfile::TempDir, String, RuntimeServant) {
        use wf_config::{ConfigVarContext, FusionConfigLoader};
        use wf_runtime::lifecycle::Reactor;

        let temp = tempfile::tempdir().unwrap();
        write_engine_fixture(temp.path(), rule, "rules/*.wfl");

        let cfg_path = temp.path().join("wfusion.toml");
        let ctx = ConfigVarContext::new();
        let loader = FusionConfigLoader::new(&cfg_path, &[], &ctx, Some(temp.path()));
        let raw = loader.load_raw().expect("load raw");
        let mut fusion_config = loader.load().expect("load config");
        fusion_config.admin_api.enabled = true;
        fusion_config.admin_api.bind = "127.0.0.1:0".to_string();
        fusion_config.admin_api.auth.token_file = "runtime/admin_api.token".to_string();
        let admin_conf = fusion_config.admin_api.clone();

        let reactor = Reactor::start(fusion_config, raw, temp.path())
            .await
            .expect("reactor start");
        let control = reactor.control_handle();
        let cancel = control.cancel_token();
        let config_source = ReloadConfigSource::new(
            cfg_path.clone(),
            Vec::new(),
            ctx.clone(),
            temp.path().to_path_buf(),
        );
        let admin = start_if_enabled(temp.path(), &admin_conf, control, config_source)
            .await
            .expect("start admin")
            .expect("admin enabled");
        let base = format!("http://{}", admin.local_addr());
        // Keep the admin server alive until the servant is dropped.
        std::mem::forget(admin);
        let run_task = tokio::spawn(async move { reactor.run().await });
        (temp, base, RuntimeServant { run_task, cancel })
    }

    /// Owns the background reactor run-task + its cancel token; `shutdown`
    /// cancels and joins.
    struct RuntimeServant {
        run_task: tokio::task::JoinHandle<
            wf_runtime::error::RuntimeResult<wf_runtime::lifecycle::RunOutcome>,
        >,
        cancel: tokio_util::sync::CancellationToken,
    }
    impl RuntimeServant {
        async fn shutdown(self) {
            self.cancel.cancel();
            let _ = self.run_task.await;
        }
    }

    #[tokio::test]
    async fn reload_applied_returns_200() {
        let (temp, base, servant) = boot_engine_with_admin(BRUTE_FORCE_RULE).await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();

        // Rule-only change on disk → reload is Applied.
        std::fs::write(
            temp.path().join("rules/brute_force.wfl"),
            BRUTE_FORCE_RULE_V2,
        )
        .unwrap();

        let resp = client
            .post(format!("{base}/admin/v1/reloads/model"))
            .bearer_auth("test-token")
            .send()
            .await
            .expect("post reload");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("json");
        assert_eq!(body["accepted"], true);
        assert_eq!(body["result"], "applied");

        servant.shutdown().await;
    }

    #[tokio::test]
    async fn reload_pipeline_returns_200() {
        // Point the rules glob at a dir with a pipeline rule — under L3
        // pipeline windows are pure additions, so the reload succeeds (200).
        let temp = tempfile::tempdir().unwrap();
        write_engine_fixture(temp.path(), BRUTE_FORCE_RULE, "rules/v1/*.wfl");
        // Move the seeded rule into v1 and add a pipeline rule in v2.
        let v2 = temp.path().join("rules/v2");
        std::fs::create_dir_all(&v2).unwrap();
        // v1 already written by write_engine_fixture (rules/v1/brute_force.wfl).
        std::fs::write(
            v2.join("repeated_fail_bursts.wfl"),
            r#"
rule repeated_fail_bursts {
  events { e : auth_events && action == "failed" }
  match<sip,username:5m:fixed> {
    on event { e | count >= 1; }
    and close { burst: e | count >= 3; }
  }
  |> match<sip:30m:fixed> {
    on event { _in | count >= 1; }
    and close { users: _in.username | distinct | count >= 2; }
  } -> score(85.0)
  entity(ip, _in.sip)
  yield security_alerts (
    sip = _in.sip, fail_count = 2,
    message = fmt("{} multi-user fail bursts", _in.sip)
  )
}
"#,
        )
        .unwrap();

        // Boot engine on v1, then flip config to v2 and reload.
        use wf_config::{ConfigVarContext, FusionConfigLoader};
        use wf_runtime::lifecycle::Reactor;
        let cfg_path = temp.path().join("wfusion.toml");
        let ctx = ConfigVarContext::new();
        let loader = FusionConfigLoader::new(&cfg_path, &[], &ctx, Some(temp.path()));
        let raw = loader.load_raw().expect("raw");
        let mut fusion_config = loader.load().expect("config");
        fusion_config.admin_api.enabled = true;
        fusion_config.admin_api.bind = "127.0.0.1:0".to_string();
        fusion_config.admin_api.auth.token_file = "runtime/admin_api.token".to_string();
        let admin_conf = fusion_config.admin_api.clone();
        let reactor = Reactor::start(fusion_config, raw, temp.path())
            .await
            .expect("start");
        let control = reactor.control_handle();
        let cancel = control.cancel_token();
        let config_source = ReloadConfigSource::new(
            cfg_path.clone(),
            Vec::new(),
            ctx.clone(),
            temp.path().to_path_buf(),
        );
        let admin = start_if_enabled(temp.path(), &admin_conf, control, config_source)
            .await
            .expect("admin")
            .expect("enabled");
        let base = format!("http://{}", admin.local_addr());
        std::mem::forget(admin);
        let run_task = tokio::spawn(async move { reactor.run().await });

        // Flip the rules glob to v2 (topology change).
        std::fs::write(
            temp.path().join("wfusion.toml"),
            std::fs::read_to_string(temp.path().join("wfusion.toml"))
                .unwrap()
                .replace("rules/v1/*.wfl", "rules/v2/*.wfl"),
        )
        .unwrap();

        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let resp = client
            .post(format!("{base}/admin/v1/reloads/model"))
            .bearer_auth("test-token")
            .send()
            .await
            .expect("post");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("json");
        assert_eq!(body["accepted"], true);
        assert_eq!(body["result"], "applied");

        cancel.cancel();
        let _ = run_task.await;
    }

    #[tokio::test]
    async fn reload_without_token_returns_401() {
        let (_temp, base, servant) = boot_engine_with_admin(BRUTE_FORCE_RULE).await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let resp = client
            .post(format!("{base}/admin/v1/reloads/model"))
            .send()
            .await
            .expect("post");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
        servant.shutdown().await;
    }

    /// Regression for review M2: an oversized request body is rejected with
    /// 413 instead of being buffered unbounded into memory.
    #[tokio::test]
    async fn reload_oversized_body_returns_413() {
        let (_temp, base, servant) = boot_engine_with_admin(BRUTE_FORCE_RULE).await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        // 2 MiB body, well over the 1 MiB cap.
        let huge = vec![b'x'; 2 * 1024 * 1024];
        let resp = client
            .post(format!("{base}/admin/v1/reloads/model"))
            .bearer_auth("test-token")
            .body(huge)
            .send()
            .await
            .expect("post");
        assert_eq!(resp.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);
        servant.shutdown().await;
    }

    /// Regression for review M1: when the engine is launched with a CLI
    /// `--mode` that differs from the TOML `mode`, a reload must NOT be wrongly
    /// blocked by the "effective mode changed" rule. The reload re-applies the
    /// captured CLI override so `prepare_reload` compares apples to apples.
    #[tokio::test]
    async fn reload_works_when_cli_mode_overrides_toml_mode() {
        use wf_config::FusionMode;
        use wf_runtime::lifecycle::Reactor;

        let temp = tempfile::tempdir().unwrap();
        // TOML says batch; we'll boot with `--mode daemon` override (like the
        // `wfusion daemon` subcommand does).
        write_engine_fixture(temp.path(), BRUTE_FORCE_RULE, "rules/*.wfl");
        let toml_path = temp.path().join("wfusion.toml");
        let mut toml = std::fs::read_to_string(&toml_path).unwrap();
        toml = toml.replacen("mode = \"daemon\"", "mode = \"batch\"", 1);
        std::fs::write(&toml_path, toml).unwrap();

        let ctx = ConfigVarContext::new();
        let loader = FusionConfigLoader::new(&toml_path, &[], &ctx, Some(temp.path()));
        let raw = loader.load_raw().expect("raw");
        let mut fusion_config = loader.load().expect("config");
        // Emulate the `daemon` subcommand forcing daemon mode.
        fusion_config.mode = FusionMode::Daemon;
        fusion_config.admin_api.enabled = true;
        fusion_config.admin_api.bind = "127.0.0.1:0".to_string();
        fusion_config.admin_api.auth.token_file = "runtime/admin_api.token".to_string();
        let admin_conf = fusion_config.admin_api.clone();

        let reactor = Reactor::start(fusion_config, raw, temp.path())
            .await
            .expect("start");
        let control = reactor.control_handle();
        let cancel = control.cancel_token();
        let config_source = ReloadConfigSource::new(
            toml_path.clone(),
            Vec::new(),
            ctx.clone(),
            temp.path().to_path_buf(),
        )
        .with_mode_override(FusionMode::Daemon);
        let admin = start_if_enabled(temp.path(), &admin_conf, control, config_source)
            .await
            .expect("admin")
            .expect("enabled");
        let base = format!("http://{}", admin.local_addr());
        std::mem::forget(admin);
        let run_task = tokio::spawn(async move { reactor.run().await });

        // Rule-only change (score 70→99). Without the M1 fix this would 409
        // because next_config.mode ("batch") != current ("daemon").
        std::fs::write(
            temp.path().join("rules/brute_force.wfl"),
            BRUTE_FORCE_RULE_V2,
        )
        .unwrap();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let resp = client
            .post(format!("{base}/admin/v1/reloads/model"))
            .bearer_auth("test-token")
            .send()
            .await
            .expect("post");
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::OK,
            "reload should be applied despite CLI mode override"
        );
        let body: serde_json::Value = resp.json().await.expect("json");
        assert_eq!(body["result"], "applied");

        cancel.cancel();
        let _ = run_task.await;
    }

    /// A reload against a config file that no longer parses returns a structured
    /// error response (not 200, not a panic) — covers the config-load-failure
    /// branch of `handle_reload`.
    #[tokio::test]
    async fn reload_with_broken_config_returns_error() {
        let (temp, base, servant) = boot_engine_with_admin(BRUTE_FORCE_RULE).await;
        // Corrupt the on-disk config so the loader fails to parse it.
        std::fs::write(
            temp.path().join("wfusion.toml"),
            "this is = = not valid toml {{{",
        )
        .unwrap();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let resp = client
            .post(format!("{base}/admin/v1/reloads/model"))
            .bearer_auth("test-token")
            .send()
            .await
            .expect("post");
        // Must be an error status (500), with a JSON body describing the failure.
        assert!(
            resp.status().is_server_error(),
            "expected 5xx for broken config, got {}",
            resp.status()
        );
        let body: serde_json::Value = resp.json().await.expect("json");
        assert_eq!(body["accepted"], false);
        assert_eq!(body["result"], "error");
        // And the response must itself be valid JSON even though the error
        // message likely contains the offending toml fragment.
        assert!(body["error"].is_string());

        servant.shutdown().await;
    }

    /// A reload issued after the Reactor has been shut down returns an error
    /// rather than hanging forever — covers the `apply_reload` Err branch
    /// (control channel closed once the reactor's `run()` exits).
    #[tokio::test]
    async fn reload_after_shutdown_returns_error_not_hang() {
        let (temp, base, servant) = boot_engine_with_admin(BRUTE_FORCE_RULE).await;
        // Stop the engine first.
        servant.cancel.cancel();
        // Give the control loop a moment to exit and drop the receiver.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client
                .post(format!("{base}/admin/v1/reloads/model"))
                .bearer_auth("test-token")
                .send(),
        )
        .await;
        // Must resolve (not hang) …
        assert!(result.is_ok(), "reload after shutdown hung past 5s");
        let resp = result.unwrap().expect("send");
        // … and report failure (5xx), not success.
        assert!(
            resp.status().is_server_error(),
            "expected 5xx after shutdown, got {}",
            resp.status()
        );

        let _ = std::fs::remove_dir_all(temp.path());
    }

    // -- L4: full=true restart -------------------------------------------------

    /// With `full=true`, a hot-reloadable change still returns 200 — don't
    /// waste a restart when rule-only changes suffice.
    #[tokio::test]
    async fn reload_full_true_hot_applied_returns_200() {
        let (temp, base, servant) = boot_engine_with_admin(BRUTE_FORCE_RULE).await;
        std::fs::write(
            temp.path().join("rules/brute_force.wfl"),
            BRUTE_FORCE_RULE_V2,
        )
        .unwrap();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let resp = client
            .post(format!("{base}/admin/v1/reloads/model"))
            .bearer_auth("test-token")
            .body(r#"{"full": true}"#)
            .send()
            .await
            .expect("post");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("json");
        assert_eq!(body["accepted"], true);
        assert_eq!(body["result"], "applied");
        servant.shutdown().await;
    }

    /// With `full=true` and a change that requires restart (changing mode in
    /// wfusion.toml), the reload returns 202 `restarting` and the reactor
    /// begins graceful shutdown.
    #[tokio::test]
    async fn reload_full_true_blocked_returns_202() {
        let (temp, base, servant) = boot_engine_with_admin(BRUTE_FORCE_RULE).await;
        // Change mode from daemon to batch — a raw-diff restart-required field.
        let toml = std::fs::read_to_string(temp.path().join("wfusion.toml")).unwrap();
        std::fs::write(
            temp.path().join("wfusion.toml"),
            toml.replace("mode = \"daemon\"", "mode = \"batch\""),
        )
        .unwrap();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let resp = client
            .post(format!("{base}/admin/v1/reloads/model"))
            .bearer_auth("test-token")
            .body(r#"{"full": true}"#)
            .send()
            .await
            .expect("post");
        assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
        let body: serde_json::Value = resp.json().await.expect("json");
        assert_eq!(body["accepted"], true);
        assert_eq!(body["result"], "restarting");
        // The reactor was asked to restart; wait for the run task to finish.
        servant.shutdown().await;
    }

    /// With `full=true` and a broken config, the reload is rejected (5xx)
    /// WITHOUT triggering a restart — crash-loop prevention. (Future work:
    /// return 422 after `compile_reload_check` does full compilation.)
    #[tokio::test]
    async fn reload_full_true_broken_config_returns_error_not_restart() {
        let (temp, base, servant) = boot_engine_with_admin(BRUTE_FORCE_RULE).await;
        std::fs::write(
            temp.path().join("wfusion.toml"),
            "this is = = not valid toml {{{",
        )
        .unwrap();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let resp = client
            .post(format!("{base}/admin/v1/reloads/model"))
            .bearer_auth("test-token")
            .body(r#"{"full": true}"#)
            .send()
            .await
            .expect("post");
        // Must NOT be 202 (no restart), must be an error (5xx).
        assert_ne!(resp.status(), reqwest::StatusCode::ACCEPTED);
        assert!(resp.status().is_server_error());
        let body: serde_json::Value = resp.json().await.expect("json");
        assert_eq!(body["accepted"], false);
        assert_eq!(body["result"], "error");
        servant.shutdown().await;
    }
}
