// wfadm self update/check -- manifest-driven self update, backed by wp-self-update.

use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand, ValueEnum};
use wp_self_update::{
    CheckReport, CheckRequest, SourceConfig, SourceKind, UpdateChannel as CoreChannel,
    UpdateReport, UpdateRequest, UpdateTarget, VersionRelation, check, compare_versions_str,
    relation_message,
};

const PRODUCT_NAME: &str = "warp-fusion";
const DEFAULT_UPDATES_RAW_BASE_URL: &str = "https://raw.githubusercontent.com/wp-labs/warp-fusion";
const UPDATES_BASE_URL_ENV: &str = "WFUSION_UPDATES_BASE_URL";
const UPDATES_ROOT_ENV: &str = "WFUSION_UPDATES_ROOT";
const SUITE_BINS: &[&str] = &["wfusion", "wfgen", "wfl", "wfadm"];

#[derive(Subcommand, Debug, Clone)]
#[command(
    name = "self",
    about = "WarpFusion 自更新工具 | WarpFusion self-update tools"
)]
pub enum SelfCmd {
    /// 检查是否有新版本 | Check whether an update is available
    #[command(
        name = "check",
        visible_alias = "检查",
        disable_version_flag = true,
        about = "检查是否有新版本 | Check whether an update is available"
    )]
    Check(SelfCheckArgs),

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
pub struct SelfCheckArgs {
    #[command(flatten)]
    pub source: SelfSourceArgs,
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

    #[arg(long = "install-dir", hide = true)]
    pub install_dir: Option<PathBuf>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
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
        SelfCmd::Check(args) => run_check(args),
        SelfCmd::Update(args) => run_update(args),
    }
}

pub fn run_check(args: SelfCheckArgs) -> Result<(), String> {
    let source = to_core_source(&args.source);
    let current_version = env!("CARGO_PKG_VERSION").to_string();
    let report = run_async(check(CheckRequest {
        product: PRODUCT_NAME.to_string(),
        source,
        current_version,
        branch: args.source.channel.default_branch().to_string(),
    }))?;

    if args.source.json {
        return print_json(&report);
    }

    let relation = compare_versions_str(&report.current_version, &report.latest_version)
        .map_err(|e| e.to_string())?;
    print_check_report(&report, relation);
    Ok(())
}

pub fn run_update(args: SelfUpdateArgs) -> Result<(), String> {
    let current_exe =
        std::env::current_exe().map_err(|e| format!("cannot get current exe path: {e}"))?;
    let current_binary_name = current_binary_name(&current_exe)?;
    let source = to_core_source(&args.source);
    let report = run_async(wp_self_update::update(UpdateRequest {
        product: PRODUCT_NAME.to_string(),
        target: UpdateTarget::Bins(suite_bins()),
        source,
        current_version: env!("CARGO_PKG_VERSION").to_string(),
        install_dir: args.install_dir,
        yes: args.yes,
        dry_run: args.dry_run,
        force: args.force,
    }))?;

    if args.source.json {
        return print_json(&report);
    }

    print_update_report(&current_binary_name, &report);
    Ok(())
}

fn suite_bins() -> Vec<String> {
    SUITE_BINS.iter().map(|bin| (*bin).to_string()).collect()
}

fn run_async<T>(
    future: impl std::future::Future<Output = Result<T, wp_self_update::UpdateError>>,
) -> Result<T, String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("create async runtime: {e}"))?;
    runtime.block_on(future).map_err(|e| e.to_string())
}

fn to_core_source(source: &SelfSourceArgs) -> SourceConfig {
    let updates_root = source
        .updates_root
        .clone()
        .or_else(|| std::env::var_os(UPDATES_ROOT_ENV).map(PathBuf::from));
    let updates_base_url = source
        .updates_base_url
        .clone()
        .or_else(|| std::env::var(UPDATES_BASE_URL_ENV).ok())
        .unwrap_or_else(|| default_updates_base_url(source.channel));

    SourceConfig {
        channel: to_core_channel(source.channel),
        kind: SourceKind::Manifest {
            updates_base_url,
            updates_root,
        },
    }
}

fn to_core_channel(channel: UpdateChannel) -> CoreChannel {
    match channel {
        UpdateChannel::Stable => CoreChannel::Stable,
        UpdateChannel::Beta => CoreChannel::Beta,
        UpdateChannel::Alpha => CoreChannel::Alpha,
    }
}

fn default_updates_base_url(channel: UpdateChannel) -> String {
    format!(
        "{}/{}/updates",
        DEFAULT_UPDATES_RAW_BASE_URL,
        channel.default_branch()
    )
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

fn print_json<T: serde::Serialize>(value: &T) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| format!("JSON output: {e}"))?;
    println!("{text}");
    Ok(())
}

fn print_check_report(report: &CheckReport, relation: VersionRelation) {
    println!("wfadm self check");
    println!("  Product : {}", report.product);
    println!("  Channel : {}", report.channel);
    println!("  Branch  : {}", report.branch);
    println!("  Manifest: {}", report.source);
    println!("  Format  : {}", report.manifest_format);
    println!("  Target  : {}", report.platform_key);
    println!("  Current : {}", report.current_version);
    println!(
        "  Latest  : {}",
        render_latest_version(&report.latest_version, relation)
    );
    println!("  Artifact: {}", report.artifact);
    println!("  SHA256  : {}", report.sha256);
    println!("  Status  : {}", relation_message(relation));
}

fn print_update_report(binary: &str, report: &UpdateReport) {
    if report.updated {
        println!("wfadm self update complete");
    } else if report.status == "dry-run" {
        println!("wfadm self update dry run");
    } else if report.status == "aborted" {
        println!("wfadm self update aborted");
        return;
    } else {
        println!("wfadm self update skipped");
    }

    println!("  Product : {}", report.product);
    println!("  Binary  : {binary}");
    println!("  Binaries: {}", SUITE_BINS.join(", "));
    println!("  Install : {}", report.install_dir);
    println!("  Channel : {}", report.channel);
    println!("  Manifest: {}", report.source);
    println!("  Current : {}", report.current_version);
    println!(
        "  Latest  : {}",
        render_latest_version_for_versions(&report.current_version, &report.latest_version)
    );
    println!("  Artifact: {}", report.artifact);
    println!("  Status  : {}", report.status);
}

fn render_latest_version_for_versions(current: &str, latest: &str) -> String {
    match compare_versions_str(current, latest) {
        Ok(relation) => render_latest_version(latest, relation),
        Err(_) => latest.to_string(),
    }
}

fn render_latest_version(latest: &str, relation: VersionRelation) -> String {
    render_latest_version_with_color(latest, relation, should_use_color())
}

fn render_latest_version_with_color(
    latest: &str,
    relation: VersionRelation,
    use_color: bool,
) -> String {
    if relation == VersionRelation::AheadOfChannel {
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

#[cfg(test)]
mod tests {
    use super::*;
    use wp_self_update::updates_manifest_url;

    fn source(channel: UpdateChannel) -> SelfSourceArgs {
        SelfSourceArgs {
            channel,
            updates_base_url: None,
            updates_root: None,
            json: false,
        }
    }

    #[test]
    fn default_manifest_source_uses_channel_branch_url() {
        let cases = [
            (
                UpdateChannel::Stable,
                "https://raw.githubusercontent.com/wp-labs/warp-fusion/main/updates/stable/manifest.json",
            ),
            (
                UpdateChannel::Alpha,
                "https://raw.githubusercontent.com/wp-labs/warp-fusion/alpha/updates/alpha/manifest.json",
            ),
            (
                UpdateChannel::Beta,
                "https://raw.githubusercontent.com/wp-labs/warp-fusion/beta/updates/beta/manifest.json",
            ),
        ];

        for (channel, expected) in cases {
            let config = to_core_source(&source(channel));
            assert_eq!(config.channel, to_core_channel(channel));
            match config.kind {
                SourceKind::Manifest {
                    updates_base_url,
                    updates_root,
                } => {
                    assert_eq!(
                        updates_manifest_url(&updates_base_url, to_core_channel(channel)),
                        expected
                    );
                    assert_eq!(updates_root, None);
                }
                _ => panic!("expected manifest source"),
            }
        }
    }

    #[test]
    fn custom_manifest_base_url_is_used_before_channel_path() {
        let mut args = source(UpdateChannel::Alpha);
        args.updates_base_url = Some("https://example.test/updates/".to_string());

        let config = to_core_source(&args);
        match config.kind {
            SourceKind::Manifest {
                updates_base_url, ..
            } => {
                assert_eq!(
                    updates_manifest_url(&updates_base_url, to_core_channel(args.channel)),
                    "https://example.test/updates/alpha/manifest.json"
                );
            }
            _ => panic!("expected manifest source"),
        }
    }

    #[test]
    fn local_manifest_root_takes_precedence_in_source_config() {
        let mut args = source(UpdateChannel::Beta);
        args.updates_base_url = Some("https://example.test/updates".to_string());
        args.updates_root = Some(PathBuf::from("/tmp/wf-updates"));

        let config = to_core_source(&args);
        match config.kind {
            SourceKind::Manifest { updates_root, .. } => {
                assert_eq!(updates_root, Some(PathBuf::from("/tmp/wf-updates")));
            }
            _ => panic!("expected manifest source"),
        }
    }

    #[test]
    fn binary_name_is_taken_from_current_exe_path() {
        assert_eq!(
            current_binary_name(Path::new("/opt/bin/wfadm")).unwrap(),
            "wfadm"
        );
    }

    #[test]
    fn suite_update_installs_all_warp_fusion_bins() {
        assert_eq!(suite_bins(), vec!["wfusion", "wfgen", "wfl", "wfadm"]);
    }

    #[test]
    fn latest_version_is_dimmed_when_channel_is_behind_current() {
        assert_eq!(
            render_latest_version_with_color("0.1.29", VersionRelation::AheadOfChannel, true),
            "\u{1b}[90m0.1.29\u{1b}[0m"
        );
    }

    #[test]
    fn latest_version_is_plain_when_update_is_available() {
        assert_eq!(
            render_latest_version_with_color("0.1.30", VersionRelation::UpdateAvailable, true),
            "0.1.30"
        );
    }
}
