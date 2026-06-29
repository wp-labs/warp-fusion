# Admin API — 与 warp-parse 对齐路线图

- 状态: Draft
- 适用对象: wfusion admin_api 开发者
- 参考实现: `warp-parse/src/admin_api.rs`(`wp-labs/warp-parse`)
- 当前实现: `crates/wfusion/src/admin_api.rs`

## 文档目标

`wfusion` 的 admin_api 应与 `wparse` 的 admin_api 在**功能与安全语义**上对齐。本文记录逐项对比结果与对齐计划，作为后续实现的路线图。

> `reload`（`POST /admin/v1/reloads/model`）是 `wparse` 的核心能力，涉及 `RuntimeControlHandle` 接入与 project_remote 同步/回滚全套逻辑，工程量较大，**本期不实现，留待后续单独立项**。本文其余项为目标对齐范围。

## 总体结论

`wfusion` 的 admin_api 是 `wparse` 的**极简子集**：HTTP server 骨架（binder / 认证 / status / shutdown / 并发模型）一致，但 `wparse` 已有的**安全加固、TLS、运行时状态接入、配置校验**在 `wfusion` 侧基本缺失。

## 一致项（无需改动）

| 项 | 说明 |
|------|------|
| 启动入口 `start_if_enabled` | 都返回 `Option<AdminApiRuntime>`，`enabled=false` → `Ok(None)` |
| 并发模型 | `tokio::select!` accept 循环 + 每连接 `tokio::spawn` + `AutoBuilder::new(TokioExecutor)` + `TokioIo` + `service_fn` |
| shutdown 机制 | `oneshot::Sender` + `JoinHandle`，`pub async fn shutdown(mut self)` |
| bearer token 认证 | 读 `Authorization: Bearer` header 比对；读 token_file + `trim()` + 空检查 |
| status 路由 | `GET /admin/v1/runtime/status` |
| 404 处理 | 未命中路由 → 404 + `{request_id, accepted:false, result:"not_found"}` |
| request_id | `Uuid::new_v4()` |
| remote_addr 日志 | status 日志带 `remote={}` |
| JSON 响应 Content-Type | `application/json` |

## 待对齐项（按优先级）

### P0 — 状态可信度与安全基线

| 项 | wparse | wfusion 现状 | 对齐动作 |
|------|--------|--------------|----------|
| `accepting` 反映真实状态 | 从 `control_handle.status_snapshot()` 动态读 `accepting_commands` | ✅ 方案 A 已实现（`!cancel.is_cancelled()`） | **当前用方案 A**；**最终目标方案 B**（见下"accepting 状态方案"） |
| status 响应字段 | `{instance_id, version, project_version, accepting_commands, reloading, current_request_id, last_reload_*}` | `{instance_id, version, accepting}`（accepting 为方案 A 真实值） | `accepting` 已接真实值；`project_version`/`reloading`/`last_reload_*` 随 reload 一并后续补 |
| 非 loopback 拒绝（无 TLS） | 无 TLS 时拒绝非 loopback bind，报错 | ✅ 已实现 | `start_if_enabled` 中 `is_loopback` 校验 |
| TLS 支持 | `rustls` + `tokio-rustls`，支持 HTTPS / 自签证书 / ALPN http/1.1 | ✅ 已实现 | `rustls` + `tokio-rustls`，`load_tls_config` + `run_tls` + `TlsAcceptor`，ALPN http/1.1 |

#### accepting 状态方案

wfusion 的 admin_api 在 `Reactor::start` 成功后启动（`cli_config.rs:431`），但 wp-reactor 的 `Reactor` 当前**不暴露任何状态查询接口**（仅 `shutdown`/`wait`/`cancel_token`，见 `wp-reactor/crates/wf-runtime/src/lifecycle/mod.rs:52-57`），也没有 wparse 的 `RuntimeControlHandle`。因此分两步：

- **方案 A（已实现，最小）**：`accepting = !cancel.is_cancelled()`。复用 `Reactor::cancel_token()`（`wp-reactor/crates/wf-runtime/src/lifecycle/mod.rs:181`，无需改 wp-reactor），`cli_config.rs:431` 将 `reactor.cancel_token()` 传入 `start_if_enabled`，`AppState` 持有，status 端点读 `!cancel.is_cancelled()`。只能反映"未开始关闭"，无法表达 reload/降级。**在 reload 落地前足够**——当前无 reload 也就无 reloading/降级状态。单元测试 `status_reflects_cancel_state` 验证 cancel 前后 `accepting` 真实翻转。
- **方案 B（最终目标，随 reload 一并实现）**：在 wp-reactor 引入 `RuntimeControlHandle` 状态机（`accepting_commands`/`reloading`/`current_request_id`/`last_reload_*`），对齐 wparse `wp-motor/src/facade/runtime_ctrl.rs`。届时 status 字段全量补齐。属于 reload 立项范围。

> 决策：当前用方案 A 让 status 报告可信的"是否在运行"；reload 立项时升级到方案 B。

### P1 — 低成本安全加固

| 项 | wparse | wfusion 现状 | 对齐动作 |
|------|--------|--------------|----------|
| token 文件权限校验 | `validate_token_file()` 检查 group/other 位，必须 `0o600`/`0o400` | ✅ 已实现 | `start_if_enabled` 中 `validate_token_file`，过宽则报错 |
| `X-Request-Id` header 支持 | 优先读 `X-Request-Id` header，否则生成 UUID | ✅ 已实现 | `request_id(headers)` 优先读 header，401/404 响应回显 |
| `max_body_bytes` 校验 | 读请求体累计字节，超限 → 413 | ⏳ 随 reload 补 | 当前无请求体路由（无 POST），`read_json_body` + 大小校验随 reload 的 `POST /admin/v1/reloads/model` 一并实现 |
| `request_timeout_ms` | 用于 reload wait 超时 | config 字段存在但未用 | 随 reload 一并后续补（当前无请求体接口，暂不阻塞） |

### P2 — 架构差异（合理保留）

| 项 | wparse | wfusion | 说明 |
|------|--------|---------|------|
| config crate | `wp-config`（`EngineConfig`） | `wf-config`（`AdminApiConf`） | wfusion 用自己的 config 体系，合理保留 |
| `env` 展开 token_file | `${HOME}` 展开 + 相对 work_root 绝对化 | 直接 `work_root.join` | wfusion token_file 须相对 work_root，若需 `${HOME}` 支持再补 |

### 后续 — reload 热重载（本期不做）

`wparse` 的 `POST /admin/v1/reloads/model` 通过 `RuntimeControlHandle.request_load_model` 触发 Reactor 重载，含：

- `reload_gate` 并发互斥（`409 CONFLICT`）
- project_remote 同步 / 快照 / 失败回滚
- `wait=true/false` 同步/异步语义
- runtime 就绪检查（`503` / `409`）
- force_replace（drain 超时降级）

**wfusion 本期不实现**。待运行时控制面（`RuntimeControlHandle` 等价物）就绪后单独立项，届时同步补 `status` 的 `reloading` / `last_reload_*` 字段与 `request_timeout_ms` 使用。

## 测试对齐

| 项 | wparse | wfusion 现状 | 对齐动作 |
|------|--------|--------------|----------|
| 单元测试 | 10 个（bearer / TLS / 非 loopback / 权限 / env / version 校验 / dual-mode） | 6 个（bearer / 404 / 基础） | 随各对齐项补充对应单元测试 |
| 集成测试 | 17 个 `#[serial]`（spawn 真实 daemon，端到端 reload / rollback / 并行） | 无 | reload 落地后再补集成测试 |
| HTTP 客户端 | `reqwest` 异步 | `reqwest` 异步 | ✅ 已对齐（曾用 `ureq` 同步导致 current-thread runtime 死锁，已改 `reqwest`） |

## 实现顺序建议

1. **P0 安全基线**：非 loopback 拒绝 + TLS 支持 → ✅ 已完成（含 3 个单元测试）
2. **P0 状态可信度**：`accepting` 接入真实运行时状态 → 方案 A（`!cancel.is_cancelled()`），最终目标方案 B 随 reload 升级
3. **P1 低成本加固**：token 权限校验 + `X-Request-Id` + `max_body_bytes` → 逐项补，每项配套单元测试
4. **后续 reload**：运行时控制面（方案 B 的 `RuntimeControlHandle`）就绪后单独立项，届时 status 字段全量补齐

### P0 进度

| 项 | 状态 |
|------|------|
| 非 loopback 拒绝（无 TLS） | ✅ 已完成 |
| TLS 支持 | ✅ 已完成 |
| `accepting` 真实状态（方案 A） | ✅ 已完成（`Reactor::cancel_token()` 接线，`!cancel.is_cancelled()`） |
| `accepting` 真实状态（方案 B） | 🔜 后续（随 reload 立项） |
| token 文件权限校验 | ✅ 已完成（`validate_token_file`，0o600） |
| `X-Request-Id` header | ✅ 已完成（优先读 header，401/404 回显） |
| `max_body_bytes` 校验 | ⏳ 随 reload 补（当前无请求体路由） |
| `request_timeout_ms` | ⏳ 随 reload 补 |

## 未来重构方向：抽取共用 crate

### 动机

`wparse`（2071 行）与 `wfusion`（340 行）的 admin_api 在 **HTTP server 骨架**上重复，且未来对齐 P0（TLS、非 loopback、token 权限）会让 `wfusion` 也长出相同代码，重复加剧。其中**安全逻辑（TLS、权限校验、非 loopback）必须两边一致**——共用能保证安全语义单点维护，这正是"对齐"目标的根本。

### 可共用 vs 不可共用

| 可共用（~300-400 行骨架） | 不可共用（项目特有） |
|------|------|
| `start_if_enabled` bind/spawn/shutdown 框架 | 运行时状态来源（wparse: `RuntimeControlHandle`；wfusion: 待接入 reactor） |
| accept 循环 + `AutoBuilder` + `serve_connection` | reload 触发 + project_remote 同步/回滚（wparse 独有 ~700 行） |
| bearer token 认证 + token_file 读取/trim/空检查 | config 体系（wparse: `wp-config`；wfusion: `wf-config`） |
| token 文件权限校验（0o600） | 日志（wparse: `wp_log`；wfusion: `tracing`） |
| 非 loopback 拒绝 + TLS（rustls）加载 | 错误类型（wparse: `wp_error::RunResult`；wfusion: `String`） |
| `request_id`（Uuid + X-Request-Id） | status 响应字段差异 |
| `json_response` / 404 兜底 | reload 异步回执（`oneshot` + `RuntimeCommandResp`） |

### 目标形态

```
wp-admin-api (新 crate，骨架)
├── AdminApiServer: bind/spawn/shutdown/accept loop
├── Auth: bearer token + 文件权限校验
├── Tls: rustls 加载 + 非 loopback 校验
├── RequestId: Uuid + X-Request-Id
├── json_response / 404 兜底
└── trait AdminApiHandler {  ← 项目注入状态/路由
        async fn status(&self) -> StatusResponse
        async fn route(&self, method, path, body) -> Response
    }
```

`wparse` 与 `wfusion` 各自实现 `AdminApiHandler`，注入项目特有的 reload/state/config。reload 那套 ~700 行仍留在 `wparse` 自己的 handler 里。

### 决策：先对齐 P0，再抽 crate

**现在不抽**。理由：

- `wfusion` 的 admin_api 仅 340 行，尚未对齐 P0（TLS/状态/权限都缺）。此时抽 crate 是"从一个还没长全的实现 + 一个过重的实现（wparse reload）"里提抽象，trait 边界必然设计错。
- reload 的异步回执与 project_remote 锁等复杂状态很难塞进通用 trait，大概率仍留在 `wparse` handler 里，导致 trait 只覆盖最简单的 status/404，抽象收益有限。

**对齐 P0 后再抽**。等 `wfusion` 的 admin_api 也长出 TLS/权限/非 loopback（~600-700 行），两边的"可共用骨架"清晰可见，trait 边界有真实参照，设计才稳。届时 reload 已明确是 `wparse` 专属，留在 handler 里，边界干净。

### 抽取时的注意点

- trait 边界以**真实两侧实现**为参照设计，不要凭空抽象。
- 日志/错误类型差异：crate 内部用自有 trait/泛型抽象，或由调用方注入 logger/error 转换，避免强绑 `wp_log`/`wp_error`。
- config：crate 定义自己的 `AdminApiServerConf`（bind/tls/auth/timeout/max_body），项目侧从各自 config 转换注入，不共享 `AdminApiConf` 类型。
- 版本管理：新 crate 独立发版，`wparse`/`wfusion` 各自按需 bump。

