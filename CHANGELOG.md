# Changelog

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
- **新增**: `wfadm config diff` 从 `wfusion config` 迁移。
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
