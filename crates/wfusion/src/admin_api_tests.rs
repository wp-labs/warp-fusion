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
    let resp = json_response(StatusCode::OK, &serde_json::json!({"key":"value"}));
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
    assert_eq!(body["accepting_commands"], true);

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
    assert_eq!(body["accepting_commands"], true);

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
    assert_eq!(body["accepting_commands"], false);

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
async fn rejects_non_loopback_when_tls_disabled() {
    let temp = tempfile::tempdir().unwrap();
    write_token(temp.path(), "runtime/admin_api.token");
    let mut config = test_config(true);
    config.bind = "0.0.0.0:0".to_string();
    config.auth.token_file = "runtime/admin_api.token".to_string();
    config.tls.enabled = false;
    let err = start_if_enabled(
        temp.path(),
        &config,
        test_control_handle(),
        test_config_source(temp.path()),
    )
    .await
    .expect_err("should reject non-loopback without tls");
    assert!(
        err.contains("requires admin_api.tls.enabled=true"),
        "unexpected error: {err}"
    );
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
stream_tag = "syslog"
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
    stream_tag = "syslog"
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
        .body(r#"{}"#)
        .send()
        .await
        .expect("post reload");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["accepted"], true);
    assert_eq!(body["result"], "reload_done");

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
        .body(r#"{}"#)
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["accepted"], true);
    assert_eq!(body["result"], "reload_done");

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
        .body(r#"{}"#)
        .send()
        .await
        .expect("post");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "reload should be applied despite CLI mode override"
    );
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["result"], "reload_done");

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
        .body(r#"{}"#)
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
    assert_eq!(body["result"], "reload_failed");
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
            .body(r#"{}"#)
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

// -- publish/reload -------------------------------------------------

/// A hot-reloadable change returns 200 with the wparse-aligned reload result.
#[tokio::test]
async fn reload_hot_applied_returns_200() {
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
        .body(r#"{}"#)
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["accepted"], true);
    assert_eq!(body["result"], "reload_done");
    servant.shutdown().await;
}

/// A change that requires restart is reported as a non-failure pending state;
/// Admin API does not trigger restart in the wparse-aligned publish protocol.
#[tokio::test]
async fn reload_blocked_returns_restart_required() {
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
        .body(r#"{}"#)
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["accepted"], true);
    assert_eq!(body["result"], "restart_required");
    assert!(
        body["warning"]
            .as_str()
            .unwrap_or_default()
            .contains("restart")
    );
    assert_eq!(body["error"], serde_json::Value::Null);
    servant.shutdown().await;
}

/// With a broken config, the reload is rejected without triggering restart.
#[tokio::test]
async fn reload_broken_config_returns_error() {
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
        .body(r#"{}"#)
        .send()
        .await
        .expect("post");
    // Must NOT be 202 (no restart), must be an error (5xx).
    assert_ne!(resp.status(), reqwest::StatusCode::ACCEPTED);
    assert!(resp.status().is_server_error());
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["accepted"], false);
    assert_eq!(body["result"], "reload_failed");
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
    assert!(
        body.get("reloading").is_some(),
        "status must include reloading"
    );
    assert_eq!(body["reloading"], false);
    servant.shutdown().await;
}

#[tokio::test]
async fn reload_wait_false_clears_reloading_when_done() {
    let (_temp, base, servant) = boot_engine_with_admin(BRUTE_FORCE_RULE).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let resp = client
        .post(format!("{base}/admin/v1/reloads/model"))
        .bearer_auth("test-token")
        .body(r#"{"wait": false}"#)
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["result"], "running");

    wait_for_reload_result(&client, &base, "reload_done").await;

    servant.shutdown().await;
}

#[tokio::test]
async fn reload_update_disabled_returns_502() {
    // No [project_remote] in the fixture config → update is rejected.
    let (_temp, base, servant) = boot_engine_with_admin(BRUTE_FORCE_RULE).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let resp = client
        .post(format!("{base}/admin/v1/reloads/model"))
        .bearer_auth("test-token")
        .body(r#"{"update": true, "version": "1.0.1"}"#)
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), reqwest::StatusCode::INTERNAL_SERVER_ERROR);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["accepted"], false);
    assert_eq!(body["result"], "update_failed");
    let err = body["error"].as_str().expect("error string");
    assert!(
        err.contains("disabled"),
        "expected disabled error, got: {err}"
    );
    servant.shutdown().await;
}

#[tokio::test]
async fn reload_update_unknown_version_returns_502() {
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
        .body(r#"{"update": true, "version": "9.9.9"}"#)
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), reqwest::StatusCode::INTERNAL_SERVER_ERROR);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["accepted"], false);
    assert_eq!(body["result"], "update_failed");
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

// ── e2e: remote sync → daemon reload (full success path) ─────────────

/// Create a local git remote with two releases:
///   v1.0.0 — initial rule (score 70)
///   v1.0.1 — updated rule (score 99)
///
/// The remote carries `models/schemas/security.wfs`, `models/rules/v1/brute_force.wfl`,
/// and `models/version.txt`, so a `sync_project_remote` into the work root
/// populates the same paths the engine reads.
fn create_rule_remote_fixture() -> wf_project_remote::test_support::RemoteFixture {
    use wf_project_remote::test_support::RemoteFixture;
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = git2::Repository::init(temp.path()).expect("init remote repo");

    // The remote's wfusion.toml must contain ALL sections the engine needs,
    // including admin_api and project_remote. Since the sync overwrites
    // conf/wfusion.toml, the reloaded config must match the booted config
    // (except for rules/models content which lives outside conf/).
    let remote_url = temp.path().to_str().expect("path utf8");

    // v1.0.0 — initial release
    let models = temp.path().join("models");
    std::fs::create_dir_all(models.join("schemas")).unwrap();
    std::fs::create_dir_all(models.join("rules/v1")).unwrap();
    std::fs::create_dir_all(temp.path().join("conf")).unwrap();
    std::fs::create_dir_all(temp.path().join("topology")).unwrap();
    std::fs::create_dir_all(temp.path().join("connectors")).unwrap();
    std::fs::write(models.join("version.txt"), "1.0.0\n").unwrap();
    std::fs::write(models.join("schemas/security.wfs"), SECURITY_SCHEMA).unwrap();
    std::fs::write(models.join("rules/v1/brute_force.wfl"), BRUTE_FORCE_RULE).unwrap();
    // Full config — identical across releases (only models/ content changes).
    std::fs::write(
        temp.path().join("conf/wfusion.toml"),
        remote_config_toml(remote_url),
    )
    .unwrap();
    std::fs::write(
        temp.path().join("models/windows.toml"),
        AUTH_SECURITY_WINDOWS_TOML,
    )
    .unwrap();
    git_commit_all(&repo, "release 1.0.0");
    git_tag_head(&repo, "v1.0.0");

    // v1.0.1 — updated rule (different score → rule-only change, hot-reloadable)
    std::fs::write(models.join("version.txt"), "1.0.1\n").unwrap();
    std::fs::write(models.join("rules/v1/brute_force.wfl"), BRUTE_FORCE_RULE_V2).unwrap();
    git_commit_all(&repo, "release 1.0.1");
    git_tag_head(&repo, "v1.0.1");

    // v1.0.2 — changes window layout, so the runtime reload is blocked and
    // Admin API must roll the project back to the previous release.
    std::fs::write(models.join("version.txt"), "1.0.2\n").unwrap();
    std::fs::write(
        temp.path().join("models/windows.toml"),
        AUTH_SECURITY_WINDOWS_TOML.replace("over_cap = \"30m\"", "over_cap = \"1h\""),
    )
    .unwrap();
    git_commit_all(&repo, "release 1.0.2");
    git_tag_head(&repo, "v1.0.2");

    let remote_path = temp.path().to_path_buf();
    RemoteFixture::from_parts(temp, remote_path)
}

/// The remote's wfusion.toml — must be byte-identical across releases so
/// sync doesn't cause a config diff. Contains all sections including
/// admin_api and project_remote (pointing at the remote's own path).
fn remote_config_toml(remote_url: &str) -> String {
    format!(
        r#"mode = "daemon"
windows = "models/windows.toml"
sinks = "sinks"

[[sources]]
type = "file"
name = "seed"
path = "seed.ndjson"
stream_tag = "syslog"
data_format = "ndjson"

[runtime]
executor_parallelism = 2
rule_exec_timeout = "30s"
schemas = "models/schemas/*.wfs"
rules = "models/rules/v1/*.wfl"

[vars]
FAIL_THRESHOLD = "3"

[admin_api]
enabled = true
bind = "127.0.0.1:0"

[admin_api.auth]
token_file = "runtime/admin_api.token"

[project_remote]
enabled = true
repo = '{remote_url}'
init_version = '1.0.0'
"#
    )
}

fn dual_remote_config_toml(repo_url: &str) -> String {
    format!(
        r#"mode = "daemon"
windows = "models/windows.toml"
sinks = "sinks"

[[sources]]
type = "file"
name = "seed"
path = "seed.ndjson"
stream_tag = "syslog"
data_format = "ndjson"

[runtime]
executor_parallelism = 2
rule_exec_timeout = "30s"
schemas = "models/schemas/*.wfs"
rules = "models/rules/v1/*.wfl"

[vars]
FAIL_THRESHOLD = "3"

[admin_api]
enabled = true
bind = "127.0.0.1:0"

[admin_api.auth]
token_file = "runtime/admin_api.token"

[project_remote]
enabled = true

[project_remote.models]
repo = '{repo_url}'
init_version = '1.0.0'

[project_remote.infra]
repo = '{repo_url}'
init_version = '1.0.0'
"#
    )
}

fn git_commit_all(repo: &git2::Repository, message: &str) {
    let mut index = repo.index().expect("open index");
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .expect("add all");
    index.write().expect("write index");
    let tree_id = index.write_tree().expect("write tree");
    let tree = repo.find_tree(tree_id).expect("find tree");
    let sig = git2::Signature::now("e2e-test", "e2e@test.local").expect("signature");
    let parent = repo
        .head()
        .ok()
        .and_then(|head| head.target())
        .and_then(|oid| repo.find_commit(oid).ok());
    match parent.as_ref() {
        Some(parent) => repo
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &[parent])
            .expect("commit"),
        None => repo
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &[])
            .expect("initial commit"),
    };
}

fn git_tag_head(repo: &git2::Repository, tag: &str) {
    let obj = repo
        .head()
        .expect("head")
        .peel(git2::ObjectType::Commit)
        .expect("peel head");
    repo.tag_lightweight(tag, &obj, false).expect("create tag");
}

/// Boot a real engine whose `models/` directory is managed by
/// `[project_remote]`. The work root is synced from the remote at v1.0.0
/// before boot, so the engine loads the initial rules. A subsequent
/// `update` reload fetches v1.0.1 (different rule score) and the
/// daemon applies the hot swap.
async fn boot_engine_with_remote_rules() -> (tempfile::TempDir, String, RuntimeServant) {
    let remote = create_rule_remote_fixture();
    let work = tempfile::tempdir().unwrap();
    let work_root = work.path();

    // Sync initial release (v1.0.0) into the work root.
    let conf = wf_project_remote::test_support::single_conf(remote.repo_url(), "1.0.0");
    wf_project_remote::sync_project_remote(work_root, &conf, None).expect("initial sync");

    // Write sink layout + token + seed (not in the remote repo).
    write_sink_layout(work_root);
    std::fs::create_dir_all(work_root.join("runtime")).unwrap();
    write_token(work_root, "runtime/admin_api.token");
    std::fs::write(work_root.join("seed.ndjson"), "").unwrap();

    // The synced conf/wfusion.toml already contains admin_api + project_remote
    // (from the remote repo). Boot the engine directly.
    use wf_config::{ConfigVarContext, FusionConfigLoader};
    use wf_runtime::lifecycle::Reactor;
    let cfg_path = work_root.join("conf/wfusion.toml");
    let ctx = ConfigVarContext::new();
    let loader = FusionConfigLoader::new(&cfg_path, &[], &ctx, Some(work_root));
    let raw = loader.load_raw().expect("raw");
    let mut fusion_config = loader.load().expect("config");
    // Override bind to ephemeral port (remote config has 127.0.0.1:0 but
    // FusionConfig may have resolved it differently).
    fusion_config.admin_api.bind = "127.0.0.1:0".to_string();
    let admin_conf = fusion_config.admin_api.clone();
    let reactor = Reactor::start(fusion_config, raw, work_root)
        .await
        .expect("start");
    let control = reactor.control_handle();
    let cancel = control.cancel_token();
    let config_source = ReloadConfigSource::new(cfg_path, Vec::new(), ctx, work_root.to_path_buf());
    let admin = start_if_enabled(work_root, &admin_conf, control, config_source)
        .await
        .expect("start admin")
        .expect("admin enabled");
    let base = format!("http://{}", admin.local_addr());
    std::mem::forget(admin);
    // Keep the remote alive for the duration of the test.
    std::mem::forget(remote);
    let run_task = tokio::spawn(async move { reactor.run().await });
    (work, base, RuntimeServant { run_task, cancel })
}

#[tokio::test]
async fn reload_update_success_applies_new_rules() {
    // Full e2e: daemon boots on v1.0.0 rules → POST reload with
    // update=true → git fetch v1.0.1 (score 70→99) → engine applies
    // the hot swap → 200 + result=applied.
    let (_temp, base, servant) = boot_engine_with_remote_rules().await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let resp = client
        .post(format!("{base}/admin/v1/reloads/model"))
        .bearer_auth("test-token")
        .body(r#"{"update": true, "version": "1.0.1"}"#)
        .send()
        .await
        .expect("post");

    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "expected 200, got {status}: {body}"
    );
    assert_eq!(body["accepted"], true, "expected accepted=true: {body}");
    assert_eq!(
        body["result"], "reload_done",
        "expected result=applied: {body}"
    );

    servant.shutdown().await;
}

#[tokio::test]
async fn reload_update_blocked_keeps_synced_project() {
    let (temp, base, servant) = boot_engine_with_remote_rules().await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let resp = client
        .post(format!("{base}/admin/v1/reloads/model"))
        .bearer_auth("test-token")
        .body(r#"{"update": true, "version": "1.0.2"}"#)
        .send()
        .await
        .expect("post");

    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "expected 200, got {status}: {body}"
    );
    assert_eq!(body["accepted"], true);
    assert_eq!(body["result"], "restart_required");
    assert_eq!(body["error"], serde_json::Value::Null);

    let state = std::fs::read_to_string(temp.path().join(".run/project_remote_state.json"))
        .expect("state file");
    assert!(
        state.contains(r#""current_version": "1.0.2""#),
        "project state should keep v1.0.2 pending restart: {state}"
    );
    let version_txt = std::fs::read_to_string(temp.path().join("models/version.txt"))
        .expect("models version marker");
    assert_eq!(version_txt, "1.0.2\n");

    servant.shutdown().await;
}

#[tokio::test]
async fn reload_update_wait_false_blocked_keeps_synced_project_in_background() {
    let (temp, base, servant) = boot_engine_with_remote_rules().await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let resp = client
        .post(format!("{base}/admin/v1/reloads/model"))
        .bearer_auth("test-token")
        .body(r#"{"wait": false, "update": true, "version": "1.0.2"}"#)
        .send()
        .await
        .expect("post");

    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(
        status,
        reqwest::StatusCode::ACCEPTED,
        "expected 202, got {status}: {body}"
    );
    assert_eq!(body["result"], "running");

    wait_for_reload_result(&client, &base, "restart_required").await;
    wait_for_project_version(temp.path(), "1.0.2").await;
    let version_txt = std::fs::read_to_string(temp.path().join("models/version.txt"))
        .expect("models version marker");
    assert_eq!(version_txt, "1.0.2\n");

    servant.shutdown().await;
}

#[tokio::test]
async fn reload_update_dual_repo_requires_group() {
    let (temp, base, servant) = boot_engine_with_remote_rules().await;
    let remote = create_rule_remote_fixture();
    std::fs::write(
        temp.path().join("conf/wfusion.toml"),
        dual_remote_config_toml(remote.repo_url()),
    )
    .unwrap();

    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let resp = client
        .post(format!("{base}/admin/v1/reloads/model"))
        .bearer_auth("test-token")
        .body(r#"{"update": true}"#)
        .send()
        .await
        .expect("post");

    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(
        status,
        reqwest::StatusCode::BAD_REQUEST,
        "expected 400, got {status}: {body}"
    );
    assert_eq!(body["result"], "invalid_request");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("requires group"),
        "unexpected body: {body}"
    );

    let status_resp = client
        .get(format!("{base}/admin/v1/runtime/status"))
        .bearer_auth("test-token")
        .send()
        .await
        .expect("status");
    assert_eq!(status_resp.status(), reqwest::StatusCode::OK);
    let status_body: serde_json::Value = status_resp.json().await.expect("status json");
    assert_eq!(status_body["reloading"], false);
    assert_eq!(status_body["current_request_id"], serde_json::Value::Null);
    assert_eq!(status_body["last_reload_result"], serde_json::Value::Null);

    servant.shutdown().await;
}

#[tokio::test]
async fn reload_update_lock_conflict_returns_409() {
    let (temp, base, servant) = boot_engine_with_remote_rules().await;
    let _lock_guard =
        wf_project_remote::acquire_project_remote_lock(temp.path()).expect("hold remote lock");
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let resp = client
        .post(format!("{base}/admin/v1/reloads/model"))
        .bearer_auth("test-token")
        .body(r#"{"update": true, "version": "1.0.1"}"#)
        .send()
        .await
        .expect("post");

    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(
        status,
        reqwest::StatusCode::CONFLICT,
        "expected 409, got {status}: {body}"
    );
    assert_eq!(body["result"], "update_in_progress");
    assert_eq!(body["accepted"], false);

    let status_resp = client
        .get(format!("{base}/admin/v1/runtime/status"))
        .bearer_auth("test-token")
        .send()
        .await
        .expect("status");
    assert_eq!(status_resp.status(), reqwest::StatusCode::OK);
    let status_body: serde_json::Value = status_resp.json().await.expect("status json");
    assert_eq!(status_body["reloading"], false);
    assert_eq!(status_body["last_reload_result"], "update_in_progress");

    servant.shutdown().await;
}

async fn wait_for_project_version(work_root: &Path, expected: &str) {
    let state_path = work_root.join(".run/project_remote_state.json");
    for _ in 0..40 {
        if let Ok(state) = std::fs::read_to_string(&state_path)
            && state.contains(&format!(r#""current_version": "{expected}""#))
        {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let state = std::fs::read_to_string(&state_path).unwrap_or_else(|err| err.to_string());
    panic!("project state did not become {expected}: {state}");
}

async fn wait_for_reload_result(client: &reqwest::Client, base: &str, expected: &str) {
    for _ in 0..40 {
        let resp = client
            .get(format!("{base}/admin/v1/runtime/status"))
            .bearer_auth("test-token")
            .send()
            .await
            .expect("status");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("status json");
        if body["last_reload_result"] == expected {
            assert_eq!(body["reloading"], false);
            assert_eq!(body["current_request_id"], serde_json::Value::Null);
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("reload result did not become {expected}");
}
