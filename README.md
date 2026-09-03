# warp-fusion

[![CI: build & test](https://github.com/wp-labs/warp-fusion/actions/workflows/build-and-test.yml/badge.svg?branch=alpha)](https://github.com/wp-labs/warp-fusion/actions/workflows/build-and-test.yml)
[![release](https://img.shields.io/github/v/tag/wp-labs/warp-fusion?include_prereleases&label=release&color=orange)](https://github.com/wp-labs/warp-fusion/releases)
![license: ELv2](https://img.shields.io/badge/license-ELv2-blue.svg)
![lang: Rust](https://img.shields.io/badge/lang-Rust-000000.svg)
![status: active](https://img.shields.io/badge/status-active-brightgreen.svg)

**WarpFusion 的 CLI / 工具 workspace**——WFL 安全检测引擎的工程入口：规则开发（`wfl`）、数据生成与基准（`wfgen`）、引擎运行（`wfusion`）、管理（`wfadm`）。

## 目录

- [快速开始](#快速开始)
- [一个最小的规则](#一个最小的规则)
- [Workspace 组件](#workspace-组件)
- [示例导览](#示例导览)
- [文档](#文档)
- [能力定位与性能参照](#能力定位与性能参照)
- [架构亮点](#架构亮点)
- [边界声明](#边界声明)
- [License](#license)

## 快速开始

### 安装（推荐）

一行命令安装 `warp-fusion` 套件（`wfusion` 引擎及配套 CLI，默认装到 `~/bin`）：

```bash
# stable（默认通道，推荐生产）
curl -sSf https://get.warpparse.ai/inst-x.sh | bash -s -- wfusion

# 预发布通道：alpha / beta（新语法验证、与引擎开发线对齐时使用）
curl -sSf https://get.warpparse.ai/inst-x.sh | bash -s -- wfusion alpha
curl -sSf https://get.warpparse.ai/inst-x.sh | bash -s -- wfusion beta
```

安装后确保 `~/bin` 在 PATH 中（脚本会提示）：

```bash
export PATH="$HOME/bin:$PATH"
```

### 快速体验（示例项目集合）

示例项目在独立仓库 [wf-examples](https://github.com/wp-labs/wf-examples)。最简路径
`getting_started/` 一条命令验证完整 CEP 管道（`wfadm init` 生成 17 规则项目 →
lint → 生成 3 万事件演示数据 → batch 回放产出 654 条 `port_scan` 告警 → TCP
daemon 实时联调）：

```bash
# 确保 wfadm / wfusion / wfgen 在 PATH（见上方安装 / 源码构建；前置细节以
# wf-examples/getting_started 的 README 为准），然后：
git clone https://github.com/wp-labs/wf-examples
cd wf-examples/getting_started
./run.sh
```

更多场景（安全检测规则库 / NEXMark 基准）见仓库内 [core](https://github.com/wp-labs/wf-examples/tree/main/core) 与 [performance](https://github.com/wp-labs/wf-examples/tree/main/performance)。

> 引擎仓库内也带可运行的安全检测规则集（`examples/rules/`，随源码构建使用），
> 矩阵见 [examples/rules/README.md](examples/rules/README.md)。

### 从源码构建（可选）

需要 Rust stable（workspace 依赖 wp-reactor 引擎 crates）：

```bash
cargo build --release --bin wfusion --bin wfl
```

构建后可跑示例自带的全流程脚本（约 20 个规则工程，逐个 lint / 内联测试 / batch 回放）：

```bash
./examples/rules/run_all.sh release
```

## 一个最小的规则

WFL 以**声明式窗口语义**表达检测（五原语 `Bind / Match / Stats / Join / Yield`，所有语法糖编译期归一为同一内核）：

```wfl
rule ssh_brute_force {
    events { c : auth_events && service == "ssh" && result == "failed" }
    match<sip:5m> {                       // 按源 IP 开 5 分钟窗口
        on event { c | count >= 10; }     // ≥10 次失败即触发
        and close { total: c | count >= 30; }
    } -> score(70.0)                      // 命中即评 70 分（支持跨规则累计）
    join scanner_whitelist anti on c.sip == scanner_whitelist.sip   // 白名单排除
    entity(ip, c.sip)                     // 实体 = 源 IP，驱动评分累加
    yield security_alerts (
        sip = c.sip,
        alert_type = "ssh_brute_force",
        detail = "failed attempts >= 10",
        targets = c.dip | values | join(",")
    )
}
```

语法与语义见 [WFL 语言参考](docs/useage/rules.md)；示例按场景编排见 [examples/rules/README.md](examples/rules/README.md)。

## Workspace 组件

| 二进制 | 作用 |
| --- | --- |
| `wfusion` | 引擎主二进制（本地文件/网络源回放 → 规则执行 → alert/错误输出） |
| `wfl` | 规则开发工具：`lint` / `test`（规则内联用例）/ `replay` / `verify` |
| `wfgen` | 数据生成与 oracle 验证；含 `nexmark_pk` 基准工具链 |
| `wfadm` | 管理 CLI（Admin API 状态查询、在线 reload、发布流程） |
| `wf-project-remote` | 远程项目加载库 |

各 crate 变更见 [CHANGELOG.md](./CHANGELOG.md) / [CHANGELOG.en.md](./CHANGELOG.en.md)。

## 示例导览

`examples/rules/` 是**可运行的安全检测场景库**（每个目录自带 schema / 规则 / 数据 / 拓扑 / `run.sh` 断言），例如：

- `ssh_brute_force` / `sqli_probe` / `port_scan_whitelist` — count 阈值 + join anti 排除类
- `rat_propagation` — 多步攻击链时序匹配
- `match_let_demo` — `let` 派生字段复用 + `case` 归一化（issue #79/#83）
- `match_expr_key_demo` — 表达式（`coalesce`）作分组 key（issue #80）
- `two_window_pipeline` / `single_stream_multi_window` — 窗口路由与中间窗口 `|>` pipeline
- `window_miss` / `shared_log_types` — 路由诊断与多源分窗

完整场景矩阵见 [examples/rules/README.md](examples/rules/README.md)。

## 文档

> **用 AI / Agent 辅助开发？建议优先使用 [wf-skills](https://github.com/wp-labs/wf-skills)**——
> 产品级技能集（Claude Code / Codex / 各类 agent 通用），把本仓库经验沉淀为
> 「何时用 → 怎么做 → 已踩过的坑 → 检查清单」，覆盖 schema / 规则 / 配置 /
> **系统集成（5 步接入）** / 基准与正确性验证。一条命令安装：
>
> ```bash
> curl -sSf https://get.warpparse.ai/inst-x.sh | bash -s -- wf-skills
> ```

- **快速上手 / 概念**：[getting-started.md](docs/useage/getting-started.md) · [warp-fusion-intro.md](docs/warp-fusion-intro.md)
- **开发者集成**：[integration.md](docs/useage/integration.md)（把引擎接入自有系统：来源 → 窗口 → 输出路由 → 规则）
- **WFL 语言**：[rules.md](docs/useage/rules.md)
- **运行与配置**：[config](docs/useage/config/) · [cli](docs/useage/cli/cli.md)
- **Admin API / 在线 reload / 发布**：[admin_api.md](docs/useage/cli/admin_api.md)
- **设计与能力**：[design](docs/design/) · [warp-fusion-competitiveness.md](docs/warp-fusion-competitiveness.md)

## 能力定位与性能参照

`warp-fusion` 定位为**通用流处理引擎**，以 WFL 高层语义 DSL 表达规则，轻量化运行。

### NEXMark 性能参照

与 Flink 系**同方法论**对照（100M 事件、in-memory 源 + blackhole 汇、同型号云服务器）：

| 对照基线                         | 几何平均领先    | 算术平均领先 |
| ---------------------------- | --------- | ------ |
| Flink OSS（3×12 vCPU / 48GiB） | **24.3×** | 44.7×  |
| 阿里 VVR（8 CU / 32GiB 托管集群）    | **6.8×**  | 10.1×  |

完整口径与逐查询数据见 [NEXMark PK 报告](https://github.com/wp-labs/wf-examples/blob/main/performance/nexmark_pk/NEXMARK_PK_REPORT.md)。

![WarpFusion vs Flink NEXMark 对照](images/vs-flink.jpg)

### WFL 表达能力

![WFL 五原语 Core IR](images/wfl-five-primitives.svg)

- **五原语内核（Bind / Match / Stats / Join / Yield）**：既写逐事件流式检测，也写声明式窗口统计（`stats<dur> [group by] { 聚合 }`）。
- **检测表达力为核心差异化**：时序链 + OR 分支 + 双阶段匹配（实时/窗口关闭），缺失检测（A→NOT B）；一等实体声明 `entity()` 驱动跨规则评分。
- **覆盖范围**：哈希族、网络 `cidr_match`、多精度时间、对象 `merge`、HOP 跳窗、`anti`/延迟触发 join、规则级 `let`、表达式派生分组 key 等；与 SPL Top50 高频函数对齐率 100%（50/50）。
- **诚实边界**：三角函数、行保留聚合（eventstats 类）等通用计算不在主战场；分项可解释评分为规划项。

## 架构亮点

| 杠杆               | 砍掉了什么                                                 |
| ------------------ | ---------------------------------------------------------- |
| **列批式向量化**  | 逐事件对象分配 + 解释器分发                                |
| **数据零拷贝**     | 消灭 Event→Record→DataRecord 多层拷贝                      |
| **内存精确控制**   | 窗口数据仅过期且被下游全部消费后才释放、数据预读总量设上限 |
| **Rust vs Java**   | 免去 Java 系引擎（Flink 等）的 JVM GC 停顿                 |
| **规则即规划**     | 运行期逐事件解释（Stats/Match 编译期定型为执行计划）       |

## 边界声明

上述领先在**「引擎纯算力 / 单机内存」隔离维度**测得：当前为**单机、无 exactly-once / checkpoint(规划) / 分布式协调开销**。NEXMark 为合成基准，结论作**能力参照**而非生产 SLA 承诺；生产级容错、分布式与有状态一致性补齐后方可对等比较。

## License

`warp-fusion` 及核心运行时采用 **Elastic License 2.0 (ELv2)**。

- **允许**：个人、研究、教学、非营利组织，以及企业**内部自用**（含部署、修改、嵌入自有产品）。
- **禁止**：将本软件作为**托管服务 / 产品对外提供**、销售本软件本身、或绕过授权限制。
- 任何超出上述免费范围的商业用途，需与版权人另行签署商业授权协议。

完整条款见 [LICENSE](./LICENSE)；版权归属 `Copyright (c) 2026 zuowenjian`。

> 注：ELv2 不属于 OSI 认证的开源协议（source-available），但允许企业内部商用。
