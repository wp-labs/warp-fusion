# WarpFusion：零依赖的实时关联计算引擎

> 用声明式规则语言，把「跨多条事件、跨时间窗口」的关联逻辑写成一条条规则——引擎实时匹配，输出可解释、带评分的结果。安全检测只是其中一个典型场景。

## 一句话定位

**WarpFusion 是一个基于 Rust 构建的实时关联计算引擎**：输入结构化事件流，输出满足关联条件的、可解释的结果——可以是告警、评分，也可以是结构化数据。它的核心是跨多条事件、跨时间窗口的模式匹配，而非简单的单条过滤。

单二进制、零外部依赖、秒级关联延迟、空载内存目标 <100MB。既能在单机跑通完整关联链路，也能通过声明式窗口分布平滑扩展到多节点。

## 为什么需要它

单个事件只能告诉你"发生了什么"，回答不了"**这些事件放在一起意味着什么**"。实时关联要解决的是：

- **跨数据源**：登录日志 + 网络流量放在一起，是否构成一个攻击序列；
- **跨时间窗口**：一段时间内同类事件是否超过阈值、不同事件是否有序发生；
- **跨步骤**：多个事件按顺序或条件组合才成立的模式。

这类需求广泛存在于**安全检测、运维监控、风控反欺诈、设备行为分析**等场景。它们都要求引擎能**缓存历史数据、维护有状态窗口、执行模式匹配**。WarpFusion 就是为此而生。

一个具体的例子：安全检测中的「同一 IP 5 分钟登录失败 3 次后发起端口扫描」，就是典型的跨数据源、跨时间窗口、跨步骤关联。

## 它是什么

WarpFusion 由两个代码仓库构成：

- **wp-reactor** —— 核心运行时库：WFL 语言编译器、CEP 状态机、窗口引擎、告警路由
- **warp-fusion** —— 三个 CLI 工具：`wfusion`（引擎）、`wfl`（规则开发）、`wfgen`（测试数据生成）

### 声明式规则语言 WFL

规则用 **WFL**（Warp Fusion Language）编写，采用职责分离的**三文件模型**：

| 文件 | 职责 |
|------|------|
| `.wfs` | 数据长什么样：窗口定义、字段 schema、时间字段 |
| `.wfl` | 匹配什么模式：事件绑定、时序匹配、评分、输出 |
| `.toml` | 怎么跑：source、sink、窗口内存、watermark |

一个「暴力破解」规则长这样：

```wfl
rule brute_force {
    events {
        fail : auth_events && action == "failed"
    }
    match<sip:5m> {
        on event {
            failed_hits: fail | count >= 3;
        }
    } -> score(70.0)

    entity(ip, fail.sip)

    yield security_alerts (
        sip = fail.sip,
        fail_count = stat.count(match_event(failed_hits)),
        first_seen = @event_first_time,
        last_seen = @event_last_time,
        message = fmt("{} brute force detected", fail.sip)
    )
}
```

## 核心能力

### 1. 时序模式匹配——序列、共现与缺失检测

- **`on event seq` 有序序列**：`has A → has B within 10m → not has C within 5m`，支持步间时间约束、否定步、严格相邻——专为多步骤序列设计；
- **`on event any` 无序共现**：所有条件并行满足即触发，乱序事件也能命中；
- **关闭模式**：`on close`（窗口结束时独立判定）/ `and close`（事件与关闭条件共同生效）；
- **窗口类型**：滑动 / 固定 / 会话窗口，覆盖实时告警、定时归并、行为会话多种场景。

### 2. 可解释、可评估的输出

- **实体建模**：`entity(type, id)` 把结果归属到 IP / 用户 / 主机 / 订单号等实体，支持实体维度的归并与行为分析；
- **数值评分**：`-> score(expr)` 给结果一个可比较的数值——安全里是风险，运营里可以是严重度、置信度；
- **稳定统计上下文**：`stat.count(window_event(...))`、`stat.value(trigger(...))` 等把「为什么触发」变成可输出的统计证据；
- **时间系统变量**：`@event_first_time`、`@window_start_time`、`@emit_time` 等把命中时间、窗口边界、分析时间输出为业务字段；
- **结构化输出**：`object { ... }` / `array [...]` 直接构造嵌套结果，无需手拼字符串；
- **公共输出复用**：`yield preset` + 项目级 `_global.wfl`，把多规则共用的输出字段集中声明。

### 3. 逐条打分 → 下游聚合的链式建模

`on each` 无状态逐条打分，中间 window 可被下游规则继续消费；`|>` 管道支持多阶段聚合。上游先做语义 enrichment，下游再做窗口归并——两阶段架构开箱即用。

### 4. 面向正确性的开发闭环

- **内联契约测试**：规则里直接写 `test ... for ...`，`wfl test` 一键验证命中/未命中路径；
- **离线回放**：`wfl replay` 用历史数据验证；
- **对拍验证**：`wfl verify` 把新规则产出与原规则逐字段对拍；
- **场景化数据生成**：`wfgen` 用 `.wfg` 场景文件生成带命中/近失/未命中的测试数据流，支持 `--send` 直接注入引擎。

## 性能与容量

**响应时间**：数据到达引擎后 **1 秒以内**输出告警。事件匹配类规则命中即触发；需窗口累计的规则，在窗口到期时输出。

**容量（8GB 内存基准配置）**：

- 可同时运行 **1000 条**检测规则
- 实时处理**万级事件**（约 1 万 EPS）
- 窗口数据按**分钟级**保留
- 事件缓冲约 **400 万条**（400 个窗口 × 每窗口 1 万条，单条事件约 1KB）

内存与数据量按比例扩展；窗口缓冲按「事件量 × 保留时长 × 事件大小」规划即可。

## 与 Flink / 流计算平台的对比

**在 WarpFusion 最合适的场景——单机 8GB、万级事件、千条规则、秒级响应——下，WarpFusion 的优势是决定性的，但那是「场景合适」，不是「引擎更强」。** Flink 能做的事它都能做，但为这个量级付集群 + Kafka + 规则开发成本，不值。

### Window 分布模型的同源同构

WarpFusion 的分布式模型与主流流计算引擎（Apache Flink、Kafka Streams、Esper）机制**同源同构**，差异在表达方式：

| WarpFusion | Apache Flink | Kafka Streams | Esper (CEP) |
|------------|-------------|---------------|-------------|
| `partitioned(key)` | `keyBy(sip)` + Keyed State | `KTable`（按 key 分区） | `context partition by sip` |
| `replicated` | `broadcast()` + Broadcast State | `GlobalKTable`（全实例全量） | — |
| `local` | Operator State | 单实例 KStream | 默认行为 |

例如三表 JOIN：两张 `partitioned` 的表按同一 key 分区，`replicated` 的小表（如字典、情报）每节点全量——**每个节点本地即可完成关联，零跨节点通信**。区别在于：Flink 用命令式 API（代码里写 `keyBy` / `broadcast`），WarpFusion 用声明式配置（TOML 里声明 Window mode）——**配置即分布策略**。

### 为什么在 8GB / 万级 EPS 这个量级占优

| 维度 | Apache Flink | WarpFusion |
|------|-------------|------------|
| 部署与运维 | 集群（JobManager + TaskManager）+ Kafka + 状态存储 | **单二进制、零外部依赖** |
| 资源预算 | 光 Flink + Kafka 就吃掉 8GB 预算的大半，放不下窗口和规则 | 8GB 内宽裕跑完「1000 规则 + 万级 EPS + 分钟级窗口」 |
| 规则开发 | MATCH_RECOGNIZE 出名地难；Java DataStream 数百行；改规则 = 重编译重部署 job | WFL 几行写序列关联，热加载生效 |
| 规则测试闭环 | 无原生规则测试 | 契约测试 + replay + verify + 场景数据生成 |
| 与上游数据管道 | 需 Kafka 中转 + 自写适配器 | Arrow IPC over TCP 直连、stream-tag 路由 |

### 使用成本对比

| 成本项 | Apache Flink | WarpFusion |
|--------|-------------|------------|
| 硬件 | 至少 2–3 个节点（JobManager + TaskManager + Kafka）+ 磁盘存储 | 1 台 8GB 节点 |
| 中间件 | Kafka + checkpoint 状态存储等额外组件 | 零外部依赖 |
| 运维人力 | 专职流工程师维护集群、调 checkpoint / state | 单二进制，普通运维即可 |
| 开发人力 | 数据工程师写 Java / SQL，每条规则一次 job 生命周期 | 分析师写 WFL，热加载生效 |
| 许可与托管 | 开源免费；托管服务（Confluent / Kinesis）按小时计费 | 开源免费 |

**规则编写成本是差距最大的单项**——一条中等级别的检测规则，从编写到验证上线：

- **WFL**：分析师**数十分钟**内写完（几行声明式代码）+ 内联契约测试一键验证 + 热加载生效。规则增删改**不占用开发资源、不排期**；
- **Flink**：数据工程师写 SQL `MATCH_RECOGNIZE` 或数百行 Java + 编译 + 部署一个 streaming job，**通常以天计**，且每次改规则都要重走一遍 job 生命周期。

**同场景下，Flink 的综合使用成本（硬件 + 运维 + 开发）通常数倍于 WarpFusion**：资源预算差 2–3 倍，运维和开发还要养不同类型的人，规则迭代还慢一个量级。

### 边界——不做过度承诺

| 维度 | Flink 的优势 |
|------|-------------|
| 容错 | exactly-once / Checkpoint；WarpFusion 目前 best-effort（at_least_once 在路线图） |
| 规模 | 10 万 EPS 以上、多节点高可用是 Flink 的场子 |
| 生态与信任 | 十年生产验证、海量 connector；WarpFusion 仍为 v0.1.x |

### 对外口径

> 在单机 8GB、万级事件、千条规则这个量级，WarpFusion 是正好的工具：一个二进制跑完，规则几行写完、自带测试闭环。Flink 能力都有，但为这个量级付集群 + Kafka + 每改一条规则重部署一个 job 的成本，不值。

## 典型场景：安全检测 —— WFL 与主流检测 DSL 的对比

实时关联最成熟的应用是安全事件关联。在安全检测这个场景里，WFL 与 YARA-L 2.0、Elastic EQL、Sigma、Splunk SPL、KQL（Microsoft Sentinel）五种主流检测 DSL 的能力差异如下。

### 能力矩阵（节选）

| 能力维度 | **WFL** | **YARA-L** | **EQL** | **Sigma** | **SPL** | **KQL** |
|---------|---------|-----------|---------|-----------|---------|---------|
| 时序链 | `match` 多步 + OR 分支 | `$e1 before $e2` | `sequence by` | ✗ | `transaction` | ✗ |
| 双阶段匹配 | `on event` + `on close` | 仅 match | ✗ | ✗ | ✗ | ✗ |
| 缺失检测 | `on close { ... \| count == 0; }` | `!$e2` | `!sequence` | ✗ | `NOT` 子搜索 | ✗ |
| 聚合 | count/sum/avg/min/max/distinct | 基础 | ✗ | 基础 | 全功能 | 全功能 |
| 集合收集 | `collect_set` / `collect_list` / `first` / `last` | ✗ | ✗ | ✗ | `values`/`list` | `make_set`/`make_list` |
| 会话窗口 | `match<key:session(gap)>` | ✗ | ✗ | ✗ | `transaction maxpause=` | ✗ |
| 外部关联 | `join snapshot/asof on ...` | 平台侧 | ✗ | ✗ | `lookup` | `externaldata` |
| 数值风险评分 | `-> score(expr)` | ✗ | ✗ | ✗ | 需 eval 手算 | ✗ |
| 分项可解释评分 | `-> score { item @ weight }` | ✗ | ✗ | ✗ | ✗ | ✗ |
| 实体声明 | `entity(type, id)` 必选 | ✗ | ✗ | ✗ | ✗ | ✗ |
| 基线偏离 | `baseline(expr, dur)` | ✗ | ✗ | ✗ | 需外部 ML | Fusion ML |
| 输出契约 | `yield target@vN` + 契约版本 | ✗ | ✗ | ✗ | ✗ | ✗ |
| 正确性门禁 | `test + shuffle + scenario verify` | 平台回放 | 平台测试 | ✗ | 平台测试 | 平台测试 |

### 逐 DSL 要点

- **vs YARA-L（Google Chronicle）**：WFL 在检测表达力上全面超越。核心差异是 OR 分支时序 + 双阶段匹配、分项可解释评分、一等实体声明——YARA-L 完全没有实体行为建模能力；session window / collect / baseline / score 进一步拉开差距。
- **vs Elastic EQL**：EQL 定位是事件查询语言（绑定 ES），WFL 是独立检测+行为分析引擎。EQL 的 `sequence` 更简洁、字符串函数更丰富，但缺聚合、行为分析、风险评分和输出能力。二者不是同层竞争。
- **vs Sigma**：Sigma 是「规则分发格式」，WFL 是「执行语言」。Sigma 赢在可移植性和 5000+ 社区规则，WFL 赢在表达力与执行语义。二者互补——WFL 可考虑支持 Sigma 规则导入。
- **vs Splunk SPL**：WFL 的新增差异化是分项可解释评分、一等实体建模、跨规则评分累加、内置基线——SPL 在语言层完全不具备，需靠 eval 手算或 MLTK 外部模块。SPL 仍在通用计算（200+ 函数、无限管道、eventstats）上保持优势。
- **vs KQL（Sentinel）**：KQL 没有原生时序链和会话窗口检测（Sentinel Fusion 是 ML 驱动而非规则驱动），WFL 在多步序列和行为分析上有结构性优势；实体建模和可解释评分是 KQL 语言层缺失的。

### 定位总结

> **WFL 是唯一同时提供时序检测、实体建模、可解释数值评分、内置基线的独立 DSL。**

SPL / KQL 通过平台能力（ML 模块、外部插件）可实现类似效果，但不是语言层原语——**WFL 把这些能力内化为编译期可检查、运行期可解释的语言一等公民**。

## 架构与实现

一个二进制里，事件是这样流动的：

- **Window 即订阅者**：窗口不是被动缓冲区，而是带订阅条件的、时间有序的数据持有者；同一数据流可被多个窗口以不同方式订阅；
- **Arrow IPC over TCP**：接收端零反序列化、类型保真、DataFusion 原生支持——从上游解析引擎（如 WarpParse）到规则引擎全程免解析；
- **事件时间语义**：基于事件时间的 watermark / 迟到处理 / 窗口 TTL，输入静默时实例也按窗口语义自动过期；
- **Pull-based 执行**：每条规则独立 task，cursor 拉取 + Notify 唤醒，无需全局调度器；LIFO 启停保证管道零告警丢失。

## 现状与路线图

- **已落地**：单机 MVP 全闭环（WFL 编译 → CEP 执行 → 告警输出）、时序序列/共现、结构化输出、契约测试与数据生成工具链、项目级 preset 预加载、monitor-sink 指标；
- **演进中**：`join` 执行、`at_least_once` / `exactly_once` 传输可靠性、`limits` 资源预算、多节点分布式部署（声明式窗口分布已设计）。

## 快速开始

```bash
git clone https://github.com/wp-labs/warp-fusion.git
cd warp-fusion && cargo build --release
```

```bash
# 1. 内联测试：验证规则逻辑
wfl test rules/brute_force.wfl --schemas "schemas/*.wfs"

# 2. 离线回放：用历史数据验证
wfl replay rules/brute_force.wfl --input data/events.ndjson

# 3. 引擎运行：完整实时链路
wfusion daemon -c ./wfusion.toml
```

一个完整的最小示例（`.wfs` + `.wfl` + `wfusion.toml` 三文件）见仓库 `examples/`。

## 资源

- 核心运行时：`github.com/wp-labs/wp-reactor`（docs/user-guide：快速开始、语言参考、规则编写、运行时配置）
- CLI / 工具：`github.com/wp-labs/warp-fusion`（docs：getting-started、configuration、rules、CLI）
- 语言设计：WFL v2.1 设计方案、WFL 与主流 DSL 对比分析
