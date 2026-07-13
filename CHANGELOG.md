# Changelog

All notable changes to wfusion will be documented in this file.

## [0.1.29 Unreleased]

### 依赖与语言能力

- **依赖**: `wf-engine` / `wf-config` / `wf-lang` / `wf-data` / `wf-runtime` 对齐 `wp-reactor` v0.1.32。
- **Sink 元字段控制**: 通过 `wp-reactor` v0.1.32 支持 `wf_meta_disable` 的 wildmatch matcher，可使用 `__wfu_*`、`__wfu_rule_*` 等 pattern 禁用 wfusion 元字段输出。

### 示例与文档

- **配置文档**: 补充 `wf_meta_disable` wildmatch pattern 说明，明确仅允许 `__wfu_` 前缀的精确字段或 pattern。
- **SSH brute force 示例**: 输出统计证据和事件/窗口时间边界，并在 sink 组中配置 `wf_meta_disable = ["__wfu_*"]`。

### Admin API / project remote

- **Reload 语义**: runtime 判定 requires-restart / blocked 时，Admin API 返回 `200 restart_required`，不再作为 `409 reload_failed` 处理。
- **Update 保留**: `update=true` 后如果项目已同步并通过校验，但 runtime reload 返回 `restart_required`，daemon 保留已同步项目内容和 project state，等待进程重启后生效。
- **状态记录**: 后台 reload 的 `last_reload_result` 增加 `restart_required`，文档和测试同步更新 wait=true / wait=false 行为。

### 发布元数据

- **版本**: CLI crate 版本推进到 `0.1.29`，stable update manifest 指向 `v0.1.28` 发布包，并归档 `v0.1.28` manifest。

## [0.1.28] — 2026-07-13

### 依赖与语言能力

- **依赖**: `wf-engine` / `wf-config` / `wf-lang` / `wf-data` / `wf-runtime` 对齐 `wp-reactor` v0.1.31。
- **WFL helper**: `wfl` / `wfusion` 通过 `wp-reactor` 新增规则表达式 helper 支持：`now()`、`now_s()`、`now_ms()`、`now_us()`、`now_ns()`、`is_blank()`、`null_if_blank()`、`default_if_blank()`、`md5()`、`sha1()`、`sha256()`、`hex()`、`stable_id()`。
- **时间语义**: `now_*` 在同一条输出记录内复用同一个内部时间戳，避免 `created_time` / `created_ns` 等字段在同一 alert 中出现漂移。
- **稳定 ID**: `stable_id()` 使用带类型和长度的稳定编码参与 SHA-256，避免简单拼接输入导致的歧义。
- **WFL 诊断**: `wfl` / `wfusion` 通过 `wp-reactor` 新增源码感知的 WFL 解析与语义编译诊断，发布失败时输出文件路径、诊断类别、规则/测试上下文、行列号和源码片段。
- **Topology 诊断**: `wfusion` 启动/重载时的 intermediate topology cycle 错误现在会尽量定位到对应规则源码，便于排查跨规则 yield 依赖环。
- **WFS / WFL 结构化输出**: `wfgen` 和 `wfusion` 适配 `object` / `array` / `array/T` 字段，结构化值在 Arrow IPC 输出中以 JSON 字符串承载。

### Stream / Window 分发

- **WFS**: 示例与文档统一使用 `window.stream_tag` 作为输入数据到 window 的分发键，替换旧 `stream` 表达。
- **wparse 对接**: `warp-parse -> warp-fusion` 的非 Arrow framed 输出 carrier 对齐为 `wp_oml_name`，用于承载 OML `name` / `full_name`；不再使用旧 `wp_stream_tag`。`wp-reactor` v0.1.31 起该字段也是默认 `stream_tag_field`。
- **Arrow framed**: `examples/wp-pipeline/streaming` 移除 wparse sink 中手写的固定 `tag = "nginx_access"`，改为依赖上游 OML name 自动写入 Arrow frame tag；wfusion source 未配置固定 `stream_tag` 时按 frame tag 路由。
- **Sink 路由修复**: 修正 `examples/wp-pipeline` 中 `error_burst` sink 订阅不存在的 `error_burst_alerts` window，改为订阅 schema 中实际声明的 `error_alerts`。
- **模板迁移**: `wfadm` 模板、docker 默认配置和配置测试统一改用 `stream_tag`，并移除旧的 `topology/sinks/connectors` 模板布局。

### 示例与文档

- **新增示例**: `examples/rules/single_stream_multi_window`，演示一个固定 `stream_tag = "netflow"` 同时投递到多个 window。
- **新增示例**: `examples/rules/multi_stream_multi_window`，演示一个 source 中混合多个 `wp_oml_name`，通过 `stream_tag_field = "wp_oml_name"` 动态分发到多个 window。
- **示例脚本**: 为 single-stream / multi-stream 示例增加 `run.sh`，并接入 `examples/rules/run_all.sh`。
- **wp-pipeline demo**: `examples/wp-pipeline/demo` 对齐 `streaming` 的共享 `models/` 布局，batch 链路改为 `wpgen -> file -> wparse -> NDJSON(wp_oml_name) -> wfusion`，运行输出统一到示例根目录 `data/`。
- **wp-pipeline demo**: 删除 demo 内重复的本地 models、connectors、Redis external 规则和 knowdb，避免示例加载旧配置或依赖外部 Redis。
- **使用文档**: 新增 `docs/wparse-window-routing.md`，按配置步骤说明 `warp-parse` 输出如何进入 `warp-fusion` window。
- **设计文档**: 新增 `docs/design/stream_tag_routing.md` 与 `docs/design/wparse_window_routing.md`，记录 logical stream tag、Arrow frame tag、`wp_oml_name`、`stream_tag_field` 的关系和排查清单。
- **配置文档**: 更新 `docs/config/source.md`，补充固定 `stream_tag`、动态 `stream_tag_field`、Arrow framed frame tag 的分发优先级。
- **配置文档**: 更新 `docs/config/sink.md`，记录 `wf_meta_disable = ["__wfu_*"]` 的配置位置、字段限制、与 `fields` 投影的执行顺序，以及通过 `DataType::Ignore` 跳过 sink 输出的机制。

### wfgen

- **结构化字段默认值**: `array` / `object` 字段默认生成 JSON array/object，typed array 继续生成数组值。
- **Arrow IPC 输出**: `array` / `object` 字段映射为 UTF-8 输出，非字符串 JSON 值序列化为 JSON 字符串。
- **校验**: 结构化字段拒绝标量 generator override，避免生成与 schema 不匹配的数据。

### project remote

- **版本解析**: `wf-project-remote` 支持请求版本带 `v` 前缀，例如 `v1.4.3` 可直接匹配 `v1.4.3` tag，同时保留 resolved tag。

### 发布元数据

- **版本**: CLI crate 版本推进到 `0.1.28`，stable update manifest 指向 `v0.1.27` 发布包，并归档 `v0.1.26` / `v0.1.27` manifest。

### 验证

- `examples/rules/multi_stream_multi_window/run.sh`
- `examples/rules/single_stream_multi_window/run.sh`
- `examples/wp-pipeline/demo/run.sh`
- `examples/wp-pipeline/streaming/run.sh`
- `cargo test -p wfgen`
- `cargo test -p wf-project-remote`
- `cargo test -p wf-runtime stream_tag_field`
- `cargo test -p wf-config daemon_mode_accepts_arrow_framed_external_source_without_fixed_stream`

## [0.1.24] — 2026-07-09

### wfusion — admin API 发布协议对齐 wparse

- **Break**: `/admin/v1/reloads/model` 发布请求直接对齐 wparse 协议，使用 `wait` / `update` / `version` / `group` / `timeout_ms` / `reason`；移除旧的 `full` / `update_remote` 兼容语义。
- **Break**: reload 不再通过 `full=true` 触发 L4 重启；遇到 requires-restart 变更时返回 `409` + `reload_failed`，由调用方决定后续重启策略。
- **新增**: `wait=false` 非阻塞 reload，立即返回 `202` + `running`，后台完成后更新 status 中的 reload 状态。
- **新增**: `GET /admin/v1/runtime/status` 对齐 wparse 字段，返回 `accepting_commands`、`project_version`、当前/最近 reload request id、结果和时间戳。
- **新增**: `update=true` 支持 dual-repo `group=models|infra`，并在 dual-repo 模式下强制要求 `group`。
- **安全**: 非 loopback `admin_api.bind` 必须启用 TLS；`admin_api.auth.mode` 仅接受 `bearer_token`；请求体大小使用 `admin_api.max_body_bytes` 校验。
- **修复**: reload 和 `wfadm conf update` 共用 project remote 文件锁；本地 reload 也会持锁读取项目，避免与外部同步并发读写。
- **修复**: `update=true` 后如果配置加载、runtime reload 或 blocked reload 失败，会回滚 project remote state、managed dirs 和 runtime artifacts。
- **修复**: remote lock 冲突返回并记录 `update_in_progress`，不再误记为 `update_failed`。

### wf-project-remote

- **新增**: 导出 `ProjectRemoteMode` / `resolve_project_remote_mode`，供 daemon 在 reload 前判断 single-repo / dual-repo 发布模式。
- **新增**: 导出 runtime artifact snapshot 捕获与恢复 API，用于 daemon 发布失败后的完整回滚。
- **新增**: `run_remote_update_locked`，要求调用方显式持有 `ProjectRemoteLockGuard`，复用同一份 project snapshot 完成 sync 校验和 daemon reload 失败回滚。

### wfadm — engine reload CLI

- **Break**: `wfadm engine reload` 参数对齐 wparse：`--update` 替代 `--update-remote`，移除 `--full`，新增 `--wait <true|false>`、`--timeout-ms`、`--group`、`--reason`、`--request-id`。
- **修复**: `--wait false` 现在作为显式 bool 参数解析，能够发送非阻塞 reload 请求。
- **更新**: `engine status` 输出适配 `accepting_commands`、`reloading` 和 `project_version`。

### 测试

- **wfusion**: admin_api 测试扩展到 34 个，覆盖 wait=false、dual-repo group 校验、remote lock 冲突、update 后 reload 失败回滚、后台回滚状态清理。
- **wf-project-remote**: 27 个测试，新增 `run_remote_update_locked` 复用调用方 snapshot 的校验回滚覆盖。
- **验证**: `cargo check -p wfusion`、`cargo check -p wfadm`、`cargo check -p wf-project-remote`、`cargo test -p wfusion admin_api`、`cargo test -p wf-project-remote`、`bash -n wf-examples/core/remote_ctrl/run.sh`。

---

## [0.1.23] — 2026-07-08

### wf-project-remote — 独立 crate 提取

- **新增**: `wf-project-remote` 独立库 crate，从 `wfadm` 中提取 `project_remote/` 模块（`managed.rs` / `repo.rs` / `state.rs`）。
- **新增**: `run_remote_update` 高级 API — 锁 → 快照 → 同步 → 验证 → 失败回滚。`wfadm conf update` 委托给此方法，仅保留 CLI 输出。
- **新增**: `test-support` feature — 暴露 `RemoteFixture::from_parts` 让下游 crate 可创建自定义 git remote fixture。
- **依赖**: `wfadm` 移除直接 `git2` / `semver` / `libc` 依赖（通过 `wf-project-remote` 间接引入）。
- **测试**: 26 个（同步逻辑从 wfadm 迁移，不变）。

### wfusion — daemon 远程更新 → 重载

- **新增**: `POST /admin/v1/reloads/model` 请求体支持 `update_remote` / `version` 全量 JSON 字段（空 body 向后兼容）。
- **新增**: `update_remote=true` 时，daemon 在重新读取配置之前调用 `run_remote_sync` → `run_remote_update` → git fetch + 版本解析 + sync managed dirs。失败返回 502 Bad Gateway。
- **新增**: `ReloadingGuard` RAII 模式 — 重载期间 `reloading=true`，即使 panic 也能清除标记。
- **新增**: `GET /admin/v1/runtime/status` 响应增加 `reloading` 字段。
- **测试**: 新增 3 个测试（`status_includes_reloading_field` / `reload_update_remote_disabled_returns_502` / `reload_update_remote_unknown_version_returns_502`），验证 daemon 进程内 git-fetch + 版本解析路径可达。
- **测试**: 新增 `reload_update_remote_success_applies_new_rules` — 完整 e2e 路径：本地 git remote (v1.0.0 → v1.0.1) → daemon sync + reload → 规则热替换 → 200 + result=applied。

### wfadm — engine reload CLI

- **新增**: `wfadm engine reload` 支持 `--update-remote` / `--version` / `--full` 参数，透传到 daemon 的 `/admin/v1/reloads/model`。
- **修复**: 子命令上 `disable_version_flag` 与 `--version` 参数共存。

### 测试

- **wfusion**: 31 个（+4，含 e2e 远程更新→重载全链路）
- **workspace 总计**: 255 passed, 0 failed

---

## [0.1.22] — 2026-07-07

### wfusion — admin API 监听与 TLS 加载

- **修复**: 支持 `admin_api.bind = "0.0.0.0:..."` 且 `admin_api.tls.enabled = false` 的启动方式，不再强制非 loopback 地址必须启用 TLS。
- **修复**: TLS 启用时在生产代码路径初始化 rustls ring `CryptoProvider`，避免 TLS 配置加载依赖测试初始化或触发 provider 未安装 panic。
- **测试**: 增加 admin API 非 loopback + TLS disabled 覆盖，并保留非 loopback HTTPS 请求验证。

### wfgen

- **新增**: `wfgen --version` 顶层版本输出。
- **测试**: 增加 `--version` 回归测试，确认 clap 版本输出可用。

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
