# 规则内存上限怎么定（`limits.max_memory` / `max_instances`）

> 一句话：**先厘清"上限管的是哪块内存"，再用引擎导出的实测值校准，不要拍脑袋**。
> 适用对象：规则开发者/运维在 `.wfl` 里写 `limits { max_memory = …; max_instances = …; on_exceed = … }`
> 却不知道填多少的人（nexmark_pk/qradar 调优同口径）。
>
> 配套：AI 原生闭环能力说明见 [ai_native_rule_dev_loop.md](ai_native_rule_dev_loop.md)；
> 契约/性能验证工具链见同仓 wf-examples/performance/nexmark_pk（bench/diag/verify_daemon）。

## 1. 先分清引擎里的三类内存（上限各管各的）

| 类别 | 谁持有 | 预算旋钮 | 上限语义 |
|---|---|---|---|
| **窗口事件存储** | window actor（每窗） | `windows.toml`：`max_window_bytes` / `max_total_bytes` / `evict_policy` | 事件原始内容驻留；超预算逐出（`window.evict_total` 计数） |
| **规则实例状态** | rule task（每规则，跨 shard 求和） | **`.wfl` `limits.max_memory` / `max_instances`** | 每条规则的 per-key 匹配状态机（部分匹配/计数/证据/字段历史） |
| **输出链/在途** | fanout 通道 + sink mailbox | `window_buffer_bytes`（入流背压）、`window.fanout_capacity_*`、`mailbox_*` | 批级在途字节；阻塞向上游传播 |

注意：**进程 RSS ≠ 三者之和**——RSS 还含解码缓冲、瞬时积压、Arrow 帧共享等。
RSS 只能回答"整机够不够"，不能回答"这条规则的 limits 该设多少"。**本文只讲第二类**。

## 2. `limits` 语义（引擎实现为准）

```wfl
limits {
    max_memory   = "512MB"   // 规则实例状态总驻留上限（估算，跨分片）
    max_instances = 10000    // 存活实例数上限（精确，跨分片）
    on_exceed    = throttle  // throttle | drop_oldest | fail_rule
}
```

| 字段 | 口径 | 精确性 |
|---|---|---|
| `max_instances` | 全规则（跨 shard）**存活实例数**（每个 key/窗口桶一个实例） | **精确**（CAS 预留，分片不会突破） |
| `max_memory` | 全规则存活实例的**估算字节和** | 近似（分片并发瞬时超 ≤shard−1 个实例；recalibrate 周期校正精确值） |

`max_memory` 估算法（`Instance::estimated_bytes`）的构成（量级参考）：

```
实例 ≈ 160B 固定（结构 128 + key 32）
    + 每 step/close 分支状态 80B（空分支也算）
    + distinct/collect/证据：每保留值 ≈ 字符串 len+24（数字/布尔 8B）
    + 字段历史：仅规则需要时保留（close/多 bind/join/L3 series），每字段每事件值 ≈ len+24+值
    + completed_steps ×64、alias 状态、baselines ×128
```

由此得出两条**定性**结论：

- **纯计数规则**（guard + count/sum/min/max/avg，无 distinct/历史）：单实例 ≈ **0.2–0.6KB 恒定**，与窗口内事件数无关——`max_memory` 基本只随实例数走。
- **证据/集合累积型**（`collect_set`、`distinct`、accu 保留证据、字段历史）：单实例随**窗口内该 key 被接受事件数**线性增长，每证据 ≈ **30–100B**。

> `on_exceed=throttle` 超限**只拒收新 key**、已有 key 继续出结果；`drop_oldest` 逐出本
> shard 最老实例（跨 shard 公平性是每 shard 局部）；`fail_rule` 硬停整条规则。

### 会计只在"配了上限"时发生

引擎按 `tracks_memory_bytes` 门控——**只有规则写了 `max_memory` 才维护内存估算**（否则纯
逐事件浪费）。含义：`rule.memory_bytes` gauge 对没写 `limits` 的规则恒 0。校准流程因此是
**"先设宽上限跑负载 → 读实测 → 再收紧"**，不是先量后写。

## 3. 观测：metrics.ndjson 里读什么

| 指标 | 含义 | 何时非零 |
|---|---|---|
| `rule.instances`（`stage=rule,label=<规则>`） | 存活实例数（跨 shard 和） | 有键控实例的规则 |
| **`rule.memory_bytes`**（`stage=rule,label=<规则>`） | 实例状态估算字节（2026-09-04 起导出） | **规则配了 `max_memory`** |
| `window.memory_bytes` / `allocated_bytes` / `rows` / `evict_total` | 窗口存储侧账目 | 任何被窗口消费的流 |
| `rule.stats_over_limit_total` | stats 执行器拒收新键桶累计 | stats 族规则 |

边界：`rule.memory_bytes` 反映的是 **match/CEP 实例**内存。走 **stats 执行器**的规则
（如 nexmark q15–q18 类统计规则）有自己的 guard 与计数（`rule_stats_over_limit_total`），
当前**不在**这条 gauge 里——判读时先确认规则形态（`match<…>` 键控 vs stats 族）。

### 快速联调（本地 path 模式）

```bash
cd warp-fusion && cargo build --release -p wfusion      # path 依赖已指向本地 wp-reactor
cd wf-examples/performance/nexmark_pk
./bench.sh q12 replay 30m
python3 - <<'EOF'
import json
peak=0
for line in open('data/metrics.ndjson'):
    r=json.loads(line)
    if r.get('stage')=='rule' and r.get('name')=='memory_bytes':
        peak=max(peak,int(r.get('value','0')))
print(f"rule.memory_bytes peak = {peak/1048576:.1f} MB")
EOF
```

实测样例（q12 `match<bidder:10s:fixed>`，30M，`max_memory=512MB`）：
`rule.instances` 峰值 **2,841**、`rule.memory_bytes` 峰值 **9.1MB**（≈3.2KB/实例）→
512MB 上限只用了 1.8%——**"拍脑袋的 512MB"是拍大了**。

## 4. 定值方法（推荐流程）

**公式**：`max_memory ≈ K × (base + E × 每证据字节)`
K = 窗口跨度内预期最大并发 key 数（读 `rule.instances` 峰值校准），
E = 单 key 窗口内保留事件上界（= 事件率 × `over` 窗口长，有界）。

**流程**：

1. **设宽**：先按量级上界写大值（如按 RSS 预算的 1/4，或 1–2GB 起步），`on_exceed=throttle`；
2. **跑最坏负载**：用目标场景/benchmark 灌到稳态与高基数/攻击形态（单次峰值不准，跑两轮取 max）；
3. **读实测**：metrics.ndjson 的 `rule.instances` + `rule.memory_bytes` 峰值；
4. **收紧为保险丝**：`max_memory = 峰值 × (1.5–3×)`，`max_instances = 实例峰值 × (2–3×)`。

第 3–4 步有现成指令（wfl，读一次 bench/diag 产出的 metrics.ndjson）：

```bash
wfl limits-est data/metrics.ndjson --rule q12_bidder_10s_window_count --headroom 2
# rule                            inst峰值      mem峰值       B/实例  建议 max_memory = 峰值×2.0   建议 max_instances
# q12_bidder_10s_window_count       2841      8.4MB       3114  17.0MB    (上限 2.0×)        5682 (上限 2.0×)
# --format json → wfl-limits-est/v1（AI/CI 可消费）
```

命令逐条说明：`rule.memory_bytes` 全 0 时提示成因（未配 max_memory 引擎不记账 / stats 族规则），
建议值向上取整到整 MiB/KiB；headroom 默认 2.0（文档推荐 1.5–3）。

**上限是"保险丝"不是"工作带"**：日常工作负载应远低于上限；撞上限意味着"负载超出设计"
开始丢新 key/逐出——**定太低会静默漏检**（metrics `[clean]` 照常，这是性能验证里最危险的
坑），定太高失去防 OOM 意义。

## 5. 校验纪律（防静默漏检）

1. `on_exceed=throttle` 下超限行为是**静默的**（不报错、`[clean]` 照常）——EMIT 变少极易
   误判成"引擎对、基准错"。
2. 收紧上限后必须跑一次**正确性对拍**：`verify_daemon.sh`（nexmark）或 oracle 对拍，
   确认 EMIT 计数没因超限丢失（nexmark README 明示该坑）。
3. RSS/内存归因分析用 `bench.sh`（全生命周期采样）；`diag.sh` 墙梯回答"内存涨在哪一段"
   用 `MEMORY=1` 模式；`rule.memory_bytes` 回答"某条规则的实例状态用了多少"。

## 6. 边界（诚实声明）

- `max_memory` 是**实例状态估算**，非进程真实驻留：用途是 tripwire（超限触发动作），
  不是精确计费；分片瞬时超 ≤shard−1 个实例、recalibrate 周期校正。
- stats 族规则的实例内存当前不在 `rule.memory_bytes` 内（见 §3 边界）。
- 该 gauge 需要引擎 ≥ 2026-09-04 的导出（wp-reactor `28d6f8f`，尚未发布 tag）——
  联调走本地 path，生产等发布后 warp-fusion bump。
