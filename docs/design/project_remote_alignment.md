# Project Remote — 与 warp-parse 对齐路线图

- 状态: Draft
- 适用对象: wfusion `wfadm conf update` / admin_api reload 开发者
- 参考实现: `warp-parse/src/project_remote/`(`wp-labs/warp-parse`)及 `wp-motor/crates/wp-config/src/engine.rs` 的 `[project_remote]` 配置
- 参考设计: `warp-parse/docs/design/zh/project_remote_sync_design.md`
- 当前实现: wfusion **无**（待移植）

## 文档目标

`wfusion` 需要与 `wparse` 对齐"远端规则版本同步"能力：从 `[project_remote]` 配置的远端 git 仓库，按版本 tag 拉取规则/配置，整体替换本地 managed 目录（`models`/`conf`/`topology`/`connectors`），做模型校验，失败回滚。本文记录对齐范围、分阶段计划与抽取共用 crate 的决策。

## 功能语义（对齐 wparse）

`conf update` 的本质是**远端规则版本拉取 + 目录替换 + 状态记录 + 校验回滚**：

- 从 `conf/wfusion.toml` 的 `[project_remote]` 读取远端 git 仓库配置
- 按 git tag（`v<semver>`）解析目标版本（`--version` 指定，否则取最新 release tag，再否则 `origin/HEAD`）
- checkout 对应 commit，把远端的 managed 目录内容**整体替换**到本地
- 持久化"当前版本状态"到 `.run/project_remote_state.json`
- 校验新配置能被引擎加载；失败则从备份回滚

支持两种模式：
- **Single-repo**：`[project_remote] repo = "..."`，一个 repo 管 `models`+`conf`+`topology`+`connectors` 全部
- **Dual-repo**：`[project_remote.models]` + `[project_remote.infra]`，必须带 `--group`；models 组只动 `models`，infra 组只动 `conf`/`topology`/`connectors`

> `conf update`（离线 CLI：sync + 校验）与 admin_api reload（在线：sync + reload 运行中引擎）**共享同一套 `project_remote` 模块**（锁/sync/快照/回滚）。本次对齐先做 CLI 的 `conf update`，admin_api reload 端点留后续（见 `admin_api_alignment.md` 的 reload 立项）。

## wparse 实现要点（移植参照）

| 模块 | 文件 | 行数 | 职责 |
|------|------|------|------|
| `mod.rs` | `src/project_remote/mod.rs` | 1277 | sync 核心编排、`resolve_project_remote_mode`、`RemoteGroup`、状态机 |
| `state.rs` | `src/project_remote/state.rs` | 305 | flock 文件锁、快照、回滚、原子写、`load_engine_config` |
| `repo.rs` | `src/project_remote/repo.rs` | 250 | git 操作：clone/open、fetch tags、resolve tag、checkout |
| `managed.rs` | `src/project_remote/managed.rs` | 364 | managed 目录的 diff/备份/替换/恢复 |
| `test_support.rs` | `src/project_remote/test_support.rs` | 237 | 测试 fixture |
| 命令编排 | `src/wproj/handlers/conf.rs` | — | `run_conf_update`：锁→快照→sync→校验→回滚 |
| 命令定义 | `src/wproj/args.rs` | — | `ConfCmd::Update(ConfUpdateArgs)` |
| 配置结构 | `wp-motor/crates/wp-config/src/engine.rs:21-59` | — | `ProjectRemoteConf`/`RepoGroupConf` |

### 关键耦合点（移植接缝）

`project_remote` 模块**不依赖引擎**（`wp_engine` 依赖为 0），但耦合以下 wparse 专属类型，移植时需替换：

| 依赖 | wparse | wfusion 现状 | 移植接缝 |
|------|--------|--------------|----------|
| config 类型 | `wp_config::engine::{ProjectRemoteConf, RepoGroupConf, EngineConfig}` | `wf-config` 无 `[project_remote]` | 在 `wf-config` 加 `ProjectRemoteConf`/`RepoGroupConf`，或 wfusion 本地定义 |
| 错误类型 | `wp_error::{RunResult, RunReason}` | `String` | 替换为 `String` 或 wfusion 错误体系 |
| env 字典 | `orion_variate::{EnvDict, EnvEvaluable}` | 无 | 调用方传入已展开的 config，crate 内不做 env 展开 |
| 日志 | `wp_log::{info_ctrl, warn_ctrl}` | `tracing` | 替换为 `tracing` |
| 密钥加载 | `crate::load_sec_dict` | 无 | 由调用方注入 |
| 模型校验 | `WpApp::validate_load_model`（命令层，非模块内） | `Reactor::start` 加载阶段 | 命令层用 wfusion 等价校验 |

### managed 目录（按 group）

- `Models` → `["models"]`
- `Infra` → `["conf", "topology", "connectors"]`
- `None`（single）→ `["conf", "models", "topology", "connectors"]`

### 辅助文件

- `.run/project_remote_state.json` — 版本状态（原子写）
- `.run/project_remote/remote[-models|-infra]/` — 远端 git 缓存
- `.run/project_remote/backup/` + `manifest.json` — 更新前 managed 目录备份
- `.run/project_remote.lock` — 文件锁（flock，非阻塞排他）

## 对齐计划（分阶段）

### 阶段 0 — 基础设施（config 结构）

✅ 已完成：
- 在 `wf-config` 新增 `project_remote.rs`，定义 `ProjectRemoteConf`/`RepoGroupConf`（字段 `enabled`/`repo`/`init_version`/`models`/`infra`，`deny_unknown_fields`，`enable` alias，`Default`）
- `wf-config/src/lib.rs` 声明 `pub mod project_remote;`
- `config_loader/fusion.rs`：`FusionConfigRaw` + `FusionConfig` 加 `project_remote` 字段（`#[serde(default)]`），Raw→Resolved 转换传递
- `config_loader/validate.rs`：测试辅助构造补 `project_remote` 字段
- 6 个单元测试：缺段默认值、single-repo、dual-repo、未知字段拒绝、`enable` alias、序列化 roundtrip

### 阶段 1 — project_remote 模块移植

加依赖：`git2`、`semver`、`libc`（flock）、`walkdir`（按需，移植模块时加）。

近乎原样移植 wparse 的 `project_remote/` 算法逻辑（git 操作、目录 diff/备份/恢复、flock、原子写、状态机），替换上述接缝：
- config 类型用 wfusion 的 `ProjectRemoteConf`
- 错误用 `String`（或 wfusion 错误体系）
- 日志用 `tracing`
- env 展开 / 密钥加载由调用方处理

### 阶段 2 — wfadm conf update 命令

- 在 `wfadm` 加 `Conf { Update(ConfUpdateArgs) }` 子命令（参数：`--work-root`、`--version`、`--group`、`--json`）
- 实现 `run_conf_update` 编排：锁 → 快照 → sync → 校验 → 回滚
- 校验用 wfusion 的模型加载入口（对齐 wparse 的 `validate_load_model`）
- 先跑通 single-repo 基本流程，dual-repo 后续补

### 阶段 3 — 完善

- dual-repo 支持（`--group models|infra`）
- 完整回滚（sync 内部 + 校验失败两层）
- `wfadm init --repo` 引导路径（对齐 wparse `project.rs` 的 `run_conf_update_from_repo`）
- 单元测试 + 集成测试（对齐 wparse 的 25 个单元测试 + 集成测试）

### 后续 — admin_api reload（独立立项）

admin_api 加 `POST /admin/v1/reloads/model` 端点，复用 `project_remote` 模块（同一把锁、同一套 sync/快照/回滚），触发运行中引擎 reload。见 `admin_api_alignment.md` 的 reload 立项与方案 B（`RuntimeControlHandle`）。

## 未来重构方向：抽取共用 crate

### 动机

`project_remote` 模块 2433 行，逻辑与引擎无关（`wp_engine` 依赖为 0），是**通用的远端规则同步能力**。它是 `conf update` 与 admin_api reload 共享的核心，wparse 与 wfusion 都需要——其中**同步/锁/回滚语义必须两边一致**，共用能保证单点维护。

### 决策：先移植 wfusion 版，两份跑通后再抽 crate

**现在不抽**。理由：

- wfusion 还没有 `project_remote` 的任何实现，现在抽是"从 wparse 单边实现提抽象"，trait 边界没有两侧真实参照，会设计错。
- 抽 crate 要先改 wparse（把 2433 行剥出来适配 trait），有回归风险。

**两份都跑通后再抽** `wp-project-remote` crate。届时 wparse 与 wfusion 各有一份 `project_remote`，**两份真实实现就是 trait 边界的参照**：核心 sync/lock/repo/managed 共用，项目特有的 config/错误/校验/env 注入由 trait 暴露。

> 与 `admin_api_alignment.md` 的抽 crate 决策同一原则：**先有两份真实实现，再提抽象**。

### 目标形态

```
wp-project-remote (新 crate，核心算法)
├── sync: sync_project_remote / sync_project_remote_group
├── repo: git clone/open/fetch/resolve tag/checkout
├── managed: 目录 diff/备份/替换/恢复
├── state: flock 锁 / 快照 / 原子写 / 回滚
└── trait ProjectRemoteHost {  ← 项目注入
        fn load_config(&self) -> ProjectRemoteConfig  // 已展开的远端配置
        fn validate(&self) -> Result<(), Error>        // 模型校验
        // 错误/日志由 crate 自有类型 + tracing
    }
```

`wparse` 与 `wfusion` 各自实现 `ProjectRemoteHost`，注入 config/校验/env。核心 sync/lock/repo/managed 共用。

### 抽取注意点

- config 类型：crate 自定义 `ProjectRemoteConfig`，项目侧从各自 config（wp-config / wf-config）转换注入，不共享 config 类型。
- 错误：crate 自定义错误类型（或基于 orion-error，两边都有），不绑 `wp_error`。
- env 展开：调用方传入已展开 config，crate 不做 env 展开（不绑 `orion_variate`）。
- 日志：用 `tracing`（通用，两边都可接受）。
- 校验：trait 方法注入，不绑 `wp_engine`/`WpApp`。
- 版本管理：新 crate 独立发版，`wparse`/`wfusion` 各自按需 bump。

## 非目标（本期不做）

- admin_api reload 端点（独立立项，见 `admin_api_alignment.md`）
- 抽取 `wp-project-remote` crate（两份实现跑通后再做）
- 远端 bootstrap 的 `init --repo` 完整流程（阶段 3 补）
