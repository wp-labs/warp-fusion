# WarpFusion 介绍 · PPT 内容稿

> 用途：面向潜在客户 / 技术决策者的产品介绍演讲，约 15 分钟，18 页。
> 每页 = 一页幻灯片；`【备注】` 为演讲者提示；`【页脚】` 为演示时建议保留的角标。
> 详细资料见 [intro.md](./intro.md)。

---

## Slide 1 · 封面

**WarpFusion**
零依赖的实时关联计算引擎

副标题：用声明式规则语言，把跨事件、跨时间窗口的关联，变成可解释、带评分的结果。

【备注】开场白一句话："今天讲一个工具——把'一堆事件放在一起意味着什么'这件事，变成一条条规则来实时回答。"

【页脚】WarpFusion · 实时关联计算引擎

---

## Slide 2 · 一句话定位

- 基于 **Rust** 的实时关联计算引擎
- 输入：结构化事件流　→　输出：**可解释的结果**（告警 / 评分 / 结构化数据）
- 核心：**跨多条事件、跨时间窗口**的模式匹配，而非单条过滤
- 单二进制、零外部依赖、秒级响应、空载内存 <100MB

【备注】重点讲"跨事件跨窗口"，和"无状态解析管道"划清界限。

---

## Slide 3 · 为什么需要它

单个事件只能告诉你"发生了什么"，回答不了"**这些事件放在一起意味着什么**"。

- **跨数据源**：登录日志 + 网络流量，是否构成一个攻击序列
- **跨时间窗口**：一段时间内同类事件是否超阈值、是否有序发生
- **跨步骤**：多个事件按顺序 / 条件组合才成立的模式

适用场景：**安全检测、运维监控、风控反欺诈、设备行为分析**。

【备注】举例：安全里的"5 分钟 3 次登录失败后发起端口扫描"。强调这类需求是跨领域的。

---

## Slide 4 · 它是什么

- 两个仓库：
  - **wp-reactor** —— 核心运行时（WFL 编译器、CEP 状态机、窗口引擎）
  - **warp-fusion** —— 三个 CLI：`wfusion`（引擎）、`wfl`（规则开发）、`wfgen`（数据生成）
- 声明式规则语言 **WFL**
- 三文件模型，职责分离：

| 文件 | 职责 |
|------|------|
| `.wfs` | 数据长什么样（窗口、字段、时间） |
| `.wfl` | 匹配什么模式（绑定、时序、输出） |
| `.toml` | 怎么跑（source、sink、内存、watermark） |

【备注】强调"规则由业务分析师写，不写代码"。

---

## Slide 5 · 规则长什么样

一条"暴力破解"规则：

```
rule brute_force {
    events { fail : auth_events && action == "failed" }
    match<sip:5m> {
        on event { failed_hits: fail | count >= 3; }
    } -> score(70.0)
    entity(ip, fail.sip)
    yield security_alerts (
        fail_count = stat.count(match_event(failed_hits)),
        first_seen = @event_first_time,
        message = fmt("{} brute force detected", fail.sip)
    )
}
```

【备注】不需要观众读懂每行，只传达两件事：① 声明式、像写需求不写代码；② 评分、实体、证据时间都是语言内置。

---

## Slide 6 · 核心能力 ① 时序模式匹配

- `on event seq`：**有序序列**——`has A → has B within 10m → not has C within 5m`（步间约束 / 否定步 / 严格相邻）
- `on event any`：**无序共现**——条件并行满足即触发
- `on close` / `and close`：**关闭判定**（窗口结束时独立触发 / 与事件共同判定）
- 窗口类型：**滑动 / 固定 / 会话**

【备注】这是"关联"的核心——不是统计，是"先 A 后 B"这类时序逻辑。

---

## Slide 7 · 核心能力 ② 可解释、可评估的输出

- **实体建模**：结果归属到 IP / 用户 / 主机 / 订单号
- **数值评分**：`score(expr)`，安全里是风险、运营里是严重度/置信度
- **统计上下文 + 时间变量**：把"为什么触发"变成可输出的证据
- **结构化输出**：`object { ... }` / `array [...]`，JSON 原生，不拼字符串

【备注】对客户价值：结果不只是"告警了"，而是"为什么、有多严重、证据是什么"。

---

## Slide 8 · 核心能力 ③ 链式建模 + 正确性闭环

- **链式**：`on each` 逐条打分 → 中间窗口 → 下游 `|>` 聚合
- **内联契约测试**：规则里写 `test ... for ...`，一键验证命中/未命中
- **回放 / 对拍 / 数据生成**：`wfl replay` / `wfl verify` / `wfgen`

【备注】强调"规则能测试、能对拍"，这是规则生命周期里最容易被忽视、又最值钱的部分。

---

## Slide 9 · 性能与容量

- **响应**：数据到达引擎后 **1 秒以内**输出
- **容量（8GB 内存基准）**：

| 指标 | 数值 |
|------|------|
| 同时运行的规则 | **1000 条** |
| 实时吞吐 | **200 万–3100 万 EPS**（NEXMark 单查询 / Linux 8 核 / blackhole 汇，2026-08-27） |
| 窗口保留 | 分钟级 |
| 事件缓冲 | 约 400 万条（400 窗口 × 1 万条） |

【备注】**两条已作废口径**：①「万级 EPS（约 1 万）」；②「口径要保守、别主动提更大数字」——均属早期刻意保守，NEXMark 实测已达千万级，**对外应主动标明口径而非压低数字**。
**引用纪律**：只引 NEXMark 单查询口径（blackhole 汇，不落告警）。多规则混合负载的历史测量（2026-08-10）**已过时且未复测，不得引用**；告警密集场景请按自身告警率实测。内存按"事件量 × 保留时长 × 事件大小"线性规划。

---

## Slide 10 · 与 Flink 对比 ① 同源同构

- 分布模型和 Flink **机制同源**：

| WarpFusion | Flink |
|------------|-------|
| `partitioned(key)` | `keyBy` + Keyed State |
| `replicated` | `broadcast` + Broadcast State |
| `local` | Operator State |

- 差异在表达：Flink 命令式（代码写 `keyBy`）；WarpFusion **声明式**（TOML 声明 Window mode）

【备注】想传达：不是"又一个 Flink"，而是"同一个模型、更简单的声明"。

---

## Slide 11 · 与 Flink 对比 ② 为什么在 8GB / 万级 EPS 占优

| 维度 | Flink | WarpFusion |
|------|-------|------------|
| 部署 | 集群 + Kafka + 状态存储 | **单二进制、零依赖** |
| 8GB 预算 | Flink+Kafka 就吃掉大半 | **8GB 内宽裕跑完全场景** |
| 规则开发 | MATCH_RECOGNIZE / 数百行 Java，改规则 = 重部署 | **几行 WFL，热加载** |
| 测试闭环 | 无原生规则测试 | 契约测试 + replay + verify |

【备注】核心一句话：这个量级 Flink 是"杀鸡用牛刀"，而且 8GB 里连刀都放不下。

---

## Slide 12 · 使用成本

| 成本项 | Flink | WarpFusion |
|--------|-------|------------|
| 硬件 | 2–3 节点 + 磁盘 | 1 台 8GB 节点 |
| 中间件 | Kafka + checkpoint 存储 | 零 |
| 运维 | 专职流工程师 | 普通运维 |
| 开发 | 数据工程师，每条规则一个 job | 分析师写 WFL |

- ~~**规则编写成本差距最大**：WFL 数十分钟；Flink 以天计~~ → **AI 时代该说法已作废，勿再讲**。改为：
  **"AI 时代，WFL 反而更好用"**——判断语言对 AI 是否友好看三项：**表面积小**（几行声明式）、
  **语义明确**（entity/score/match 显式）、**验证快**（`wfl test` 秒级）。实践中**规则已可完全由 AI 生成**，
  人只做审核验收。Flink 侧 AI 秒出数百行 Java，仍要起环境/造数据/部署 job 才知道对错，隐式约定多、出错面大。
- ⚠ 若被问"AI 能写好 WFL 吗"：**正面回答**——能，且效率高；支撑设施是 `wf-skills`
  （6 个技能装进 Claude/Codex）+ `wf-rules` 语料 + LSP 即时校验。
  **但必须补一句**：AI 生成 ≠ 免验证，检测规则出错是静默漏报/误报，产出必须跑 `wfl test` 并纳入回归。
- 综合成本通常**数倍**于 WarpFusion

【备注】这个数字是估算口径，用于对比量级，别当承诺。

---

## Slide 13 · 边界（不回避）

- **容错**：WarpFusion 目前 best-effort，`at_least_once` 在路线图；Flink 有 exactly-once
- **规模（吞吐不是瓶颈）**：8 核单查询实测 **200 万–3100 万 EPS**；真正的边界是**多节点高可用与分布式容错**（无 exactly-once / checkpoint），这部分是 Flink 的场子
- **生态与信任**：Flink 十年生产验证、数百个社区 connector；WarpFusion 核心运行时 2.0.8 / CLI 0.5.3，仍在快速演进。**但勿说"我们没有 connector"**——我们有基于注册表的 connector 体系（可外部扩展），`wp-connectors` 覆盖 Kafka、MySQL、PostgreSQL、Doris、**Elasticsearch、ClickHouse**、VictoriaLogs、VictoriaMetrics、Prometheus、HTTP、UDP 及 file / tcp / syslog，差距是**广度**不是有无。（注：上游 README 曾把 ES/ClickHouse 标为 placeholder，2026-08-30 代码核查确认已实现，勿再沿用旧说法）

【备注】主动讲边界反而加分：让客户知道"我知道我的场景在哪、你的场景在哪"。

---

## Slide 14 · 典型场景：安全检测 —— 语言对比

- 实时关联最成熟的应用是**安全事件关联**
- 在安全检测场景，WFL vs 主流检测 DSL（YARA-L / EQL / Sigma / SPL / KQL）：

| 独有能力 | 说明 |
|---------|------|
| 时序 + 双阶段 | `on event` + `on close`，多数 DSL 没有 |
| 实体建模 | `entity(type, id)`，语言一等公民 |
| 可解释评分 | `score` / 分项评分，SPL/KQL 需手算或外部 |
| 内置基线 | `baseline(expr, dur)`，别家靠外部 ML |

> **WFL 是唯一同时提供时序检测、实体建模、可解释评分、内置基线的独立 DSL。**

【备注】一句话收："在安全检测这个场景，我们的语言是唯一把这几件事都做成一等公民的。"

---

## Slide 15 · 架构与实现

- **Window 即订阅者**：带条件的、时间有序的数据持有者
- **Arrow IPC over TCP**：接收端零反序列化、类型保真
- **事件时间语义**：watermark / 迟到处理 / 窗口 TTL
- **Pull-based 执行**：每条规则一个 task，无全局调度器；LIFO 启停零告警丢失

【备注】技术页，快速带过，给懂行的观众吃定心丸："不是玩具，是有正经架构的。"

---

## Slide 16 · 现状与路线图

- **已落地**：单机全闭环、时序序列/共现、结构化输出、契约测试 + 数据生成工具链、项目级 preset 预加载
- **演进中**：`join` 执行、`at_least_once` / `exactly_once`、`limits` 资源预算、多节点分布式

【备注】诚实："我们核心运行时 2.0.8 / CLI 0.5.3，仍在快速演进；单机这个量级已经完整可用，多节点高可用与 exactly-once 仍在路上。"（**勿再说 v0.1.x**）

---

## Slide 17 · 快速开始

```bash
# 1. 内联测试：验证规则逻辑
wfl test rules/brute_force.wfl --schemas "schemas/*.wfs"

# 2. 离线回放：用历史数据验证
wfl replay rules/brute_force.wfl --input data/events.ndjson

# 3. 引擎运行：完整实时链路
wfusion daemon -c ./wfusion.toml
```

【备注】"三行命令，从验证到跑通。最小示例在仓库 examples/。"

---

## Slide 18 · 结尾

**WarpFusion —— 零依赖的实时关联计算引擎**

- 单二进制、零外部依赖、秒级响应
- 8GB 跑 1000 条规则 / 万级事件
- 声明式 WFL，分析师写规则，自带测试闭环
- 安全检测是典型场景，不止于安全

资源：
- 核心运行时：`github.com/wp-labs/wp-reactor`
- CLI / 工具：`github.com/wp-labs/warp-fusion`

【备注】收尾："如果你们的场景是单机、万级事件、几百上千条关联规则——先花 10 分钟试一下这个工具。"

【页脚】谢谢 · 欢迎交流
