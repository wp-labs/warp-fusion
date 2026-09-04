# 规则内存上限怎么定（`limits.max_memory` / `max_instances`）

> 一句话：**先厘清"上限管的是哪块内存"，再用引擎导出的实测值校准（`wfl limits-est`），不要拍脑袋**。
> 适用：写 `.wfl` 的 `limits { max_memory = …; max_instances = …; on_exceed = … }` 却不知填多少的人。
> 配套：[ai_native_rule_dev_loop.md](ai_native_rule_dev_loop.md)（AI 闭环）、wf-examples/performance/nexmark_pk（bench/diag/verify 工具链）。

## 1. 引擎三类内存，上限各管各的

| 类别 | 谁持有 | 预算旋钮 |
|---|---|---|
| 窗口事件存储 | window actor（每窗） | `windows.toml`：`max_window_bytes` / `max_total_bytes` / `evict_policy` |
| **规则实例状态（本文）** | rule task（每规则，跨 shard 求和） | `.wfl`：`limits.max_memory` / `max_instances` |
| 输出链/在途 | fanout 通道 + sink mailbox | `window_buffer_bytes`（入流背压）、`fanout_capacity_*`、`mailbox_*` |

**进程 RSS ≠ 三者之和**（还含解码缓冲、瞬时积压、Arrow 帧共享）——RSS 只答"整机够不够"，答不了"这条规则设多少"。

## 2. `limits` 语义

```wfl
limits {
    max_memory   = "512MB"    // 实例状态总驻留上限（估算，跨分片）
    max_instances = 10000     // 存活实例数上限（精确，跨分片）
    on_exceed    = throttle   // throttle | drop_oldest | fail_rule
}
```

- `max_instances` **精确**（CAS 预留，分片不突破）；`max_memory` 是**估算**（分片并发瞬时超 ≤ shard−1 实例，recalibrate 周期校正）。
- 量级（定性）：**纯计数/聚合**单实例 ≈ 0.2–0.6KB **恒定**，不随窗内事件数；**证据/集合累积型**（collect_set / distinct / 字段历史）单实例随窗内该 key 被接受事件数线性涨，每保留值 ≈ 30–100B。
- `on_exceed`：`throttle` 只拒收新 key、已有 key 继续出结果；`drop_oldest` 逐本 shard 最老实例；`fail_rule` 硬停整条规则。
- **会计只在配了 `max_memory` 时发生**（`tracks_memory_bytes` 门控，否则逐事件浪费）→ `rule.memory_bytes` 对没配的规则恒 0。校准因此是"**先设宽跑 → 读实测 → 再收紧**"。

## 3. 观测（metrics.ndjson，`stage=rule,label=<规则>`）

| 指标 | 含义 | 何时非零 |
|---|---|---|
| `rule.instances` | 存活实例数（跨 shard 和） | 有键控实例 |
| `rule.memory_bytes` | 实例状态估算字节（2026-09-04 起导出） | **配了 `max_memory`** |
| `rule.stats_over_limit_total` | stats 执行器拒收新键累计 | stats 族规则 |

`memory_bytes` 只覆盖 **match/CEP 实例**；stats 族规则（如 q15–q18/q22 类）走自己的 guard/计数，恒 0。`wfl limits-est` 会**自动识别 stats 族**并明示成因（见 §4 输出 / §6），不用手工判形态。

快速联调（本地 path）：

```bash
cd warp-fusion && cargo build --release -p wfusion     # path 指向本地 wp-reactor
cd wf-examples/performance/nexmark_pk && ./bench.sh q12 replay 30m
wfl limits-est data/metrics.ndjson --rule q12_bidder_10s_window_count --headroom 2
```

## 4. 定值流程

**量级公式**：`max_memory ≈ K × (base + E × 每证据字节)`
K = 窗内最大并发 key 数（读 `rule.instances` 峰值）；E = 单 key 窗内保留上界（= 事件率 × `over` 窗口长，有界）。

1. **设宽**：按量级上界起步（如 RSS 预算的 1/4，或 1–2GB），`on_exceed=throttle`
2. **跑最坏负载**：灌到稳态 + 高基数/攻击形态（单次峰值不准，跑两轮取 max）
3. **读实测**：`wfl limits-est data/metrics.ndjson`（全零时自动提示成因）
4. **收紧**：`max_memory = 内存峰值 × (1.5–3)`；`max_instances = 实例峰值 × (2–3)`

实测样例（q12 30M）：

```text
rule                          inst_peak     mem_peak    B/inst  suggest_mem suggest_inst
q12_bidder_10s_window_count        2841        8.4MB      3114      17.0MB       5682

推导（headroom=2.0）:
  q12_bidder_10s_window_count: max_instances = round(2841 × 2.0) = 5682
          max_memory = 8,848,081B × 2.0 = 17,696,162B → 向上取整到 MiB → 17,825,792B（17.0MB）
```

每条建议都可由"峰值 × headroom + 档位取整"**复核**；`--format json`（schema `wfl-limits-est/v1`，含 `memory_granularity` 与原始字节）供 AI/CI 消费。

**上限是保险丝不是工作带**：日常负载应远低于上限；定太低撞限即**静默丢新 key**（`[clean]` 照常，最危险），定太高失去防 OOM 意义。

### 最坏负载 = 窗内并发 × 单 key 累积，不是总 N

| 规则形态 | 单实例内存 | 随总 N 缩放 | 校准负载 |
|---|---|---|---|
| **窗口并发有界型**（match 纯计数/聚合，无逐 key 证据/历史，如 q12/q4） | 恒定，随实例数走 | 基本不随——峰值 = 窗内 key 密度（由事件速率决定） | 最密短窗即可，不必跑满全量 N |
| **逐 key 累积型**（collect_set / distinct / 证据保留 / 字段历史） | 随窗内该 key 接受事件数线性涨 | 仅当总 N 抬高窗内密度才涨 | 按窗内洪峰密度灌（如攻击形态） |

q12 实测佐证（30M→100M，N 涨 3.3×）：实例峰值 2,841→2,621、内存 8.4MB→7.5MB，EMIT 精确 ×3.336 零丢——实例内存**不随总 N**，前提是到达**速率**不变；RSS 涨（3→4.4GB）来自输出链/窗口簿记，非规则状态。若窗内密度随 N 放大（攻击变密），才需按最大 N 兜底跑（`./bench.sh q12 replay 100m` → `wfl limits-est …`）。

## 5. 校验纪律（防静默漏检）

1. `throttle` 超限是**静默的**（不报错、`[clean]` 照常）——EMIT 变少极易误判"引擎对、基准错"。
2. 收紧后跑一次**正确性对拍**（`verify_daemon.sh` / oracle），确认 EMIT 没因超限丢失。
3. 归因分工：`bench.sh` = 全生命周期 RSS/CPU；`diag.sh`（`MEMORY=1`）= 内存涨在哪一段；`rule.memory_bytes` = 某条规则实例状态用了多少。

## 6. 边界

- `max_memory` 是实例状态**估算/tripwire**（超限触发动作），非精确计费、非进程真实驻留。
- stats 族规则不在 `memory_bytes` 记账内（§3）；`limits-est` 检测到 `stats_over_limit_total` 采样即明确拒绝给建议，指向拒收计数与全局预算。
- 需引擎 ≥ 2026-09-04 导出（wp-reactor `28d6f8f`，尚未发布 tag）——联调走本地 path，生产等发布后 warp-fusion bump。
