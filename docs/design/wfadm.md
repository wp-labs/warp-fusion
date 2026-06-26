# wfadm — WarpFusion Admin CLI

> 规划文档。`wfadm` 是 `wfusion`/`wfgen`/`wfl` 的**项目管理层**，不重复已有命令。

## 设计原则

1. **不重复造轮子**：`wfusion` 已有的 `run`、`config`、`rule`、`scenario`、`version` 原封不动保留，`wfadm` 只加项目级管理能力
2. **借鉴 `wproj` 模式**：参考 warp-parse 的 `wproj` CLI 结构（`init` → `check` → 各模块管理），去掉 wfusion 不需要的概念（`data`/`rescue`/`knowdb`/`model sources`）
3. **统一入口**：`wfadm` 作为面向用户的单一入口，内部转发到 `wfusion`/`wfgen`/`wfl`

## 已有命令（wfadm 不重复）

| 命令 | 来源 | 说明 |
|------|------|------|
| `wfadm run` | `wfusion run` | 启动引擎 |
| `wfadm config` | `wfusion config` | 配置渲染/比对/变量 |
| `wfadm rule` | `wfusion rule` / `wfl` | 规则解释/校验/格式化/回放/测试 |
| `wfadm scenario` | `wfgen` | 场景生成/校验/验证/发送/压测/流式 |
| `wfadm version` | `wfusion version` | 版本检查 |

> 以上命令 `wfadm` 直接转发到对应的二进制，不加任何额外逻辑。

## 新增命令（wfadm 的项目管理层）

### `wfadm init` — 创建 wf-rules 项目

```bash
wfadm init [--name <name>] [--dir <path>]
```

创建项目骨架：

```
<dir>/
├── wfusion.toml              # 带注释的模板配置
├── schemas/                  # WFS schema 文件
│   └── example.wfs           # 示例 schema
├── rules/                    # WFL 规则文件
│   └── example.wfl           # 示例规则
├── scenarios/                # wfgen 场景文件
│   └── example.wfg           # 示例场景
├── test/
│   ├── sources/              # 测试用 source 配置
│   └── sinks/                # 测试用 sink 配置
│       ├── connectors/sink.d/
│       ├── business.d/
│       ├── infra.d/
│       └── defaults.toml
└── .gitignore
```

可选 `--from <repo-url>` 从远程模板仓库拉取。

### `wfadm check` — 验证项目完整性

```bash
wfadm check [--dir <path>]
```

| 检查项 | 说明 |
|--------|------|
| schema 语法 | 所有 `.wfs` 文件可正常解析 |
| rule 语法 | 所有 `.wfl` 文件编译通过 |
| rule→schema 引用 | 规则的 `use "xxx.wfs"` 指向存在的 schema |
| rule→window 引用 | 规则的 `events { alias : window }` 中 window 存在于 schema |
| sink 配置 | `sinks/` 目录结构正确，connector 引用有效 |
| sink→window 路由 | `business.d/` 中 `windows` 匹配实际存在的告警窗口 |
| 场景校验 | 所有 `.wfg` 文件可正常解析 |

### `wfadm conf` — 配置管理增强

```bash
# 渲染实际运行配置（合并 overlay + 变量）
wfadm conf render

# 显示每个配置项的来源
wfadm conf origins [--prefix <path>]

# 显示所有变量
wfadm conf vars

# 两个配置 diff
wfadm conf diff --base base.toml --target other.toml
```

> 大部分功能 `wfusion config` 已有，`wfadm conf` 提供更友好的默认路径和输出格式。

### `wfadm sink` — Sink 验证

```bash
# 列出所有 sink 组和路由
wfadm sink list

# 验证某个 yield_target 是否有对应 sink
wfadm sink check <yield_target>

# 验证整个 sink 配置
wfadm sink validate
```

### `wfadm self-update` — 自更新

```bash
wfadm self-update [--version <ver>]
```

从 GitHub Releases 下载更新并替换二进制。

## 命令总览

```
wfadm
├── run          → wfusion run          (已有)
├── config       → wfusion config       (已有)
├── rule         → wfusion rule / wfl   (已有)
├── scenario     → wfgen                (已有)
├── version      → wfusion version      (已有)
├── init         ★ 新建
├── check        ★ 新建
├── conf         ★ 增强（转发 wfusion config + 默认路径）
├── sink         ★ 新建
└── self-update  ★ 新建
```

## 实施顺序

1. **`wfadm init`** — 最优先，降低新用户上手成本
2. **`wfadm check`** — 项目验证，CI/CD 集成
3. **`wfadm sink`** — sink 验证，对应本次调试中发现的 sinks 布局问题
4. **`wfadm self-update`** — 运维必需
5. **`wfadm conf`** — 已有基础，增强易用性

## 关键技术选型

- CLI 框架：`clap`（与 `wfusion`/`wproj` 一致）
- 模板引擎：`include_str!` 宏嵌入模板，或用 `handlebars` 做变量替换
- 自更新：`reqwest` + 解析 GitHub Releases API，参考 `wproj/handlers/self_update.rs`

## 参考

- `warp-parse/src/wproj/` — wproj 的完整实现
- `warp-fusion/crates/wfusion/src/main.rs` — 当前 CLI 入口
- `warp-fusion/crates/wfgen/src/main.rs` — wfgen 独立二进制
- `warp-fusion/crates/wfl/src/main.rs` — wfl 独立二进制
