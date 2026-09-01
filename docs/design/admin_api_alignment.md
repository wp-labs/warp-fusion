# Admin API — 与 warp-parse 对齐现状

- 状态: Current
- 适用对象: wfusion admin_api / wfadm engine 开发者
- 参考实现: `warp-parse/src/admin_api.rs`
- 当前实现: `crates/wfusion/src/admin_api.rs`
- 更新时间: 2026-07-09

## 目标

`wfusion` 的 admin API 在发布、reload、status、安全基线上直接对齐 `wparse`。当前代码已经不保留旧的 `full` / `update_remote` 兼容协议。

## 当前协议

### Status

`GET /admin/v1/runtime/status`

响应字段:

```json
{
  "instance_id": "fusion:<pid>",
  "version": "0.1.24",
  "project_version": "1.0.1",
  "accepting_commands": true,
  "reloading": false,
  "current_request_id": null,
  "last_reload_request_id": "...",
  "last_reload_result": "reload_done",
  "last_reload_started_at": "...",
  "last_reload_finished_at": "..."
}
```

说明:

- `accepting_commands` 由 `RuntimeControlHandle::cancel_token()` 判断。
- `reloading` / `current_request_id` / `last_reload_*` 由 admin API reload 状态机维护。
- `project_version` 从 `.run/project_remote_state.json` 读取；dual-repo 模式返回 group versions。

### Reload / Publish

`POST /admin/v1/reloads/model`

请求体:

```json
{
  "wait": true,
  "update": false,
  "version": "1.0.1",
  "group": "models",
  "timeout_ms": 15000,
  "reason": "manual publish"
}
```

字段语义:

| 字段 | 默认 | 说明 |
|------|------|------|
| `wait` | `true` | 是否等待 reload 结果；`false` 立即返回 `202 running` |
| `update` | `false` | 是否先执行 project remote sync |
| `version` | `null` | update 目标版本；只能在 `update=true` 时使用 |
| `group` | `null` | dual-repo 目标组：`models` 或 `infra`；只能在 `update=true` 时使用 |
| `timeout_ms` | `admin_api.request_timeout_ms` | `wait=true` 等待 runtime 回复的超时 |
| `reason` | `""` | 日志审计字段 |

结果码:

| HTTP | `result` | 说明 |
|------|----------|------|
| `200` | `reload_done` | reload 成功 |
| `202` | `running` | `wait=false` 或等待超时，后台继续处理 |
| `400` | `invalid_request` | 参数非法，例如 `version` without `update` |
| `401` | `unauthorized` | bearer token 无效 |
| `404` | `not_found` | 未知路由 |
| `409` | `reload_in_progress` | 当前已有 reload 请求 |
| `409` | `update_in_progress` | project remote lock 被占用 |
| `200` | `restart_required` | project 已同步/校验通过，但 runtime 判定需要重启后生效 |
| `413` | `payload_too_large` | 请求体超过 `max_body_bytes` |
| `500` | `update_failed` | project remote sync / snapshot / config load 前置失败 |
| `500` | `reload_failed` | runtime reload 执行失败 |
| `503` | `runtime_not_ready` | runtime 已进入关闭状态 |

## 与旧 wfusion 协议的差异

| 旧字段 / 行为 | 当前状态 |
|---------------|----------|
| `update_remote` | 已移除，使用 `update` |
| `full` | 已移除；Admin API 不触发进程级重启 |
| `result:"applied"` | 改为 `reload_done` |
| `result:"blocked"` | 改为 `restart_required`，HTTP `200` |
| `result:"restarting"` | 已移除 |
| status `accepting` | 改为 `accepting_commands` |
| 空 body 默认 reload | 不再兼容；请求体必须是合法 JSON，例如 `{}` |

## 安全基线

| 项 | 当前实现 |
|----|----------|
| bearer token | 所有路由必须带 `Authorization: Bearer <token>` |
| token 文件权限 | Unix 下拒绝 group/other 权限过宽的 token 文件 |
| 非 loopback bind | 未启用 TLS 时拒绝非 loopback 地址 |
| TLS | 支持 rustls + tokio-rustls，ALPN `http/1.1` |
| auth mode | 仅支持 `bearer_token` |
| request id | 优先使用 `X-Request-Id`，否则生成 UUID |
| body limit | `admin_api.max_body_bytes`，超限返回 `413` |

## 实现映射

| 能力 | 代码位置 |
|------|----------|
| server 启动 / TLS / token 校验 | `crates/wfusion/src/admin_api.rs::start_if_enabled` |
| request routing | `handle_request` |
| status | `status_response` |
| reload 主流程 | `handle_reload` |
| wait / background monitor | `monitor_reload_task` |
| runtime 结果映射 | `map_reload_result` |
| project remote sync | `run_remote_sync` |
| 失败回滚 | `rollback_updated_project` |
| CLI 对接 | `crates/wfadm/src/engine.rs` |

## 与 wparse 的保留差异

- `wparse` 的 status 直接来自 runtime status snapshot；`wfusion` 当前由 admin API 在 reload 请求边界维护 `ReloadState`。
- `wparse` 有自己的 `wp_error` / `wp_log` 体系；`wfusion` 使用 `String` 错误和 `tracing`。
- 配置类型分别来自 `wp-config` 和 `wf-config`，不共享 config struct。

这些差异不影响 HTTP 协议和发布语义。

## 测试覆盖

当前相关验证:

- `cargo test -p wfusion admin_api`
- `cargo test -p wf-project-remote`
- `cargo check -p wfusion`
- `cargo check -p wfadm`
- `bash -n wf-examples/core/remote_ctrl/run.sh`

重点覆盖:

- bearer / 404 / TLS / 非 loopback / token 权限
- status `accepting_commands` / `reloading`
- `wait=false` 后台完成状态清理
- `update=true` 成功发布
- unknown version / disabled remote
- dual-repo 缺 `group`
- project remote lock 冲突
- update 后 config load / runtime blocked 失败回滚

## 后续重构方向

可以考虑把 admin API 的 HTTP server 骨架抽成共用 crate，但不应把 reload 业务逻辑抽象过早泛化。可共用部分包括:

- bind / accept loop / shutdown
- bearer token + token file 权限
- TLS 加载与非 loopback 校验
- request id
- JSON response / 404

项目特有部分仍应留在各自 crate:

- runtime status 来源
- reload 执行与回滚
- project config 类型
- 错误与日志体系
