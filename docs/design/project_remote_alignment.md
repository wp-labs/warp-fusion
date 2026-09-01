# Project Remote — 与 warp-parse 对齐现状

- 状态: Current
- 适用对象: `wf-project-remote` / `wfadm conf update` / admin API reload 开发者
- 参考实现: `warp-parse/src/project_remote/`
- 当前实现: `crates/wf-project-remote`
- 更新时间: 2026-07-09

## 目标

`wfusion` 通过 `wf-project-remote` 提供和 `wparse` 对齐的远端项目同步能力。该能力同时服务:

- 离线 CLI: `wfadm conf update`
- 远端 bootstrap: `wfadm init --repo`
- 在线发布: `POST /admin/v1/reloads/model` with `update=true`

核心语义是: **文件锁 → 快照 → git sync → 状态持久化 → 校验 → 失败回滚**。

## 配置模式

### Single-repo

```toml
[project_remote]
enabled = true
repo = "https://example.com/project.git"
init_version = "1.0.0"
```

一个 repo 管理全部 managed dirs:

- `conf`
- `models`
- `topology`
- `connectors`

### Dual-repo

```toml
[project_remote]
enabled = true

[project_remote.models]
repo = "https://example.com/models.git"
init_version = "1.0.0"

[project_remote.infra]
repo = "https://example.com/infra.git"
init_version = "1.0.0"
```

group 语义:

| group | managed dirs |
|-------|--------------|
| `models` | `models` |
| `infra` | `conf`, `topology`, `connectors` |

Dual-repo 模式下，`wfadm conf update` 和 admin API `update=true` 都必须指定 `group`。

## 状态与辅助文件

| 路径 | 说明 |
|------|------|
| `.run/project_remote_state.json` | 当前版本状态，single / dual 格式兼容 |
| `.run/project_remote/remote` | single-repo git cache |
| `.run/project_remote/remote-models` | models group git cache |
| `.run/project_remote/remote-infra` | infra group git cache |
| `.run/project_remote/backup` | managed dirs 更新前备份 |
| `.run/project_remote/backup/manifest.json` | 备份 manifest，避免恢复错误目录 |
| `.run/project_remote.lock` | 非阻塞排他 flock |

## 核心 API

| API | 用途 |
|-----|------|
| `run_remote_update` | 调用方不持锁时的一站式更新入口 |
| `run_remote_update_locked` | 调用方已经持有 `ProjectRemoteLockGuard` 时使用；要求显式传入 guard |
| `sync_project_remote` | single-repo sync |
| `sync_project_remote_group` | dual-repo group sync |
| `sync_project_remote_from_repo` | `wfadm init --repo` bootstrap |
| `resolve_project_remote_mode` | 判断 single / dual / disabled / invalid |
| `current_project_version` | 读取 single-repo 当前版本 |
| `current_project_group_versions` | 读取 dual-repo 当前版本 |
| `capture_project_remote_snapshot_with_group` | 捕获 state snapshot |
| `restore_project_remote_update` | 恢复 managed dirs 和 state |
| `capture_runtime_artifact_snapshot` | 捕获 runtime artifacts |
| `restore_runtime_artifact_snapshot` | 恢复 runtime artifacts |

## 更新流程

```text
acquire_project_remote_lock
  └─ capture_project_remote_snapshot_with_group
      └─ sync_project_remote / sync_project_remote_group
          ├─ clone/open cache repo
          ├─ fetch tags
          ├─ resolve version/tag/head
          ├─ checkout commit
          ├─ backup managed dirs
          ├─ replace managed dirs
          └─ persist project_remote_state.json
      └─ validate project
      └─ restore_project_remote_update on validation failure
```

`run_remote_update_locked` 不获取锁，而是要求调用方传入 `&ProjectRemoteLockGuard`。这用于 admin API reload:

1. admin API 先拿锁。
2. 捕获 project snapshot 和 runtime artifact snapshot。
3. 如果 `update=true`，用同一把锁执行 remote sync。
4. runtime reload 失败时，复用同一份 snapshot 回滚。

这样可以避免 `wfadm conf update` 和在线 reload 读写同一个 work root。

## wfadm conf update

命令:

```bash
wfadm conf update \
  --work-root . \
  --version 1.0.1 \
  --group models \
  --json
```

行为:

- 从 `<work-root>/conf/wfusion.toml` 读取 `[project_remote]`。
- single-repo 模式可不传 `--group`。
- dual-repo 模式传 `--group models|infra`。
- 使用 `wf_project_remote::run_remote_update`。
- 成功时输出当前版本、tag、revision、changed。
- 失败时自动回滚 managed dirs 和 state。

## wfadm init --repo

命令:

```bash
wfadm init --dir . --repo https://example.com/project.git --version 1.0.0
```

行为:

- 不依赖本地 `[project_remote]`。
- 直接从显式 repo URL bootstrap 工作目录。
- 复用 `run_conf_update_from_repo` 和 `sync_project_remote_from_repo`。
- 同样执行校验和失败回滚。

## Admin API update=true

请求:

```json
{
  "wait": true,
  "update": true,
  "version": "1.0.1",
  "group": "models",
  "reason": "release"
}
```

行为:

- reload 请求开始后获取 `.run/project_remote.lock`。
- dual-repo 模式下缺 `group` 返回 `400 invalid_request`。
- lock 冲突返回 `409 update_in_progress`。
- sync 成功后重新加载 config 并调用 runtime reload。
- config load / runtime reload 真正失败时回滚 project 和 runtime artifacts。
- runtime blocked / requires-restart 时保留已同步并通过校验的项目内容，等待进程重启后生效。

## Rollback 范围

| 失败阶段 | 回滚内容 |
|----------|----------|
| git sync 内部校验失败 | managed dirs + project state |
| admin API update 后 config load 失败 | managed dirs + project state + runtime artifacts |
| admin API runtime reload failed | managed dirs + project state + runtime artifacts |
| admin API blocked / requires restart | 不回滚；保留已同步项目内容，等待重启生效 |

runtime artifacts 当前包括:

- `.run/rule_mapping.dat`
- `.run/authority.sqlite`

## 与 wparse 的保留差异

- `wf-project-remote` 使用 `String` 作为错误类型，日志使用 `tracing`。
- 配置类型来自 `wf_config::project_remote`。
- 项目校验由调用方注入，crate 不依赖引擎。

## 测试覆盖

当前 `wf-project-remote` 测试覆盖:

- lock 冲突
- single-repo sync / version resolve / latest release fallback
- dual-repo group sync / 独立状态持久化
- managed dirs backup / restore
- state single / dual / backward compatible
- sync 失败回滚
- `run_remote_update_locked` 使用调用方 snapshot 做校验失败回滚

相关命令:

```bash
cargo test -p wf-project-remote
cargo test -p wfusion admin_api
```

## 后续方向

`wf-project-remote` 已经从 `wfadm` 中提取为独立 crate。未来如果要和 `wparse` 共享实现，应优先抽取两边共同的 repo / managed / state / lock 核心逻辑，保留各项目自己的:

- config 转换
- 项目校验
- 错误类型适配
- CLI / admin API 编排
