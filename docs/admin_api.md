# Admin API 使用文档

`wfusion` daemon 可以通过 Admin API 查询运行状态、在线 reload 规则/配置，并在配置了 `[project_remote]` 时执行在线发布。HTTP 协议已对齐 `wparse`，当前不再支持旧的 `full` / `update_remote` 参数。

## 启用 Admin API

在 `conf/wfusion.toml` 中启用:

```toml
[admin_api]
enabled = true
bind = "127.0.0.1:19080"
request_timeout_ms = 15000
max_body_bytes = 4096

[admin_api.auth]
mode = "bearer_token"
token_file = "${HOME}/.warp_fusion/admin_api.token"

[admin_api.tls]
enabled = false
# cert_file = "certs/admin.crt"
# key_file = "certs/admin.key"
```

字段说明:

| 字段 | 默认值 | 说明 |
|------|--------|------|
| `enabled` | `false` | 是否启用 Admin API |
| `bind` | `127.0.0.1:19080` | 监听地址 |
| `request_timeout_ms` | `15000` | reload 等待 runtime 结果的默认超时 |
| `max_body_bytes` | `4096` | 请求体最大字节数 |
| `auth.mode` | `bearer_token` | 当前仅支持 bearer token |
| `auth.token_file` | `${HOME}/.wfusion/admin_api.token` | bearer token 文件；`wfadm init` 模板会显式设置为 `${HOME}/.warp_fusion/admin_api.token` |
| `tls.enabled` | `false` | 是否启用 HTTPS |
| `tls.cert_file` | 空 | PEM 证书路径 |
| `tls.key_file` | 空 | PEM 私钥路径 |

安全限制:

- 所有请求必须带 `Authorization: Bearer <token>`。
- token 文件不能为空。
- Unix 下 token 文件不能对 group / other 开放权限，建议 `chmod 600`。
- `bind` 使用非 loopback 地址时必须启用 TLS。
- `auth.mode` 只能是 `bearer_token`。
- 推荐使用 `${HOME}/...` 形式的 token 路径；手写相对路径时，daemon 按 `--work-dir` 解析。

`wfadm init` 会创建默认 token 文件。手写配置或迁移旧工程时，可以手动创建:

```bash
mkdir -p "$HOME/.warp_fusion"
openssl rand -hex 32 > "$HOME/.warp_fusion/admin_api.token"
chmod 600 "$HOME/.warp_fusion/admin_api.token"
```

启动 daemon:

```bash
wfusion daemon --config conf/wfusion.toml --work-dir .
```

## 通过 wfadm 使用

查询状态:

```bash
wfadm engine status --config conf/wfusion.toml
```

触发本地 reload:

```bash
wfadm engine reload --config conf/wfusion.toml
```

非阻塞 reload:

```bash
wfadm engine reload --config conf/wfusion.toml --wait false
```

在线发布并 reload。

Single-repo:

```bash
wfadm engine reload \
  --config conf/wfusion.toml \
  --update \
  --version 1.0.1 \
  --wait true \
  --reason "release 1.0.1"
```

Dual-repo:

```bash
wfadm engine reload \
  --config conf/wfusion.toml \
  --update \
  --group models \
  --version 1.0.1 \
  --wait true \
  --reason "release 1.0.1"
```

直接指定 Admin API 地址和 token:

```bash
wfadm engine reload \
  --admin-url http://127.0.0.1:19080 \
  --token-file "$HOME/.warp_fusion/admin_api.token" \
  --json
```

常用参数:

| 参数 | 说明 |
|------|------|
| `--config` | 读取 `[admin_api]` 中的 `bind` 和 `auth.token_file` |
| `--admin-url` | 直接指定 Admin API 地址，覆盖配置文件中的 `bind` |
| `--token-file` | 直接指定 bearer token 文件，覆盖配置文件中的 `auth.token_file` |
| `--json` | 直接输出 Admin API JSON 响应 |
| `--wait true/false` | 是否等待最终 reload 结果；默认 `true` |
| `--timeout-ms` | `--wait true` 时等待结果的超时时间 |
| `--update` | 发布模式：先同步 `[project_remote]`，再 reload |
| `--version` | 目标版本；需要 `--update` |
| `--group` | dual-repo 发布分组：`models` 或 `infra`；需要 `--update` |
| `--reason` | 写入 daemon 日志的原因 |
| `--request-id` | 发送为 `X-Request-Id`，用于日志关联 |

## 通过 curl 使用

准备变量:

```bash
ADMIN_URL="http://127.0.0.1:19080"
TOKEN="$(cat "$HOME/.warp_fusion/admin_api.token")"
```

### 查询状态

```bash
curl -sS "$ADMIN_URL/admin/v1/runtime/status" \
  -H "Authorization: Bearer $TOKEN"
```

响应示例:

```json
{
  "instance_id": "fusion:12345",
  "version": "0.1.24",
  "project_version": "1.0.1",
  "accepting_commands": true,
  "reloading": false,
  "current_request_id": null,
  "last_reload_request_id": "9a1f...",
  "last_reload_result": "reload_done",
  "last_reload_started_at": "2026-07-09T10:00:00Z",
  "last_reload_finished_at": "2026-07-09T10:00:01Z"
}
```

字段说明:

| 字段 | 说明 |
|------|------|
| `instance_id` | daemon 实例 ID |
| `version` | 当前 `wfusion` 版本 |
| `project_version` | 当前 project remote 版本；dual-repo 时为 group versions |
| `accepting_commands` | runtime 是否仍接受控制命令 |
| `reloading` | 是否有 reload 正在进行 |
| `current_request_id` | 当前 reload 请求 ID |
| `last_reload_request_id` | 最近一次 reload 请求 ID |
| `last_reload_result` | 最近一次 reload 结果 |
| `last_reload_started_at` | 最近一次 reload 开始时间 |
| `last_reload_finished_at` | 最近一次 reload 结束时间 |

### 本地 reload

本地 reload 重新读取 work root 中的配置和规则，然后调用 runtime reload:

```bash
curl -sS -X POST "$ADMIN_URL/admin/v1/reloads/model" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{}'
```

成功响应:

```json
{
  "request_id": "9a1f...",
  "accepted": true,
  "result": "reload_done",
  "force_replaced": false
}
```

### 非阻塞 reload

```bash
curl -sS -X POST "$ADMIN_URL/admin/v1/reloads/model" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"wait": false, "reason": "manual async reload"}'
```

响应:

```json
{
  "request_id": "9a1f...",
  "accepted": true,
  "result": "running",
  "update": false
}
```

之后用 status 查询 `last_reload_result`。

### 在线发布

`update=true` 会先同步 `[project_remote]` 指定的远端版本，再 reload runtime。

Single-repo:

```bash
curl -sS -X POST "$ADMIN_URL/admin/v1/reloads/model" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"update": true, "version": "1.0.1", "reason": "release 1.0.1"}'
```

Dual-repo:

```bash
curl -sS -X POST "$ADMIN_URL/admin/v1/reloads/model" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"update": true, "group": "models", "version": "1.0.1"}'
```

成功响应:

```json
{
  "request_id": "9a1f...",
  "accepted": true,
  "result": "reload_done",
  "update": true,
  "requested_version": "1.0.1",
  "current_version": "1.0.1",
  "resolved_tag": "v1.0.1",
  "group": "models",
  "force_replaced": false
}
```

Dual-repo 模式下必须传 `group`:

- `models`: 只同步 `models`
- `infra`: 同步 `conf` / `topology` / `connectors`

### 自定义 Request ID

可以通过 `X-Request-Id` 指定请求 ID，便于日志关联:

```bash
curl -sS -X POST "$ADMIN_URL/admin/v1/reloads/model" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -H "X-Request-Id: deploy-20260709-001" \
  -d '{"wait": false}'
```

## Reload 请求字段

| 字段 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `wait` | bool | `true` | 是否等待最终 reload 结果 |
| `update` | bool | `false` | 是否先执行 project remote sync |
| `version` | string/null | `null` | 目标版本；只能与 `update=true` 一起使用；省略时由 project remote 自动解析 |
| `group` | string/null | `null` | `models` 或 `infra`；dual-repo update 必填，single-repo update 不需要 |
| `timeout_ms` | number/null | `admin_api.request_timeout_ms` | `wait=true` 的等待上限 |
| `reason` | string/null | `null` | 日志原因 |

注意:

- 请求体必须是合法 JSON。默认 reload 请发送 `{}`。
- `version` / `group` 不能在 `update=false` 时使用。
- `update=true` 需要配置并启用 `[project_remote]`。
- dual-repo 发布必须传 `group`；single-repo 发布不要传 `group`。
- Admin API 不支持 `full`，不会触发进程级重启。
- Admin API 不支持旧字段 `update_remote`，请使用 `update`。

## 返回结果

| HTTP | `result` | 说明 |
|------|----------|------|
| `200` | `reload_done` | reload 成功 |
| `202` | `running` | 请求已接受，后台继续执行 |
| `400` | `invalid_request` | 请求 JSON 或参数非法 |
| `401` | `unauthorized` | bearer token 无效或缺失 |
| `404` | `not_found` | 路由不存在 |
| `409` | `reload_in_progress` | daemon 内已有 reload 正在执行 |
| `409` | `update_in_progress` | project remote lock 被占用 |
| `200` | `restart_required` | project 已同步/校验通过，但 runtime 判定需要重启后生效 |
| `413` | `payload_too_large` | 请求体超过 `max_body_bytes` |
| `500` | `update_failed` | project remote 未启用、sync、snapshot 或前置检查失败 |
| `500` | `reload_failed` | config load 或 runtime reload 执行失败 |
| `503` | `runtime_not_ready` | daemon 正在关闭或不再接受命令 |

需要重启的响应格式:

```json
{
  "request_id": "9a1f...",
  "accepted": true,
  "result": "restart_required",
  "warning": "reload requires restart because 1 restart-required changes were found; synced project content was kept"
}
```

## 并发与回滚

Admin API 有两层保护:

- 同一 daemon 内一次只处理一个 reload。
- reload 会持有 `.run/project_remote.lock`，避免和 `wfadm conf update` 并发读写同一个 work root。

`update=true` 后，如果 config load 或 runtime reload 真正失败，daemon 会回滚:

- managed dirs
- `.run/project_remote_state.json`
- runtime artifacts: `.run/rule_mapping.dat`、`.run/authority.sqlite`

如果 runtime 返回 requires-restart / blocked，daemon 不回滚已经同步并通过校验的项目内容；该版本保留在本地，等待进程重启后生效。

纯本地 reload 失败不会修改磁盘内容。

## TLS 示例

监听非 loopback 地址必须启用 TLS:

```toml
[admin_api]
enabled = true
bind = "0.0.0.0:19080"

[admin_api.tls]
enabled = true
cert_file = "certs/admin.crt"
key_file = "certs/admin.key"
```

curl:

```bash
curl -k -sS "https://127.0.0.1:19080/admin/v1/runtime/status" \
  -H "Authorization: Bearer $TOKEN"
```

生产环境应使用受信任证书，不要长期依赖 `curl -k`。

## 常见问题

### `401 unauthorized`

检查:

- 是否带了 `Authorization: Bearer <token>`。
- token 是否和 `auth.token_file` 内容一致。
- token 文件是否包含多余空格；daemon 会 `trim()` 文件内容。

### daemon 启动时报 token 权限错误

设置 owner-only 权限:

```bash
chmod 600 "$HOME/.warp_fusion/admin_api.token"
```

### `409 update_in_progress`

通常表示另一个 `wfadm conf update` 或 admin API reload 正在操作同一个 work root。等待当前操作完成后重试。

### `200 restart_required`

runtime 判定变更需要重启，例如 window/schema/topology 发生不可热更新变化。若本次请求包含 `update=true`，项目内容已经同步到本地并通过校验，不会被自动回滚。Admin API 不会自动重启进程；需要由部署系统或运维流程重启 daemon。

### `500 update_failed`

通常是 project remote 配置、git fetch、版本解析、snapshot 或 config load 失败。查看 daemon 日志中的 request id。
