//! Admin API server — minimal HTTP API for engine status.
//!
//! Protected by bearer token authentication.

use std::convert::Infallible;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as AutoBuilder;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ServerConfig;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;
use uuid::Uuid;
use wf_config::AdminApiConf;

// ── AdminApiRuntime ───────────────────────────────────────────────────

#[derive(Debug)]
pub struct AdminApiRuntime {
    local_addr: SocketAddr,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl AdminApiRuntime {
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }
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
    /// Root cancellation token of the Reactor. While not cancelled, the engine
    /// is accepting input. This is the scheme-A approximation of wparse's
    /// `accepting_commands` (scheme B will replace it with a full
    /// RuntimeControlHandle once reload lands).
    cancel: tokio_util::sync::CancellationToken,
}

// ── Start ─────────────────────────────────────────────────────────────

fn conf_err(detail: impl Into<String>) -> String {
    detail.into()
}

pub async fn start_if_enabled(
    work_root: &Path,
    config: &AdminApiConf,
    cancel: tokio_util::sync::CancellationToken,
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
        cancel,
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
            let accepting = !state.cancel.is_cancelled();
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
        _ => {
            let body = format!(
                r#"{{"request_id":"{}","accepted":false,"result":"not_found","error":"unsupported route {} {}"}}"#,
                request_id, method, path
            );
            Ok(json_response(StatusCode::NOT_FOUND, &body))
        }
    }
}

/// Resolve a request id: prefer the `X-Request-Id` header (non-empty after
/// trim), otherwise generate a fresh UUID. Mirrors wparse's `request_id`.
fn request_id(headers: &hyper::header::HeaderMap) -> String {
    headers
        .get("X-Request-Id")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string())
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

    /// A fresh, uncancelled token — admin_api reports `accepting=true`.
    fn fresh_cancel() -> tokio_util::sync::CancellationToken {
        tokio_util::sync::CancellationToken::new()
    }

    #[test]
    fn disabled_returns_none() {
        let config = test_config(false);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(start_if_enabled(Path::new("."), &config, fresh_cancel()));
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn enabled_but_missing_token_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = test_config(true);
        config.auth.token_file = "nonexistent/token".to_string();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(start_if_enabled(dir.path(), &config, fresh_cancel()));
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
        let runtime = start_if_enabled(temp.path(), &config, fresh_cancel())
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
        let runtime = start_if_enabled(temp.path(), &config, fresh_cancel())
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
        // Scheme A: `accepting` follows the Reactor's cancellation token.
        // Before cancel → accepting=true; after cancel → accepting=false.
        let temp = tempfile::tempdir().unwrap();
        write_token(temp.path(), "runtime/admin_api.token");
        let mut config = test_config(true);
        config.auth.token_file = "runtime/admin_api.token".to_string();
        let cancel = fresh_cancel();
        let runtime = start_if_enabled(temp.path(), &config, cancel.clone())
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
        let err = start_if_enabled(temp.path(), &config, fresh_cancel())
            .await
            .expect_err("should reject too-permissive token file");
        assert!(
            err.contains("too permissive"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn request_id_echoed_from_header() {
        // X-Request-Id header is echoed back in the 401 response body.
        let temp = tempfile::tempdir().unwrap();
        write_token(temp.path(), "runtime/admin_api.token");
        let mut config = test_config(true);
        config.auth.token_file = "runtime/admin_api.token".to_string();
        let runtime = start_if_enabled(temp.path(), &config, fresh_cancel())
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
        let runtime = start_if_enabled(temp.path(), &config, fresh_cancel())
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
        assert!(status.success(), "openssl failed to generate self-signed cert");
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
        let err = start_if_enabled(temp.path(), &config, fresh_cancel())
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
        let runtime = start_if_enabled(temp.path(), &config, fresh_cancel())
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
        let runtime = start_if_enabled(temp.path(), &config, fresh_cancel())
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
}
