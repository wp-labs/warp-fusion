use super::*;

const AUTH_SECURITY_WINDOWS_TOML: &str =
    include_str!("../../../tests/fixtures/auth_security_windows.toml");

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
async fn allows_non_loopback_when_tls_disabled() {
    let temp = tempfile::tempdir().unwrap();
    write_token(temp.path(), "runtime/admin_api.token");
    let mut config = test_config(true);
    config.bind = "0.0.0.0:0".to_string();
    config.auth.token_file = "runtime/admin_api.token".to_string();
    config.tls.enabled = false;
    let runtime = start_if_enabled(
        temp.path(),
        &config,
        test_control_handle(),
        test_config_source(temp.path()),
    )
    .await
    .expect("start non-loopback without tls")
    .expect("enabled");
    assert_eq!(runtime.local_addr().ip().to_string(), "0.0.0.0");
    runtime.shutdown().await;
}

#[tokio::test]
async fn tls_accepts_https_requests() {
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
    std::fs::create_dir_all(root.join("models")).unwrap();
    std::fs::write(root.join("models/windows.toml"), AUTH_SECURITY_WINDOWS_TOML).unwrap();
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

#[tokio::test]
async fn status_includes_reloading_field() {
    let (_temp, base, servant) = boot_engine_with_admin(BRUTE_FORCE_RULE).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let resp = client
        .get(format!("{base}/admin/v1/runtime/status"))
        .bearer_auth("test-token")
        .send()
        .await
        .expect("get status");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert!(body.get("reloading").is_some(), "status must include reloading");
    assert_eq!(body["reloading"], false);
    servant.shutdown().await;
}

#[tokio::test]
async fn reload_update_remote_disabled_returns_502() {
    // No [project_remote] in the fixture config → update_remote is rejected.
    let (_temp, base, servant) = boot_engine_with_admin(BRUTE_FORCE_RULE).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let resp = client
        .post(format!("{base}/admin/v1/reloads/model"))
        .bearer_auth("test-token")
        .body(r#"{"update_remote": true}"#)
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_GATEWAY);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["accepted"], false);
    assert_eq!(body["result"], "error");
    let err = body["error"].as_str().expect("error string");
    assert!(
        err.contains("disabled"),
        "expected disabled error, got: {err}"
    );
    servant.shutdown().await;
}

#[tokio::test]
async fn reload_update_remote_unknown_version_returns_502() {
    // [project_remote] points at a real local git remote; requesting a
    // non-existent version makes the in-process sync fail → 502. This proves
    // the daemon invokes the full run_remote_update path (git fetch + version
    // resolution) before reload.
    let remote = wf_project_remote::test_support::create_remote_fixture();
    let (temp, base, servant) = boot_engine_with_admin(BRUTE_FORCE_RULE).await;
    append_project_remote(temp.path(), remote.repo_url(), "1.4.2");

    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let resp = client
        .post(format!("{base}/admin/v1/reloads/model"))
        .bearer_auth("test-token")
        .body(r#"{"update_remote": true, "version": "9.9.9"}"#)
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_GATEWAY);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["accepted"], false);
    assert_eq!(body["result"], "error");
    servant.shutdown().await;
}

/// Append an enabled single-repo `[project_remote]` section to the fixture's
/// `wfusion.toml` (run_remote_sync re-reads the file from disk). Uses a TOML
/// literal string for the repo path to avoid escaping.
fn append_project_remote(root: &Path, repo_url: &str, init_version: &str) {
    let path = root.join("wfusion.toml");
    let mut content = std::fs::read_to_string(&path).unwrap();
    if !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(&format!(
        "\n[project_remote]\nenabled = true\nrepo = '{repo_url}'\ninit_version = '{init_version}'\n"
    ));
    std::fs::write(&path, content).unwrap();
}
