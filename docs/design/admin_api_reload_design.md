# Admin API Reload / Publish 设计

- 状态: Current
- 入口: `POST /admin/v1/reloads/model`
- 当前实现: `crates/wfusion/src/admin_api.rs`
- 更新时间: 2026-07-09

## 设计目标

在线 reload 同时承担两类动作:

1. **本地 reload**: 重新读取 work root 中的配置和规则，调用 runtime 热重载。
2. **在线发布**: 先从 `[project_remote]` 同步目标版本，再 reload 运行中的 daemon。

HTTP 协议直接对齐 `wparse`，不再兼容旧的 `full` / `update_remote`。

## 请求协议

```http
POST /admin/v1/reloads/model
Authorization: Bearer <token>
Content-Type: application/json
```

```json
{
  "wait": true,
  "update": false,
  "version": "1.0.1",
  "group": "models",
  "timeout_ms": 15000,
  "reason": "release"
}
```

字段:

| 字段 | 默认 | 说明 |
|------|------|------|
| `wait` | `true` | 等待 runtime reload 结果 |
| `update` | `false` | reload 前执行 project remote sync |
| `version` | `null` | sync 目标版本；必须配合 `update=true` |
| `group` | `null` | dual-repo group：`models` 或 `infra`；必须配合 `update=true` |
| `timeout_ms` | `admin_api.request_timeout_ms` | `wait=true` 的等待上限 |
| `reason` | `""` | 记录到日志 |

空 body 或非法 JSON 不再按默认 reload 处理；调用方应发送 `{}`。

## 响应协议

成功 reload:

```json
{
  "request_id": "...",
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

后台执行:

```json
{
  "request_id": "...",
  "accepted": true,
  "result": "running",
  "update": true,
  "current_version": "1.0.1"
}
```

需要重启:

```json
{
  "request_id": "...",
  "accepted": true,
  "result": "restart_required",
  "warning": "reload requires restart because 1 restart-required changes were found; synced project content was kept"
}
```

## 端到端流程

```text
handle_request
  └─ authorized
  └─ handle_reload
      ├─ reload_gate.try_lock
      ├─ read_json_body
      ├─ validate request fields
      ├─ load [project_remote] when update=true
      ├─ runtime readiness check
      ├─ mark_reload_started
      ├─ acquire_project_remote_lock
      ├─ capture project snapshot
      ├─ capture runtime artifact snapshot
      ├─ run remote sync when update=true
      ├─ load raw/effective config
      ├─ apply CLI overrides
      ├─ spawn runtime apply_reload task
      ├─ wait result OR return running
      └─ map result / rollback on failure
```

## 并发控制

有两层互斥:

| 锁 | 范围 | 作用 |
|----|------|------|
| `reload_gate: Mutex<()>` | admin API reload 请求 | 同一 daemon 内只接受一个 reload |
| `.run/project_remote.lock` | work root 文件锁 | 阻止 admin API reload 与 `wfadm conf update` 并发读写 managed dirs |

注意: 本地 reload 即使 `update=false` 也会拿 project remote lock。原因是 reload 会从磁盘读取配置和规则；如果此时 `wfadm conf update` 正在替换目录，读取结果可能不一致。

## update=true 发布流程

```text
acquire_project_remote_lock
capture_project_remote_snapshot_with_group
capture_runtime_artifact_snapshot
run_remote_update_locked
  └─ sync_project_remote / sync_project_remote_group
  └─ validation rollback on sync check failure
load updated config
runtime apply_reload
rollback project + runtime artifacts on reload failure
```

Dual-repo 规则:

- `[project_remote.models]` + `[project_remote.infra]` 同时存在时，`update=true` 必须带 `group`。
- `group=models` 只替换 `models`。
- `group=infra` 只替换 `conf` / `topology` / `connectors`。

## 回滚语义

admin API 维护 `ProjectRemoteReloadContext`:

```rust
struct ProjectRemoteReloadContext {
    _lock_guard: ProjectRemoteLockGuard,
    snapshot: ProjectRemoteSnapshot,
    runtime_snapshot: ProjectRuntimeArtifactSnapshot,
    update_result: Option<ProjectRemoteUpdateResult>,
}
```

只有 `update_result.is_some()` 且 config load / runtime reload 真正失败时才执行发布回滚；纯本地 reload 失败不回滚项目文件。`ReloadOutcome::Blocked` 表示需要重启后生效，不回滚已经同步并通过校验的项目内容。

回滚内容:

1. `restore_project_remote_update`
   - 如果 sync 改变了 managed dirs，恢复备份目录。
   - 恢复 `.run/project_remote_state.json`。
2. `restore_runtime_artifact_snapshot`
   - 恢复 rule mapping。
   - 恢复 authority db。

回滚触发点:

- update 后 config load 失败
- runtime reload task join 失败
- runtime `ReloadOutcome::Blocked`
- runtime reload 返回 error
- 后台 reload task 失败

## wait=true / wait=false

### `wait=true`

默认行为。admin API 等待 runtime reload 任务:

- runtime 在 `timeout_ms` 内完成，直接返回最终 `reload_done` / `restart_required` / `reload_failed`。
- 等待超时，返回 `202 running`，后台 task 继续监控结果。

### `wait=false`

admin API 启动 runtime reload task 后立即返回:

```json
{"accepted": true, "result": "running"}
```

后台 `monitor_reload_task` 负责:

- 等待 runtime 结果。
- 更新 `last_reload_result`。
- 失败时执行回滚。

## Status 状态机

`ReloadState` 由 admin API 维护:

```rust
struct ReloadState {
    current_request_id: Option<String>,
    last_reload_request_id: Option<String>,
    last_reload_result: Option<&'static str>,
    last_reload_started_at: Option<SystemTime>,
    last_reload_finished_at: Option<SystemTime>,
}
```

状态变更:

| 时机 | 变化 |
|------|------|
| `mark_reload_started` | `reloading=true`，设置 `current_request_id`，清空 `last_reload_result` |
| `mark_reload_finished` | `reloading=false`，清空当前 request，写入 result 和 finished time |

`last_reload_result` 取值:

- `reload_done`
- `restart_required`
- `reload_failed`
- `update_failed`
- `update_in_progress`

## Runtime reload 能力边界

runtime 返回:

| `ReloadOutcome` | Admin API 响应 |
|-----------------|----------------|
| `Applied(plan)` | `200 reload_done` |
| `Blocked(plan)` | `200 restart_required`，保留已同步项目内容 |
| `Err(err)` | `500 reload_failed`，必要时回滚 update |

当前 Admin API 不负责进程级重启:

- 无 `full=true`。
- 无 `result:"restarting"`。
- requires-restart 变更返回 `200 restart_required`。
- 调用方或外部 supervisor 决定是否重启 daemon。

## 安全与校验

| 场景 | 结果 |
|------|------|
| 缺 bearer token | `401 unauthorized` |
| body 超限 | `413 payload_too_large` |
| 非法 JSON | `400 invalid_request` |
| `version` without `update` | `400 invalid_request` |
| `group` without `update` | `400 invalid_request` |
| invalid group | `400 invalid_request` |
| dual-repo `update=true` 缺 group | `400 invalid_request` |
| runtime 已关闭 | `503 runtime_not_ready` |
| reload 并发 | `409 reload_in_progress` |
| project remote lock 冲突 | `409 update_in_progress` |

## wfadm 对接

`wfadm engine reload` 发送同一协议:

```bash
wfadm engine reload \
  --update \
  --version 1.0.1 \
  --group models \
  --wait false \
  --timeout-ms 15000 \
  --reason "release"
```

旧参数已移除:

- `--update-remote` → `--update`
- `--full` → 不再支持

## 测试覆盖

关键测试:

- `reload_applied_returns_200`
- `reload_wait_false_clears_reloading_when_done`
- `reload_update_success_applies_new_rules`
- `reload_update_blocked_keeps_synced_project`
- `reload_update_wait_false_blocked_keeps_synced_project_in_background`
- `reload_update_dual_repo_requires_group`
- `reload_update_lock_conflict_returns_409`
- `run_remote_update_locked_uses_provided_snapshot_for_validate_rollback`

验证命令:

```bash
cargo test -p wfusion admin_api
cargo test -p wf-project-remote
cargo check -p wfadm
```

## 已知取舍

- status reload state 由 admin API 维护，而不是直接来自 runtime snapshot。
- rollback 只针对 `update=true` 后的项目变更；纯本地 reload 失败不修改磁盘。
- config 语法错误当前返回 `500 reload_failed`，未来可细化为 4xx。
- `force_replaced` 当前成功路径固定为 `false`，保留字段用于与 wparse 协议一致。
