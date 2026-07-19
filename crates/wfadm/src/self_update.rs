// wfadm self update — download latest binary from an update manifest
//
// Reads the selected channel manifest, downloads the archive for the current
// platform, and replaces the running binary with the matching binary from that
// archive.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};

const DEFAULT_UPDATES_RAW_BASE_URL: &str = "https://raw.githubusercontent.com/wp-labs/warp-fusion";
const UPDATES_BASE_URL_ENV: &str = "WFUSION_UPDATES_BASE_URL";
const UPDATES_ROOT_ENV: &str = "WFUSION_UPDATES_ROOT";

#[derive(Subcommand, Debug, Clone)]
#[command(
    name = "self",
    about = "WarpFusion 自更新工具 | WarpFusion self-update tools"
)]
pub enum SelfCmd {
    /// 下载并安装新版本 | Download and install the latest release
    #[command(
        name = "update",
        visible_alias = "更新",
        disable_version_flag = true,
        about = "下载并安装新版本 | Download and install the latest release"
    )]
    Update(SelfUpdateArgs),
}

#[derive(Args, Debug, Clone)]
pub struct SelfSourceArgs {
    /// 更新通道 | Update channel
    #[arg(
        long = "channel",
        value_enum,
        default_value_t = UpdateChannel::Stable,
        visible_alias = "通道",
        help = "更新通道：stable|beta|alpha（默认 stable）| Update channel: stable|beta|alpha (default: stable)"
    )]
    pub channel: UpdateChannel,

    /// 远端 manifest 基础地址 | Remote manifest base URL
    #[arg(
        long = "updates-base-url",
        visible_alias = "updates基地址",
        help = "远端 manifest 基础地址（默认按 channel 选择 warp-fusion 分支 updates 根；最终拼成 {channel}/manifest.json）| Remote manifest base URL (defaults to the warp-fusion channel branch updates root; resolved as {channel}/manifest.json)"
    )]
    pub updates_base_url: Option<String>,

    /// 本地 manifest 根目录覆盖 | Local manifest root override
    #[arg(
        long = "updates-root",
        visible_alias = "updates目录",
        help = "本地 manifest 根目录覆盖（最终拼成 {channel}/manifest.json）| Local manifest root override (resolved as {channel}/manifest.json)"
    )]
    pub updates_root: Option<PathBuf>,

    /// JSON 输出 | JSON output
    #[arg(
        long = "json",
        default_value_t = false,
        visible_alias = "输出JSON",
        help = "JSON 输出 | JSON output"
    )]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct SelfUpdateArgs {
    #[command(flatten)]
    pub source: SelfSourceArgs,

    /// 自动确认安装 | Skip confirmation prompt
    #[arg(
        long = "yes",
        default_value_t = false,
        visible_alias = "确认",
        help = "自动确认安装 | Skip confirmation prompt"
    )]
    pub yes: bool,

    /// 仅输出将执行的动作，不真正下载/替换 | Print planned actions without applying changes
    #[arg(
        long = "dry-run",
        default_value_t = false,
        visible_alias = "演练",
        help = "仅输出将执行的动作，不真正下载/替换 | Print planned actions without applying changes"
    )]
    pub dry_run: bool,

    /// 强制继续 | Force update
    #[arg(
        long = "force",
        default_value_t = false,
        visible_alias = "强制",
        help = "强制继续（例如版本未前进或疑似包管理器安装）| Force update even when safeguards would stop it"
    )]
    pub force: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UpdateChannel {
    Stable,
    Beta,
    Alpha,
}

impl UpdateChannel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Beta => "beta",
            Self::Alpha => "alpha",
        }
    }

    fn default_branch(self) -> &'static str {
        match self {
            Self::Stable => "main",
            Self::Beta => "beta",
            Self::Alpha => "alpha",
        }
    }
}

impl fmt::Display for UpdateChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

pub fn run_self(command: SelfCmd) -> Result<(), String> {
    match command {
        SelfCmd::Update(args) => run_update(args),
    }
}

pub fn run_update(args: SelfUpdateArgs) -> Result<(), String> {
    let current_exe =
        std::env::current_exe().map_err(|e| format!("cannot get current exe path: {e}"))?;
    let current_binary_name = current_binary_name(&current_exe)?;
    let target = detect_target()?;
    let source = resolve_manifest_source(&args.source);
    let manifest = fetch_manifest(&source)?;
    let asset = select_asset(&manifest, target)?;
    let current_version = env!("CARGO_PKG_VERSION");
    let relation = version_relation(current_version, &manifest.version);
    let artifact = url_file_name(&asset.url);
    let install_path = current_exe.display().to_string();

    let mut report = SelfUpdateReport {
        product: "warp-fusion".to_string(),
        binary: current_binary_name.clone(),
        channel: args.source.channel.as_str().to_string(),
        source: source.display(),
        target: target.to_string(),
        current_version: current_version.to_string(),
        latest_version: manifest.version.clone(),
        artifact,
        install_path,
        backup_path: None,
        status: "planned".to_string(),
        updated: false,
        dry_run: args.dry_run,
        forced: args.force,
    };

    if should_skip_for_version(relation) && !args.force {
        report.status = relation.status().to_string();
        return print_report(&report, args.source.json);
    }

    if args.dry_run {
        report.status = "dry-run".to_string();
        return print_report(&report, args.source.json);
    }

    if !args.yes {
        if args.source.json {
            report.status = "confirmation-required".to_string();
            return print_report(&report, true);
        }
        if !confirm_update(&report)? {
            report.status = "aborted".to_string();
            return print_report(&report, false);
        }
    }

    let tmp = download_asset(&asset.url)?;
    let updated_bin = extract_binary(&tmp, &current_binary_name)?;
    let backup = install_binary(&updated_bin, &current_exe, &current_binary_name)?;

    report.backup_path = Some(backup.display().to_string());
    report.status = "updated".to_string();
    report.updated = true;
    print_report(&report, args.source.json)
}

fn should_skip_for_version(relation: VersionRelation) -> bool {
    matches!(
        relation,
        VersionRelation::UpToDate | VersionRelation::AheadOfChannel
    )
}

fn confirm_update(report: &SelfUpdateReport) -> Result<bool, String> {
    print_update_plan(report);
    print!("Proceed with install? [y/N] ");
    io::stdout()
        .flush()
        .map_err(|e| format!("flush stdout: {e}"))?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|e| format!("read confirmation: {e}"))?;

    Ok(matches!(
        input.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn print_report(report: &SelfUpdateReport, json: bool) -> Result<(), String> {
    if json {
        let text = serde_json::to_string_pretty(report).map_err(|e| format!("JSON output: {e}"))?;
        println!("{text}");
        return Ok(());
    }

    match report.status.as_str() {
        "dry-run" => {
            println!("wfadm self update dry run");
            print_update_plan(report);
        }
        "updated" => {
            println!("wfadm self update complete");
            print_common_report(report);
            if let Some(backup) = report.backup_path.as_deref() {
                println!("  Backup  : {backup}");
            }
        }
        "aborted" => {
            println!("wfadm self update aborted");
        }
        "up-to-date" | "ahead-of-channel" => {
            println!("wfadm self update skipped");
            print_common_report(report);
            println!("  Status  : {}", report.status);
        }
        "confirmation-required" => {
            println!("wfadm self update requires confirmation");
            print_common_report(report);
            println!("  Status  : pass --yes to install non-interactively");
        }
        _ => {
            println!("wfadm self update");
            print_common_report(report);
            println!("  Status  : {}", report.status);
        }
    }
    Ok(())
}

fn print_update_plan(report: &SelfUpdateReport) {
    println!("wfadm self update");
    print_common_report(report);
    println!("  Status  : {}", report.status);
}

fn print_common_report(report: &SelfUpdateReport) {
    println!("  Binary  : {}", report.binary);
    println!("  Install : {}", report.install_path);
    println!("  Channel : {}", report.channel);
    println!("  Manifest: {}", report.source);
    println!("  Target  : {}", report.target);
    println!("  Current : {}", report.current_version);
    println!(
        "  Latest  : {}",
        render_latest_version(&report.current_version, &report.latest_version)
    );
    println!("  Artifact: {}", report.artifact);
}

fn render_latest_version(current: &str, latest: &str) -> String {
    render_latest_version_with_color(current, latest, should_use_color())
}

fn render_latest_version_with_color(current: &str, latest: &str, use_color: bool) -> String {
    if version_relation(current, latest) == VersionRelation::AheadOfChannel {
        return render_dim_with_color(latest, use_color);
    }
    latest.to_string()
}

fn render_dim_with_color(value: &str, use_color: bool) -> String {
    if use_color {
        format!("\x1b[90m{value}\x1b[0m")
    } else {
        value.to_string()
    }
}

fn should_use_color() -> bool {
    std::env::var_os("NO_COLOR").is_none()
        && std::env::var("TERM")
            .map(|term| term != "dumb")
            .unwrap_or(true)
}

fn detect_target() -> Result<&'static str, String> {
    detect_target_parts(std::env::consts::ARCH, std::env::consts::OS)
}

fn detect_target_parts(arch: &str, os: &str) -> Result<&'static str, String> {
    match (arch, os) {
        ("aarch64", "macos") => Ok("aarch64-apple-darwin"),
        ("aarch64", "linux") => Ok("aarch64-unknown-linux-gnu"),
        ("x86_64", "linux") => Ok("x86_64-unknown-linux-gnu"),
        _ => Err(format!(
            "unsupported self-update target '{arch}-{os}'. Supported targets: {}",
            supported_targets().join(", ")
        )),
    }
}

fn supported_targets() -> Vec<&'static str> {
    vec![
        "aarch64-apple-darwin",
        "aarch64-unknown-linux-gnu",
        "x86_64-unknown-linux-gnu",
    ]
}

fn current_binary_name(current_exe: &Path) -> Result<String, String> {
    current_exe
        .file_name()
        .and_then(OsStr::to_str)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            format!(
                "cannot determine current binary name: {}",
                current_exe.display()
            )
        })
}

fn install_binary(
    updated_bin: &Path,
    current_exe: &Path,
    binary_name: &str,
) -> Result<PathBuf, String> {
    let parent = current_exe
        .parent()
        .ok_or("cannot determine binary directory")?;
    let backup = backup_path(parent, binary_name);

    std::fs::rename(current_exe, &backup)
        .map_err(|e| format!("cannot backup current binary: {e}"))?;
    if let Err(e) = std::fs::copy(updated_bin, current_exe) {
        let _ = std::fs::rename(&backup, current_exe);
        return Err(format!("cannot install new binary: {e}"));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(current_exe)
            .map_err(|e| format!("metadata: {e}"))?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(current_exe, perms).map_err(|e| format!("chmod: {e}"))?;
    }

    Ok(backup)
}

fn backup_path(parent: &Path, binary_name: &str) -> PathBuf {
    let default = parent.join(format!("{binary_name}.bak"));
    if !default.exists() {
        return default;
    }
    parent.join(format!("{binary_name}.bak.{}", std::process::id()))
}

// ----- Manifest helpers -----

#[derive(Deserialize)]
struct UpdateManifest {
    version: String,
    assets: BTreeMap<String, ManifestAsset>,
}

#[derive(Debug, Deserialize)]
struct ManifestAsset {
    url: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
enum ManifestSource {
    Remote { url: String },
    Local { path: PathBuf },
}

impl ManifestSource {
    fn display(&self) -> String {
        match self {
            Self::Remote { url } => url.clone(),
            Self::Local { path } => path.display().to_string(),
        }
    }
}

fn resolve_manifest_source(source: &SelfSourceArgs) -> ManifestSource {
    if let Some(root) = source
        .updates_root
        .clone()
        .or_else(|| std::env::var_os(UPDATES_ROOT_ENV).map(PathBuf::from))
    {
        return ManifestSource::Local {
            path: root.join(source.channel.as_str()).join("manifest.json"),
        };
    }

    let base = source
        .updates_base_url
        .clone()
        .or_else(|| std::env::var(UPDATES_BASE_URL_ENV).ok())
        .unwrap_or_else(|| default_updates_base_url(source.channel));

    ManifestSource::Remote {
        url: format!(
            "{}/{}/manifest.json",
            base.trim_end_matches('/'),
            source.channel.as_str()
        ),
    }
}

fn default_updates_base_url(channel: UpdateChannel) -> String {
    format!(
        "{}/{}/updates",
        DEFAULT_UPDATES_RAW_BASE_URL,
        channel.default_branch()
    )
}

fn fetch_manifest(source: &ManifestSource) -> Result<UpdateManifest, String> {
    let body = match source {
        ManifestSource::Remote { url } => http_get_json(url)?,
        ManifestSource::Local { path } => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("read update manifest {}: {e}", path.display()))?;
            serde_json::from_str(&text).map_err(|e| format!("parse JSON: {e}"))?
        }
    };
    serde_json::from_value(body).map_err(|e| format!("parse update manifest: {e}"))
}

fn select_asset<'a>(
    manifest: &'a UpdateManifest,
    target: &str,
) -> Result<&'a ManifestAsset, String> {
    manifest.assets.get(target).ok_or_else(|| {
        let mut available: Vec<&str> = manifest.assets.keys().map(String::as_str).collect();
        available.sort_unstable();
        format!(
            "no update asset found for target '{target}' in manifest v{}. Available targets: {}",
            manifest.version,
            available.join(", ")
        )
    })
}

fn download_asset(url: &str) -> Result<PathBuf, String> {
    let tmp = std::env::temp_dir().join(format!("wfadm_update_{}.tar.gz", std::process::id()));
    let resp = ureq::get(url)
        .call()
        .map_err(|e| format!("download failed: {e}"))?;

    let mut reader = resp.into_body().into_reader();
    let mut file =
        std::fs::File::create(&tmp).map_err(|e| format!("cannot create temp file: {e}"))?;
    io::copy(&mut reader, &mut file).map_err(|e| format!("download write error: {e}"))?;

    Ok(tmp)
}

fn extract_binary(tarball: &Path, binary_name: &str) -> Result<PathBuf, String> {
    let out_dir = std::env::temp_dir().join(format!("wfadm_extract_{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("cannot create extract dir: {e}"))?;

    let f = std::fs::File::open(tarball).map_err(|e| format!("cannot open tarball: {e}"))?;
    let decoder = flate2::read::GzDecoder::new(f);
    let mut archive = tar::Archive::new(decoder);
    archive
        .unpack(&out_dir)
        .map_err(|e| format!("extract error: {e}"))?;

    // Find the requested binary in extracted tree.
    for entry in walkdir::WalkDir::new(&out_dir).into_iter().flatten() {
        if entry.file_name() == binary_name && entry.path().is_file() {
            return Ok(entry.path().to_path_buf());
        }
    }

    Err(format!("{binary_name} binary not found in release archive"))
}

// ----- Version helpers -----

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum VersionRelation {
    UpdateAvailable,
    UpToDate,
    AheadOfChannel,
}

impl VersionRelation {
    fn status(self) -> &'static str {
        match self {
            Self::UpdateAvailable => "update-available",
            Self::UpToDate => "up-to-date",
            Self::AheadOfChannel => "ahead-of-channel",
        }
    }
}

fn version_relation(current: &str, latest: &str) -> VersionRelation {
    let current = current.trim_start_matches('v');
    let latest = latest.trim_start_matches('v');

    match (
        semver::Version::parse(current),
        semver::Version::parse(latest),
    ) {
        (Ok(current), Ok(latest)) => match current.cmp(&latest) {
            Ordering::Less => VersionRelation::UpdateAvailable,
            Ordering::Equal => VersionRelation::UpToDate,
            Ordering::Greater => VersionRelation::AheadOfChannel,
        },
        _ if current == latest => VersionRelation::UpToDate,
        _ => VersionRelation::UpdateAvailable,
    }
}

fn url_file_name(url: &str) -> String {
    url.split('?')
        .next()
        .unwrap_or(url)
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(url)
        .to_string()
}

// ----- JSON HTTP helper -----

fn http_get_json(url: &str) -> Result<serde_json::Value, String> {
    let resp = ureq::get(url)
        .header("Accept", "application/json")
        .header("User-Agent", "wfadm/self-update")
        .call()
        .map_err(|e| format!("HTTP request failed: {e}"))?;

    let body_str = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("read response body: {e}"))?;

    serde_json::from_str(&body_str).map_err(|e| format!("parse JSON: {e}"))
}

#[derive(Debug, Serialize)]
struct SelfUpdateReport {
    product: String,
    binary: String,
    channel: String,
    source: String,
    target: String,
    current_version: String,
    latest_version: String,
    artifact: String,
    install_path: String,
    backup_path: Option<String>,
    status: String,
    updated: bool,
    dry_run: bool,
    forced: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(channel: UpdateChannel) -> SelfSourceArgs {
        SelfSourceArgs {
            channel,
            updates_base_url: None,
            updates_root: None,
            json: false,
        }
    }

    #[test]
    fn target_detection_uses_release_manifest_triples() {
        assert_eq!(
            detect_target_parts("aarch64", "macos").unwrap(),
            "aarch64-apple-darwin"
        );
        assert_eq!(
            detect_target_parts("aarch64", "linux").unwrap(),
            "aarch64-unknown-linux-gnu"
        );
        assert_eq!(
            detect_target_parts("x86_64", "linux").unwrap(),
            "x86_64-unknown-linux-gnu"
        );
    }

    #[test]
    fn unsupported_target_is_actionable() {
        let err = detect_target_parts("x86_64", "macos").unwrap_err();
        assert!(err.contains("unsupported self-update target 'x86_64-macos'"));
        assert!(err.contains("aarch64-apple-darwin"));
        assert!(err.contains("x86_64-unknown-linux-gnu"));
    }

    #[test]
    fn default_manifest_source_uses_channel_manifest_url() {
        let manifest = resolve_manifest_source(&source(UpdateChannel::Stable));
        assert_eq!(
            manifest,
            ManifestSource::Remote {
                url: "https://raw.githubusercontent.com/wp-labs/warp-fusion/main/updates/stable/manifest.json".to_string()
            }
        );

        let manifest = resolve_manifest_source(&source(UpdateChannel::Alpha));
        assert_eq!(
            manifest,
            ManifestSource::Remote {
                url: "https://raw.githubusercontent.com/wp-labs/warp-fusion/alpha/updates/alpha/manifest.json".to_string()
            }
        );

        let manifest = resolve_manifest_source(&source(UpdateChannel::Beta));
        assert_eq!(
            manifest,
            ManifestSource::Remote {
                url: "https://raw.githubusercontent.com/wp-labs/warp-fusion/beta/updates/beta/manifest.json".to_string()
            }
        );
    }

    #[test]
    fn custom_manifest_base_url_is_trimmed_before_channel_path() {
        let mut args = source(UpdateChannel::Alpha);
        args.updates_base_url = Some("https://example.test/updates/".to_string());

        let manifest = resolve_manifest_source(&args);
        assert_eq!(
            manifest,
            ManifestSource::Remote {
                url: "https://example.test/updates/alpha/manifest.json".to_string()
            }
        );
    }

    #[test]
    fn local_manifest_root_takes_precedence() {
        let mut args = source(UpdateChannel::Beta);
        args.updates_base_url = Some("https://example.test/updates".to_string());
        args.updates_root = Some(PathBuf::from("/tmp/wf-updates"));

        let manifest = resolve_manifest_source(&args);
        assert_eq!(
            manifest,
            ManifestSource::Local {
                path: PathBuf::from("/tmp/wf-updates/beta/manifest.json")
            }
        );
    }

    #[test]
    fn manifest_selects_target_asset() {
        let manifest = UpdateManifest {
            version: "0.1.29".to_string(),
            assets: BTreeMap::from([(
                "aarch64-apple-darwin".to_string(),
                ManifestAsset {
                    url: "https://example.test/warp-fusion-v0.1.29-aarch64-apple-darwin.tar.gz"
                        .to_string(),
                },
            )]),
        };

        let asset = select_asset(&manifest, "aarch64-apple-darwin").unwrap();
        assert!(asset.url.ends_with("aarch64-apple-darwin.tar.gz"));
    }

    #[test]
    fn missing_manifest_asset_lists_available_targets() {
        let manifest = UpdateManifest {
            version: "0.1.29".to_string(),
            assets: BTreeMap::from([(
                "aarch64-apple-darwin".to_string(),
                ManifestAsset {
                    url: "https://example.test/archive.tar.gz".to_string(),
                },
            )]),
        };

        let err = select_asset(&manifest, "x86_64-unknown-linux-gnu").unwrap_err();
        assert!(err.contains("manifest v0.1.29"));
        assert!(err.contains("aarch64-apple-darwin"));
    }

    #[test]
    fn binary_name_is_taken_from_current_exe_path() {
        assert_eq!(
            current_binary_name(Path::new("/opt/bin/wfadm")).unwrap(),
            "wfadm"
        );
    }

    #[test]
    fn version_relation_skips_up_to_date_and_ahead() {
        assert_eq!(
            version_relation("0.1.30", "0.1.30"),
            VersionRelation::UpToDate
        );
        assert_eq!(
            version_relation("0.1.31", "0.1.30"),
            VersionRelation::AheadOfChannel
        );
        assert_eq!(
            version_relation("0.1.29", "0.1.30"),
            VersionRelation::UpdateAvailable
        );
    }

    #[test]
    fn latest_version_is_dimmed_when_channel_is_behind_current() {
        assert_eq!(
            render_latest_version_with_color("0.1.30", "0.1.29", true),
            "\u{1b}[90m0.1.29\u{1b}[0m"
        );
    }

    #[test]
    fn latest_version_is_plain_when_update_is_available() {
        assert_eq!(
            render_latest_version_with_color("0.1.29", "0.1.30", true),
            "0.1.30"
        );
    }

    #[test]
    fn url_file_name_ignores_query_string() {
        assert_eq!(
            url_file_name("https://example.test/path/archive.tar.gz?download=1"),
            "archive.tar.gz"
        );
    }

    #[test]
    fn extracts_requested_binary_from_release_archive() {
        let tmp = tempfile::tempdir().unwrap();
        let tarball = tmp.path().join("release.tar.gz");

        {
            let file = std::fs::File::create(&tarball).unwrap();
            let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            let mut archive = tar::Builder::new(encoder);

            append_bytes(&mut archive, "artifacts/wfusion", b"wrong-binary");
            append_bytes(&mut archive, "artifacts/wfadm", b"right-binary");
            archive.finish().unwrap();
        }

        let extracted = extract_binary(&tarball, "wfadm").unwrap();
        let bytes = std::fs::read(extracted).unwrap();
        assert_eq!(bytes, b"right-binary");
    }

    fn append_bytes(
        archive: &mut tar::Builder<flate2::write::GzEncoder<std::fs::File>>,
        path: &str,
        bytes: &[u8],
    ) {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_cksum();
        archive.append_data(&mut header, path, bytes).unwrap();
    }
}
