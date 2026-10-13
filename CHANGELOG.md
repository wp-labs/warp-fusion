# Changelog

本文件记录 `wfusion` / `wfl` / `wfgen` / `wfadm` 面向使用者的变更。内部实现细节、依赖版本对齐和测试计数不在此展开。

## [0.10.0-alpha] — 2026-10-13

### 新增

- **WFL 新增 `to_int(x)`**：把数字字符串字段（如 `"6"`、`"1036"`）转成整数，可直接用于数值比较（`to_int(s.count) >= 3`），无需再用 `regex_match` 模拟数值范围；空串 / 非数字 / 非整值（`"6.5"`）/ 缺失得到 null（条件不成立）。

### 修复

- **WFL `case` 的 subject 缺失或为 `null` 时改走 `_` 默认分支**：`case e.severity { "high" => "HIGH", ..., _ => "UNKNOWN" }` 在 severity 字段缺失或为 null 时得到 `"UNKNOWN"`，而非空值；未写 `_` 时仍得 null。

### 引擎（对齐 wp-reactor 2.6.0）

- 引擎对齐 wp-reactor 2.5.1 → 2.6.0：带入上述 WFL `to_int` 与 `case` null 语义修复。无其他源码改动。

## [0.9.0-alpha] — 2026-10-12

### 引擎（对齐 wp-reactor 2.5.1）

- 引擎对齐 wp-reactor 2.4.0 → 2.5.1；整条依赖链升级到 **orion-error 0.9**（`orion-variate` 0.14 / `orion_conf` 0.8），并同步连接器依赖到 0.9 世代：`wp-connector-api` 0.14、`wp-connector-utils` 0.5、`wp-core-connectors` 0.10.1、`wp-connectors` v0.23.1、`wf-connector-api` 0.4、`wp-self-update` 0.4、`wp-knowledge` 0.19。
- 目的：依赖图中不再同时出现 orion-error 0.8 与 0.9 两个互不互通的 `StructError`。无源码改动，行为不变。

## [0.8.0-alpha] — 2026-10-12

### 新增

- **monitor 指标可按配置筛选 `[metrics.monitor]`**（warp-fusion#115）：可按 `stage` / `name`（指标名）/ `target`（标签）的通配规则选择哪些运行指标发往 monitor sink（文件 NDJSON、VM 等），支持 `include` / `exclude`（`exclude` 优先）与 `default = "include" | "exclude"`。未配置时行为不变；详见 [`docs/useage/config/metrics.md`](docs/useage/config/metrics.md)。

### 引擎（对齐 wp-reactor 2.4.0）

- 引擎对齐 wp-reactor 2.3.0 → 2.4.0：新增 monitor 指标筛选（`[metrics.monitor]`）；被排除的指标不再写入 monitor sink，仅 monitor 读的指标族启动期即不采集。

## [0.7.0]

`.wfg` 注入语法重写（**破坏性**，现有场景需改写）：旧写法在加载期报错并给出改写方向，语法用法见
[`docs/useage/scenarios.md`](docs/useage/scenarios.md)、旧写法逐条对照见
[`docs/design/wfg-design.md`](docs/design/wfg-design.md) §5。引擎对齐 wp-reactor 2.3.0、连接器对齐 wp-connectors 0.21.0。

### 新增

- **生成期硬断言 INJ1 / INJ2**：`hit` 的每个实体必须报警、`near_miss` / `miss` 的每个实体必须不报警，失败点名实体。
- **跨流注入 `join <window> as <key> { … }`**：为规则的 join 目标窗造配对事件（支持 snapshot 与 join-then-key 形态）。
- **背景实体分布 `entity <window>.<field> zipf(…)`**：背景按 Zipf 权重从实体池抽取并保留新实体比例，值域与注入实体分区。
- **`replay <window> { use from "file" }`**：把现成数据原样灌进去（不写条数、不做实体数学、不参与断言）。
- `use(...)` 可以写结构化值（object / array / `null`，含多层嵌套），规则里可以用 `s.source_finding_obj.title` 这类嵌套路径读。
- `without(preds) [within D]`：声明「该实体的窗口内不得出现匹配事件」。
- `on each` 规则可作注入目标；数量显式写（总条数 = 背景配额 + 注入）。

校验期新增 13 个错误码（VN22–VN35，触发条件见 [`docs/design/wfg-design.md`](docs/design/wfg-design.md) §4.1）。

### 修复

- `wfgen verify` 不再把「空对空」算成通过（两侧都是 0 条时此前报 `pass`）——要接受空输入用 `--allow-empty`（`wfl verify` 同名旗标）。
- `--no-expect` 不再静默关掉注入断言，并清掉上一次遗留的期望文件（陈旧期望会让对拍拿旧期望比新数据）。
- `use(...)` / `without(...)` / join 块里写实体键字段或 schema 时间字段一律拦下（VN12 / VN35）：此前会被静默换成别的值，断言随之指错实体；join 驱动侧的连接键值也不再需要手写 `use(...)` 去凑。
- 对拍口径修正：`digit` 按列类型取值（>2^53 的 ID 此前被量化成另一个数）；链式规则的中间管道输出不再当最终告警写进期望；`stats` 收口口径对齐引擎。
- 背景不再与注入实体的键值撞车（负样本的「必不报警」此前可能被背景噪声破坏）。
- `use from "file"` 现在真正生效（此前文件里的值用不上）。
- 场景注解只认 `duration` / `seed`（其他键此前被静默忽略）；`wave` / `burst` / `timeline` 此前按常量速率生成（压测强度静默失效）。
- `.wfg` 的 `lint` 不再 panic；join 块里的 `use from` 会被解析；同一窗口重复声明 `stream` 会被拦下。
- `wfgen lint` 现在也检查规则语义（此前只看 `.wfg` 结构，会出现「`lint` 说 OK、`gen` 才报错」；阈值非常量这类能过编译、运行期却永不触发的写法现在 `lint` 就会报出）。
- `close` 规则尾部实例的时间此前可能与引擎对不上；`array/<base>` 此前丢失数组结构与值。
- `wfgen verify` 的对拍匹配显著提速；`wfgen dump-frames` 不再需要一个在跑的 daemon。
- admin API 的超限请求体稳定返回 413（此前偶发连接被重置）。

### 命名

“oracle”向用户侧统一改叫**期望**（标识符不动）：`--no-oracle` → `--no-expect`；`verify` 报告字段 `oracle_total` → `expected_total`、`schema` 升到 `/v2`；文档改名 `docs/EXPECTATION_VERIFY.md`。

### 引擎（对齐 wp-reactor 2.3.0）

- 新增在线行为基线检测（`judge` / `detect`）与 `baseline_dev` / `sumsq` / `phase_bucket` 内建；外部供给刷新一致性提升、供给 SQL 变量配置错误改为启动即报错。
- 规则里「能编译、但永远不生效」的写法改编译期报错（位置不对的集合 / 统计函数与 `window.has` / `baseline`、无限定 `has(...)`、`stats` 桶键写成表达式）。
- `|v| >= 2^53` 的大整数不再被量化；修掉几处静默丢数据的输入路径（`arrow_framed` 文件源丢批、接收侧不支持的列组合让投影作废）。

## [0.6.3]

### 修复

- **`first(field)` 超过字段历史上限后漂移（issue #100）**：窗口内超过 1024 条事件后最早样本被丢弃，用它组成聚合唯一键或 `alert_id` 会输出多个唯一键；现固定为实例内最早事件的值（样本上界 1024→1025，`collect_*` / `min` / `max` / `sum` / `avg(alias.field)` 会包含最早样本）。
- **阈值表达式必须是编译期常量（issue #101）**：字段引用、非常量 `let`、函数调用（`first` / `collect_*` / `now*` / `baseline` 等）此前能通过编译，但运行期该分支永不触发且无报错；现编译期拒绝。

### 语言

- 规则级**常量** `let`（如 `let THRESHOLD = 3`）可直接作阈值；非常量 `let` 仍拒绝，pipeline stage 内不可引用规则级 `let`，`let` 重名报错。
- 超复杂输入改为编译期报错（此前是编译进程栈溢出 abort）：表达式嵌套分组 ≤ 5 层、同一分组内算子链 ≤ 16 层、规则级 `let` 引用链 ≤ 5 层；`let` 前向引用改为明确的声明顺序错误。

### 引擎

- 对齐 wp-reactor v2.0.24。

## [0.6.2]

### 修复

- **L3 序列函数只写在规则级 `let` 中时输出为空（issue #99）**：`first` / `last` / `collect_*` 等只出现在 `let` 里（yield 以 let 名间接引用）时结果为空（entity 输出 `alert_id` 为空）；现已修复，与直接写在 `yield` 中一致。

### 引擎

- 对齐 wp-reactor v2.0.23：含异步落盘 flush 竞态修复与 `@first_match_time` 语义文档/覆盖（公开 API 与规则语义不变）。

## [0.6.1]

### 修复

- **Kafka NDJSON 数字时间戳不再转 null（issue #95）**：NDJSON→Arrow 解码支持 JSON 数字时间戳（秒/毫秒/微秒/纳秒按位宽归一化）、数字字符串与 `%Y-%m-%d %H:%M:%S`——Kafka 与文件输入的时间字段一致；布尔文本（`1/0`、大小写、空白）同步对齐（wp-connector-utils 0.2.1）。

### 变更

- **工程内务**：移除 `wfusion` CLI 遗留的 moju 建模注解及其依赖（公开行为不变）；依赖树常规升级（arrow 59.3 等）。

## [0.6.0]

### 变更

- **alpha → beta 通道提升**：内容等同 alpha v0.5.10——`events` 条件复用规则级字符串字面量 `let` 正则（issue #90）、对齐 wp-reactor v2.0.21 与 wp-connectors v0.20.0；版本线进入 0.6.x。

## [0.5.10]

### 语言（对齐 wp-reactor 2.0.21）

- **`events` 条件支持复用规则级字符串字面量 `let` 正则（issue #90）**：同一正则可在多个字段（如 URI/请求头/请求体）的 `regex_match` 中按名引用，`let` 可置于 `events` 之前或之后；语义与内联一致，非法/未定义引用静态报错。

### 引擎

- 对齐 wp-reactor v2.0.21（其余为内部重构，公开 API 与规则语义不变）；`wp-connectors` 对齐 v0.20.0。

## [0.5.9]

### 修复

- **TCP 连接修复**：`wp-core-connectors` 0.8.3 → 0.8.4——修复 TCP 连接回归（socket2 回退 0.6.x → 0.5.10）。规则语言与公开 API 不变。

## [0.5.8]

### 引擎（对齐 wp-reactor 2.0.19）

- 对齐 wp-reactor v2.0.19：内存驱逐 WARN 防刷屏（issue #86）——驱逐告警改为首条详报 + 滚动/收尾汇总，不再每条驱逐刷屏；拒收/抖动计数保留供指标告警。其余为内部工程重构（公开 API 与规则语义不变）。

## [0.5.7]

### 引擎（对齐 wp-reactor 2.0.18）

- 对齐 wp-reactor v2.0.18（2.0.16–2.0.18 均为内部工程重构，公开 API 与规则语义不变）。

### wfl

- **L1 结构化回执**：`wfl test --format json`（verify 同构）——断言级通过/失败回执。
- **L2 对抗测试生成**：`wfl test --gen-negatives`——按 bind-guard（`field == literal` / `!= literal`）自动追加违反行反例，断言命中数不变。
- **L3 检测意图编译**：`wfl intent`——`.wfi` 正样本（`hits >= 1`，漏报）/负样本（`hits == 0`，误报）编译回执。
- **`wfl limits-est`**：按 `metrics.ndjson` 实测峰值推荐 `max_memory` / `max_instances` = 峰值 × headroom（默认 2）。

### wfgen

- **L4 性能门禁**：`wfgen perf-diag --gate`，超限即红。

### 示例 / 文档

- 新增 `hello_detection` 集成示例与 memory-limits 指南；`ssh_brute_force` 回放补 `_stream` 字段；README 定位与许可说明更新。

## [0.5.6]

### 语言（WFL，对齐 wp-reactor 2.0.15）

- **match 分组 key 支持函数/字面量派生表达式（issue #80）**：`coalesce(s.a, s.b, "unknown")` / `concat` / `case` / 字面量等表达式的求值结果可直接作窗口分组 key（`match<k:10m>`），与普通字段 key 混用；`let` 引用链编译期展开为纯事件表达式。
  - 缺失语义：`coalesce` 将空串视为空值，需非空兜底（如 `"unknown"`）才稳定归组不丢事件；无回退且求值为空时与 key 缺失一致（事件不入组）。
  - 表达式派生 key 支持 `rule_shards` 分片：同派生键事件恒入同分片，跨事件窗口聚合状态完整。
  - 窗口统计/查询类与 `now*` 时间函数不可作 key（checker 编译期报错）。
  - 示例：`examples/rules/match_expr_key_demo`（coalesce 实体归一作分组键）。
- `match_let_demo` 示例补充 let 派生字段作 match key 的写法（issue #83 形态，与直接写字段结果一致）。

### wfl

- **`wfl verify` EOF 收口修正（issue #23）**：对 span 短于窗口的验证数据，`and close` 规则不再等窗口自然过期——EOF 统一 close 剩余实例，verify 与期望命中一致。

### 引擎（对齐 wp-reactor 2.0.15）

- 对齐 wp-reactor v2.0.15（match 表达式派生 key、fanout 表达式分片、异步落盘等待看门狗等）。

## [0.5.5]

### 语言（WFL）

- **`let` 派生字段（issue #79）**：规则内多处输出共享的复杂逻辑一处定义、链式引用（如 `dedup_key` → `alert_id`），`entity` / `yield` / `where` / `score` 均可按裸名引用；支持 match / close / on-each / deferred 路径，stats 规则暂不支持（checker 报错）。
- **`case` 值分派表达式（issue #79）**：`case x { "crit" | "alert" => "CRITICAL", _ => x }` 替代多层 if/else 做枚举归一化（多模式 `|` / 默认 `_` / 短路求值）；关键字用 `case`，`match` 保留给规则级 CEP 子句。
- 示例：`examples/rules/match_let_demo`（let 链式派生 + case 归一化）。

### 引擎（对齐 wp-reactor 2.0.12）

- 对齐 `wp-reactor` v2.0.12：`rule_parallelism` → `rule_shards`（无状态 each 规则整批分片，输出链并行）；`parse_parallelism` / `parse_buffer_bytes` 废弃（引擎忽略）。

### wfgen

- **`verify-nexmark --detail-diff`**：期望字段级明细对拍——每条告警的 yield 字段值与引擎输出逐行比较，验证从计数级提升到字段级。

## [0.5.4]

### 引擎（对齐 wp-reactor 2.0.10）

- 对齐 `wp-reactor` v2.0.10，核心是 join 索引多 key 支持与多规则对拍正确性修复：
  - **join 索引多 key**：多规则在同一窗口按不同 key 字段 join（q8 按 seller / q20 按 id 共窗）时，后注册者原先回退全窗扫描 O(全窗)×pending、混跑冻结；现在每个 key 字段各建索引，`mix` 混跑恢复可用。
  - **多规则正确性**：q8/q11/q6/q7 对拍定位的一组修复（join 索引字段校验 / 分片冲突检测 / stats 键字段注入 / close_all 水位对齐粒度）。
  - **snapshot join**：首批索引竞态 + gate 性能回退修复（q20 回退 10–12%→3.7%）。
  - 关机尾批丢失 + q13 中间管道消费竞态修复。

### 许可

- 许可证明确为 **Elastic License 2.0（ELv2）**：企业内部自用免费（含部署 / 修改 / 嵌入自有产品）；作为托管服务对外提供需商业授权。

## [0.5.3]

### 语言（WFL）

- **顶层列表 + `use` 导入（issue #73）**：`name = ("a", "b", ...)` 一处定义，多规则 `expr in <name>` / `expr not in <name>` 引用；`use "lists.wfl"` include 导入目标文件全部顶层列表（无可见性控制，全部可见）。
  - 编译期展开为字面列表 + **InList 类型检查**（元素-左值类型比对，字面与命名列表统一；混合类型/不兼容报错）。
  - 错误面：未知名 / use 目标缺失 / 循环引用 / 重名 → 报错（lint 与 compile 同一条路径）。
  - `wfl lint`/`test`/`replay`/`explain` 四命令统一走 `load_wfl_with_imports`（use 解析）；`wfl fmt`（tree-sitter 语法）对列表声明的支持待 tree-sitter-wfl 同步更新。

### 引擎（对齐 wp-reactor 2.0.9）

- 对齐 `wp-reactor` v2.0.9（内部引擎修复）。

### wfgen

- **stats 规则接入期望对拍**：q15–q19 verify 一致；期望按绑定窗口喂行 + 中间事件入队。

## [0.5.2]

### 引擎（对齐 wp-reactor 2.0.8）

- 对齐 `wp-reactor` v2.0.8；`mimalloc` 周期回收（`WF_COLLECT_MS`，默认 5s），q18 RSS 显著优化。

## [0.5.1]

### wfusion

- **mimalloc 内存分账**：`mi_process_info` 暴露的进程内存计入 `metrics alloc.*`，可观测真实内存占用。

### wfgen

- **发送缓冲可配**：`WFGEN_SEND_BUF`（默认 1MB）；perf-diag 发送 8KB→1MB（注入 620MB/s→6.9GB/s）。
- **perf-diag 流式发送**：消除整文件读入内存（30M 6.4GB×2 峰值下降）。

## [0.5.0]

### wfgen —— NEXMark 数据生成与期望对拍

- **数据生成对齐 Flink 官方**：`gen-nexmark` 分布参数逐项修正（字符串字段 / extra padding / 固定 100µs 事件速率 / nextExtra 区间 / cold 90% / horizon 毫秒取整）；`bid.url` 对齐官方 `getBaseUrl`（3 段目录，支撑 q22）；`bid` 增 `channel_id` 字段（q21 对齐）。
- **`gen-nexmark --check` 自检**：值域 / 时间戳 / 流计数 + md5 指纹 + 流序自检；`--check` / `verify-nexmark` 输出 Flink NEXMark 符合性声明。
- **`verify-nexmark` 期望对拍**：新增 Rust 版 NEXMark ground-truth 模拟器，改用真实 WFL 规则引擎对拍；补 deferred join / 帧内跨流时间序 / join 窗口状态（q21 打通）/ 中间输出 feed 下游 + 并查集分组（q13 双规则链）；`known-diff` 机制（q12/q17）；按 auction 分片并行（100M 5min→44s）。
- **`diff` 命令**：分层文件比对（L1 哈希相同性 / L2 Myers 差异量 / L3 `--detail` 定位）。
- **终端进度条**：`gen-nexmark` / `verify-nexmark`（stderr、仅 TTY）；非 TTY 降级输出完成摘要。
- **`send-arrow` 注入控制**：`--rate-bytes` 限速（默认 0=不限速）；1MiB 大缓冲 TCP copy（替代 8KiB）。

### 性能诊断（perf-diag）

- **哨兵四元组体系**：`wfgen`/`wfusion` 支持 `--perf-diag` 驱动；`send-arrow`/`stream` `--sentinel <n>` 在数据末尾追加 `__wf_sentinel` 完成帧（分连接哨兵），用于精确 EPS/CPU 统计。

### wfadm

- `wfadm init` 自动生成 `business.d/sentinel.toml`（perf-diag 哨兵落盘组）；docker 默认设置补 sentinel sink 模板。

## [0.3.1]

### 引擎（对齐 wp-reactor 1.0.2）

- 对齐 `wp-reactor` v1.0.2，预读预算（`parse_buffer_bytes`）改用内容字节记账：
  - **记账单位修正**：预算不再按解码后 Arrow 内存计（IPC 解码会结构性高估真实内存 ~10×，把管线槽位卡死），改按内容字节（≈ wire）计，与窗口记账口径一致；
  - **默认 256MB → 128MB**：避开 256MB 的 12-14GB RSS 平台，实测吞吐不降反微升（q1 100M 6.13M / RSS 5.88GB）；要高吞吐显式调大（512MB-2GB 为甜点，4GB 过度缓冲反而退化）。

### wfgen

- **`shard-frames` 分片文件 + 多连接回放**：`shard-frames` 把一个帧文件按键拆成 N 个分片文件（`--shards` / `--shard-keys`，同 key 必落同一文件）；`send-arrow --shard-files` 让每条 TCP 连接 raw-copy 一个分片文件、零解码回放，同 key 同连接（键闭包），有状态规则也安全——多连接供给并发（C-UCP）的正确打开方式。
- **修复流结尾丢帧**：一个流结束时，剩余不足一帧的行以前被打成 `tail` 标签，引擎按标签路由时整帧丢弃（100M 丢 1.4M 行，bench 卡在超时等待）。现在结尾行沿用原流标签落帧，不再丢数据。

## [0.3.0 Unreleased]

### 引擎（对齐 wp-reactor 1.0.0）

- 对齐 `wp-reactor` v1.0.0（本地 `crates/wf-*` 路径），获得规则分片聚合与共享资源约束：
  - **`conv` 规则可跨 shard 聚合**：fixed 窗口带 `conv`（sort/top/dedup/where）的规则现在可分片，各 shard 的 close 输出经水位 barrier 汇聚后在合并批上做全局 `apply_conv`（全局 top-N / sort），再统一限流输出；EOS 排空为完整数据出口，cancel 丢弃未 seal 部分桶。
  - **跨 shard 共享限流/预算**：`max_throttle` / `max_instances` / `max_memory_bytes` 由各 shard 独立执行改为 `SharedLimits` 共享原子约束（限流为共享滑窗、`max_instances` 精确 CAS、FailRule 规则级 latch），`rule_instances` 指标跨 shard 求和。
  - 语义修复：`max_instances` 分片下精确（原 read-then-act 可超限 ≤shard_count-1）；conv 阶段超限按 `on_exceed` 分派（FailRule 正确 latch）；`shards=1` 行为不变。

### 语言（WFL）

- `on event<accu>` 窗口内累积：达到阈值后，窗口内计数与证据持续累积、不清零，后续每条满足条件的事件都触发一次输出，展示运行累计值与全部证据，直到窗口过期才整体重启（对齐 `wp-reactor` v0.4.0）。

### 示例

- `ssh_brute_force` 示例改用 `on event<accu>`：检测到爆破后（count >= 10）每次失败登录都触发，输出运行累计 count 与逐条递增的证据。

### wfgen

- `--no-wfl` 跳过整个 WFL 管线（不加载/编译规则、无 injection），生成纯背景随机事件。
- `--no-oracle` 仍编译 WFL（**保留 injection `use()` 固定值**），只跳过期望输出（不产出 `.except.*` 侧车文件）。
- 规则同目录的 `_global.wfl` 声明的 `yield preset` 自动合并进规则，`yield <target> : <preset>` 可复用公共输出字段。

### wfusion

- `[metrics] console_output = false` 可关闭控制台统计日志。

## [0.1.43]

- 版本推进，对齐 `wp-reactor` v0.1.42（含其内部引擎修复）。

## [0.1.42]

- 版本推进（alpha），对齐 `wp-reactor` 内部修复；无新增用户功能。

## [0.1.41]

### 语言（WFL）

- `on event seq { ... }` 有序序列与 `on event any { ... }` 无序共现：攻击链检测，支持步间 `within` 时间约束、`not has ... within` 否定步、`consec` 严格相邻（`skip = to_next` 延后到 L3）。

### wfusion

- 输入静默时规则实例按窗口 TTL 自动过期释放，不再残留状态。

## [0.1.40]

### wfusion

- `[logging] level` 是日志等级唯一权威，不再被 `RUST_LOG` 静默覆盖；模块提级用 `[logging].modules`。

## [0.1.39]

### wfgen

- `--out` 改为可选，与 `--send` 解耦为四种组合；两者都缺时返回清晰的参数错误。
- `--no-oracle` 重命名为 `--no-wfl`（跳过整个 WFL 管线）；`--no-oracle` 保留为向后兼容别名。

## [0.1.38]

### 语言（WFL）

- 项目级 `_global.wfl` 规则 prelude：声明公共 `yield preset`，`yield <target> : <preset>` 复用。
- 字符串 helper：`sha1_n(text, n)`、`join(...)`、`join_by(sep, ...)`。
- 规则执行 DEBUG 漏斗日志与状态机 progress 诊断（便于定位规则卡在哪步）。

### wfusion

- stream window 支持 `object` / `array` / `array/T` 输入字段，`merge()` 做浅合并富化。

## [0.1.35-alpha] — 2026-07-22

### 语言（WFL）

- `object { ... }` / `array [ ... ]` 结构化字面量、`merge()`；stream window 支持结构化输入字段。

### wfadm

- `wfadm self update`（含 `self check`），安装 warp-fusion 套件。

## [0.1.34-alpha] — 2026-07-20

### wfadm

- `self update` 安装 warp-fusion 全套二进制；manifest channel 选择修复。

## [0.1.32-alpha] — 2026-07-20

### wfadm

- `self update` 默认远端 manifest URL 按 channel 选择分支（`alpha` / `beta`），避免错误读取其他分支的 manifest。

## [0.1.31-alpha] — 2026-07-19

### wfadm

- `self update` 对齐 `wpadm`：新增 `--channel` / `--updates-base-url` / `--updates-root` / `--json` / `--yes` / `--dry-run` / `--force`；更新源改用 manifest canonical target triple，修复 macOS arm64 短名 404。

## [0.1.30-alpha] — 2026-07-19

### wfusion

- 内置 `__window_miss` 诊断窗口：未知 stream schema / 缺失 stream tag 字段可通过 monitor sink 观察。

## [0.1.29-alpha] — 2026-07-14

### wfusion

- sink 组支持 `wf_meta_disable`，用 `__wfu_*` 等 wildmatch pattern 禁用元字段输出。
- admin API reload 语义：requires-restart 变更返回 `restart_required`，不再作为 409 失败。

## [0.1.28] — 2026-07-13

### 语言（WFL）

- helper：`now()` / `now_s()` / `now_ms()` / `now_us()` / `now_ns()`、`is_blank()` / `null_if_blank()` / `default_if_blank()`、`md5()` / `sha1()` / `sha256()` / `hex()` / `stable_id()`。
- 源码感知的 WFL 解析/编译诊断：出错时输出文件、行列和源码片段。

### wfusion

- `.wfs` 用 `window.stream_tag` 作为数据分发键（替换旧 `stream`）；上游 carrier 统一为 `wp_oml_name`。

## [0.1.24] — 2026-07-09

### wfusion — Admin API 发布协议对齐 wparse（Break）

- `POST /admin/v1/reloads/model` 参数改为 `wait` / `update` / `version` / `group` / `timeout_ms` / `reason`；移除旧 `full` / `update_remote` 语义。
- 非 loopback `admin_api.bind` 必须启用 TLS；`admin_api.auth.mode` 仅接受 `bearer_token`。

## [0.1.23] — 2026-07-08

### wfusion / wfadm

- daemon 支持 `update=true` 远程更新（git fetch + 版本解析 + sync managed dirs）后热重载，失败自动回滚。
- `wfadm conf update` 委托远程更新 API。

## [0.1.22] — 2026-07-07

### wfusion / wfgen

- 修复：非 loopback admin API 允许 TLS disabled 启动。
- `wfgen --version`。

## [0.1.21] — 2026-07-06

### wfusion — 路径基准变更（Break）

- 相对路径默认相对**工作目录**，不再相对配置文件所在目录。`wfusion.toml` 中的相对路径（`schemas` / `rules` / `sinks` / `sources_dir` 等）需去掉一层 `..`；`business.d/*.toml` 的 `base` 路径同理。`--work-dir` 显式指定时优先。

## [0.1.17] — 2026-07-01

### wfusion

- admin API 在线热重载：`POST /admin/v1/reloads/model`（L1 规则热替换 / L2 增量增窗 / L3 局部重建 / L4 重启）。
- **Break**: 移除 `wfusion config` 子命令（功能迁移到 `wfadm config`）。

### wfadm

- `conf update`（远程规则源同步）、`init --repo`（从远程模板初始化项目）。

## [0.1.16] — 2026-06-28

### wfusion

- **Break**: `wfusion run` 拆为 `wfusion daemon` / `wfusion batch`，`mode` 由 CLI 显式控制，不再读取配置。
- **Break**: 移除 `wfusion rule`（与 `wfl` 二进制重复）。
- admin API HTTP server（`GET /admin/v1/runtime/status`）。

### wfadm

- `check`（WFL/WFS/WFG 深度校验）、`conf diff`、`engine status` / `engine reload`、`self-update`。

## [0.1.11] — 2026-06-21

### wfgen

- 通过 `wp-core-connectors` 的 TcpArrowSink 发送数据（RFC6587 framing，匹配 wfusion `tcp_src`）。
