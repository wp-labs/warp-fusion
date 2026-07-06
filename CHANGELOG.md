# Changelog

All notable changes to wfusion will be documented in this file.

## [0.1.21] — 2026-07-06

### wfusion — 路径基准从 config-file-relative 改为 working-dir-relative

- **Break**: `runtime_base_dir` 默认值从 `config_path.parent()`（配置文件所在 `conf/` 目录）改为 `current_dir()`（进程工作目录）。`wfadm check` 同步修正。
- 影响：`wfusion.toml` 中所有相对路径（`sources_dir` / `sinks` / `schemas` / `rules`）均需去掉一层 `..`。
  - 旧: `"../topology/sources"` → 新: `"topology/sources"`
  - 旧: `"../../../models/schemas/"` → 新: `"../../models/schemas/"`
- `business.d/*.toml` 中的 `base` 路径同样需去掉一层 `..`（`"../../data/alerts"` → `"../data/alerts"`）。
- 与 wparse 的路径基准统一（均为 working-dir-relative），消除同项目内路径层数不一致的问题。
- `--work-dir` CLI 参数逻辑不受影响（显式指定时优先级高于默认值）。

### wfadm

- **新增**: `wfadm init` 生成的 `wfusion.toml` 模板增加 `[admin_api.tls]` 配置段。
- **重构**: 内部字段 `conf_dir` 重命名为 `base_dir`，反映路径基准语义变化。

### 示例管线修复

- **streaming**: 修复 `parsed_netflow.toml` 缺少 `protocol = "arrow"` 导致 Arrow IPC 解码失败。
- **streaming**: `wpgen.toml` 增加 `[models].wpl` 配置，与 `wparse.toml` 共享 models 目录。
- **streaming / kafka**: `run.sh` 中 `wpgen sample` 发送端口改为整数（避免 connector 参数类型不匹配），启动顺序增加 `wait_port` 就绪探针替代固定 `sleep`。
- **kafka**: wfusion source `data_format` 从 `arrow_framed` 改为 `ndjson`，匹配 wparse kafka sink 的 JSON 输出。
- **kafka**: 删除 `demo.toml`（调试 sink，与 kafka sink 同配 `oml = ["*"]` 导致 first-match-wins 路由到 kafka sink 失败）。

### Docker

- **修复**: 多架构 Docker build（arm64→amd64 QEMU 交叉构建）时 libgit2 链接失败。`git2` 开启 `vendored-libgit2` feature 静态链接，消除运行时系统依赖。
- **修复**: 更新 git2 0.19 API 调用（`reference.name()` / `remote.url()` / `StringArray::iter()` / `symbolic_target()` 返回类型变更）。
- **修复**: `wfadm/Cargo.toml` 移除硬编码 local path 依赖（`path = "../../../wp-reactor/..."`），改为 `workspace = true`。
- **审计**: 添加 `.cargo/audit.toml` 忽略 RUSTSEC-2023-0071（`rsa` Marvin Attack，深层传递依赖，无补丁版本）。

### 测试与稳定性

- **wfadm**: connector 模板生成顺序稳定化（`param_map_to_toml` / `json_to_toml` 排序 key）。
- **wfgen**: e2e 测试适配 wp-reactor v0.1.25 的外部 windows.toml 配置。
- **wfgen**: 修复 `FusionConfig` import 在 test target 中缺失的编译错误。
- **wfusion**: admin_api 测试适配外部 windows.toml 配置。

---

## [0.1.17] — 2026-07-01

### wfusion — admin API 在线热重载

- **新增**: `POST /admin/v1/reloads/model` — 运行中引擎的在线热重载端点，支持 L1-L4 四级重载能力。
  - **L1 规则热替换**: 新编译的规则 task 替换旧的，保留 window/router/sink/evictor/metrics。
  - **L2 增量新增**: 新增 window 定义在 reload 时注册到运行中 registry，无需重启。
  - **L3 局部重建**: 修改现有 window 定义时，旧 window 被原子替换为新 window。
  - **L4 全量重启**: `full=true` 时，Reactor 以退出码 75 退出，由外部 supervisor 重启进程。
- **重构**: `RuntimeServant` 从 bare `CancellationToken` 升级为 `RuntimeControlHandle`（mpsc + oneshot channel），支持序列化 reload 请求。
- **新增**: `ReloadConfigSource` — 记录 boot 时的 `--config` / `--overlay` / `--var` 来源，reload 时用相同参数重新加载配置，而非硬编码 `wfusion.toml`。
- **新增**: body 限流（1 MB），防止 oversized reload payload。
- **测试**: 25 个 admin_api 测试（含 reload 序列化、L1-L4 场景、blocked 结果、full 重启）。

### wfusion CLI — 生命周期重构

- **Break**: 移除 `wfusion config` 子命令（功能已迁移到 `wfadm config`）。
- **重构**: CLI daemon/batch 生命周期从 `wait_for_signal()` + `reactor.shutdown()` + `reactor.wait()` 替换为 `reactor.run()`（内建 signal 处理 + reload control loop）。
- **重构**: `FusionConfigLoader` 改为 raw + effective config 双输出，reload 时重用 raw config 做 diff。

### wfadm — project_remote 远程规则源管理（实验性）

- **新增**: `wfadm conf update` — 从远程 git 仓库（`[project_remote]` 配置）同步 managed directories（`models/` / `conf/` / `topology/` / `connectors/`）到指定版本 tag。
  - 支持 `--version` / `--group` / `--json` 参数。
  - 自动锁定 → 快照 → 同步 → 校验 → 失败回滚。
- **新增**: `wfadm init --repo` — 从远程模板仓库初始化项目。
- **新增**: `crates/wfadm/src/project_remote/` 模块（`managed.rs` / `state.rs` / `repo.rs` / `test_support.rs`，共 2276 行），含完整 test_support 基础设施。

### wfadm — 修复

- `conf update` panic fix：禁用 Update subcommand 的 auto `--version` flag（与 clap 全局 version 冲突）。
- Clippy fix：`map_or(true, |u| u.is_empty())` → `is_none_or(|u| u.is_empty())`。

### 设计文档

- **新增**: `docs/design/admin_api_reload_design.md` — reload 完整方案设计，包含分层架构（L1-L4）、channel-style 控制通道、per-window diff 指纹方案、分阶段实现计划。
- **新增**: `docs/design/project_remote_alignment.md` — project_remote 对齐设计，与 wparse `wproj conf update` 对标。

### 依赖

- **新增**: `git2` 0.19、`semver` 1、`libc` 0.2（wfadm project_remote）。
- **新增**: `tempfile` 3（wfadm dev-dependencies，test_support）。
- **wf-runtime**: 本地 path 依赖（开发中），对应 wp-reactor `v0.1.24`（含 hot-reload RuntimeControlHandle + apply_reload）。
- **wfusion crate**: 版本号 0.1.15 → 0.1.16（crate version bump）。

## [0.1.16] — 2026-06-28

### wfusion — daemon / batch CLI + admin API

- **Break**: `wfusion run` 拆为 `wfusion daemon` 和 `wfusion batch`，`mode` 由 CLI 显式
  控制，不再读取配置中的 `mode` 字段。
- **Break**: 移除 `wfusion rule` 子命令（与 `wfl` 二进制 100% 重复）。
- **新增**: admin API HTTP server（hyper + bearer token 鉴权），`GET /admin/v1/runtime/status`
  端点。由 `wfadm engine status` 查询。
- **新增**: `[admin_api]` 配置段（默认端口 `127.0.0.1:19080`）。
- **修复**: `run.sh` / `smoke.sh` 脚本路径修正，`kill_wait` 超时防止挂死，
  `run.sh` → `test_run.sh` 重命名。

### wfadm — 完整 CLI 重构

- **新增**: `wfadm check` 深度校验——WFL/WFS/WFG 解析 + lint（`wf_lang::parse_wfl/wfs/wfg`）。
- **合并**: `wfadm sink` 合并到 `wfadm check`。
- **新增**: `wfadm conf diff` 从 `wfusion config` 迁移。
- **新增**: `wfadm engine status` / `engine reload`，通过 admin API 查询引擎状态。
- **重构**: `init_tpl` 合并到 `wfadm` 子模块（`src/init_tpl/`），模板文件在 `templates/`。
- **新增**: `wfadm self-update`（从 GitHub Releases 下载最新二进制）。
- **新增**: connector 从 `wp-core-connectors` registry 动态生成模板。
- **测试**: 36 个测试（含 WFL/WFS/WFG 解析、lint、init、check）。

### examples — 示例修正

- 所有示例 `wfusion run` → `wfusion daemon` / `wfusion batch`。
- `wp-pipeline` 脚本移除不兼容的 `-p` / `-n` / `-S` 参数。
- `wp-pipeline/deps-check.sh` 简化为仅用 PATH 查找二进制。
- 移除各示例 `wfusion.toml` 中不再需要的 `mode` 字段。

### 模板 & Docker

- 模板路径修正：`conf/` / `models/` / `topology/` 布局统一。
- 恢复 `docker/default_setting/` 模板文件。
- `test/wfusion.batch.toml` 新增到模板。

### 依赖

- `wp-core-connectors` 0.5.5 → 0.5.6
- `arrow` 54 → 59
- `wp-arrow` 0.1 → 0.2
- 新增: `hyper`, `hyper-util`, `http-body-util`, `uuid`（wfusion admin API）

## [0.1.11] - 2026-06-21

### wfgen — 使用 wp-core-connectors TcpArrowSink 发送数据

- **依赖**：添加 `wp-core-connectors`、`wp-connector-api`、`tokio` 依赖
- **重构**：`tcp_send.rs` 从原始 `TcpStream` + 手动 Arrow IPC 编码 → `TcpArrowSink::connect()` + `encode_batch_payload_with_tag()` + `send_payload()`
  - Arrow IPC 编码：使用 `encode_ipc_frame`（与 `wp_arrow::ipc::encode_ipc` 兼容）
  - Framing：RFC6587 octet-counted（`<len> <payload>`），匹配 wfusion `tcp_src` 的 `framing = "len"`
  - 传输层：`NetWriter` 带背压控制
- **异步化**：`cmd_stream`、`cmd_send`、`cmd_bench`、`cmd_gen` 全部改为 `async fn`
- **依赖升级**：`wp-core-connectors` 0.5.2 → 0.5.5（含 `encode_batch_payload_with_tag` 公开 API）
