# WarpFusion 数据监控方案

## 架构设计

参考 [wp-motor](https://github.com/wp-labs/wp-motor) 的 monitor sink 模式：**指标即数据**，走通用 Sink 管线，不做专用输出通道。

```
                              ┌──────────┐
Receiver ──► Router ──► Window │ 指标采集  │
                              └────┬─────┘
                                   │
                         AtomicU64 inc/observe
                            （热路径，零改动）
                                   │
                    run_metrics_task() 定时循环
                         report_interval
                                   │
                          snapshot() drain
                                   │
                        to_data_records()
                          → Vec<Record>
                                   │
                          MonSend channel
                                   │
                    ┌──────────────┴──────────────┐
                    │      Monitor Sink Group     │
                    │  (sinks/infra.d/monitor/)   │
                    └──────────────┬──────────────┘
                                   │
                        SinkDispatcher (fan-out)
                                   │
                    ┌──────────────┼──────────────┐
                    ▼              ▼              ▼
               file/json      prometheus      arrow-ipc
               (本地文件)     (VM/Prom)       (实时大盘)
```

### 核心理念

| | 当前 | 目标 |
|---|---|---|
| 指标输出 | 手写 Prometheus TCP + 日志 | 走通用 Sink 管线 |
| 加接收端 | 改代码 | 改 TOML 配置 |
| 输出格式 | 仅 Prometheus text | 任意 sink backend |
| 数据流 | 指标专用通道 | 与告警走同一套框架 |

### 为什么不用独立的 Prometheus 端点

独立端点的问题是"写死"——只能出 Prometheus 格式，只能被 scrape。走 Sink 管线后：

```toml
# 同一份指标数据，配置决定去哪
[sink_group]
name = "monitor"

# 接收端 1：本地文件（JSON 格式，方便调试）
[[sink_group.sinks]]
connect = "file_json_sink"
params.file = "out/metrics.ndjson"

# 接收端 2：VictoriaMetrics（Prometheus 兼容）
[[sink_group.sinks]]
connect = "prometheus_sink"
params.endpoint = "http://vm:8428/write"

# 接收端 3：Arrow IPC → 实时大盘
[[sink_group.sinks]]
connect = "arrow_ipc_sink"
params.addr = "dashboard:9800"

# 接收端 4：Kafka → 离线分析
[[sink_group.sinks]]
connect = "kafka_sink"
params.topic = "wf_metrics"
```

改一行配置就多一个接收端，不需要改代码。

---

## 现状

内置了一套 hand-rolled 指标系统（`wf-runtime/src/metrics.rs`，1103 行），已覆盖 40+ 指标：

### 已有指标

**Receiver（接入层）**
| 指标 | 类型 | 说明 |
|------|------|------|
| `receiver_connections_total` | counter | TCP 连接数 |
| `receiver_frames_total` | counter | IPC 帧数 |
| `receiver_rows_total` | counter | 事件行数 |
| `receiver_decode_errors_total` | counter | 解码失败 |
| `receiver_read_errors_total` | counter | 读取失败 |
| `receiver_decode_seconds` | histogram | 解码延迟 |

**Router（路由层）**
| 指标 | 类型 | 说明 |
|------|------|------|
| `router_route_calls_total` | counter | 路由调用次数 |
| `router_delivered_total` | counter | 成功投递 |
| `router_dropped_late_total` | counter | 迟到丢弃 |
| `router_skipped_non_local_total` | counter | 非本地跳过 |
| `router_route_errors_total` | counter | 路由失败 |

**Rule（规则引擎）** — per-rule
| 指标 | 类型 | 说明 |
|------|------|------|
| `rule_events_total` | counter | 送入状态机的事件数 |
| `rule_matches_total` | counter | 命中次数 |
| `rule_instances` | gauge | 活跃实例数 |
| `rule_cursor_gap_total` | counter | cursor gap（驱逐导致） |
| `rule_scan_timeout_seconds` | histogram | 超时扫描耗时 |
| `rule_flush_seconds` | histogram | 关闭冲刷耗时 |

**Alert（告警）** — per-rule
| 指标 | 类型 | 说明 |
|------|------|------|
| `alert_emitted_total` | counter | 告警产出数 |
| `alert_channel_send_failed_total` | counter | 通道发送失败 |
| `alert_serialize_failed_total` | counter | 序列化失败 |
| `alert_dispatch_total` | counter | 分发到 sink 数 |
| `alert_dispatch_seconds` | histogram | 分发延迟 |

**Evictor（驱逐）**
| 指标 | 类型 | 说明 |
|------|------|------|
| `evictor_sweeps_total` | counter | 驱逐周期数 |
| `evictor_time_evicted_total` | counter | 时间驱逐数 |
| `evictor_memory_evicted_total` | counter | 内存驱逐数 |

**Window（窗口）** — per-window
| 指标 | 类型 | 说明 |
|------|------|------|
| `window_memory_bytes` | gauge | 内存占用 |
| `window_rows` | gauge | 存储行数 |
| `window_batches` | gauge | batch 数量 |

---

## 缺口分析

```
Receiver ──► Router ──► Window ──► StateMachine ──► Alert ──► Sink
   ✅          ✅         ⚠️           ❌            ⚠️        ✅
  解码/帧    路由/丢弃  仅总量      无延迟观测    仅总量    分发/耗时
```

### 缺口 1：延迟黑洞

事件从进入窗口到告警产出，中间的链路完全不可见：

| 指标 | 说明 |
|------|------|
| `wf_window_append_seconds` | 事件追加到窗口 buffer 耗时 |
| `wf_sm_advance_seconds` | 状态机 advance（NFA 步进 + join）耗时 |
| `wf_sm_match_seconds` | 命中后 execute_match（entity/yield/conv）耗时 |
| `wf_event_e2e_latency_seconds` | 事件时间戳 → 告警产出，端到端延迟 |

### 缺口 2：通道背压

Rule task → Alert task 之间是 `mpsc::channel(64)`，只有失败计数，不知道当前队列堆积了多少：

| 指标 | 类型 | 说明 |
|------|------|------|
| `wf_alert_channel_depth` | gauge | 当前队列深度 |
| `wf_alert_channel_full_total` | counter | 队列满次数 |

### 缺口 3：窗口流入/流出

Per-window 只有 gauges（总量），没有速率：

| 指标 | 类型 | 说明 |
|------|------|------|
| `wf_window_append_total` | counter (per-window) | 追加事件数 |
| `wf_window_evict_total` | counter (per-window) | 驱逐事件数 |
| `wf_window_late_total` | counter (per-window) | 迟到丢弃数 |

### 缺口 4：数据质量

| 指标 | 类型 | 说明 |
|------|------|------|
| `wf_schema_mismatch_total` | counter | schema 字段不匹配 |
| `wf_null_field_total` | counter | 必填字段为 null |
| `wf_event_order_violation_total` | counter | 乱序事件 |

### 缺口 5：TopN（config 已有，实现缺失）

`MetricsTopNConfig` 在 config 中已定义，但无实现。指标 key（规则名、窗口名）在启动时确定，直接排序截断即可，不需要 LRU。

---

## Monitor Sink 设计

### 为什么比 wp-stats 更简单

warp-fusion 的指标是**预聚合**的——key 数量在启动时就确定（规则名来自 WFL，窗口名来自 WFS）。不存在 wp-motor 那种 "每条规则 × 每个源 IP" 的维度爆炸问题，所以不需要 LRU。

| | wp-stats | warp-fusion |
|---|---|---|
| 指标 key | 每条 event × 多维（维度无界） | per-rule / per-window（数量固定） |
| 内存控制 | LRU 淘汰冷 key | 不需要——key 不会增长 |
| 复杂度 | StatDim / StatTarget / StatReq | 就是 AtomicU64 |
| 采集接口 | record_begin/end（per-event） | inc / observe（per-aggregation） |

### 管道：snapshot → DataRecord → Monitor Sink

就三步，~200 行新增代码：

```
RuntimeMetrics (已有，保留)
  │  AtomicU64 inc/observe（热路径，不变）
  ▼
snapshot() → MetricsSnapshot
  │  drain counters，重置为 0
  ▼
to_data_records() → Vec<DataRecord>
  │  {
  │    stage: "match", name: "rule_hits", rule: "port_scan", value: 12345,
  │    stage: "window", name: "memory_bytes", window: "conn_events", value: 1048576,
  │    stage: "rule", name: "match_latency_p50", rule: "port_scan", value: 0.5,
  │    ...
  │  }
  ▼
MonSend channel → Monitor Sink Group
  │
  ▼
┌─────────────────────────────────────────┐
│  SinkDispatcher (已有，fan-out)          │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ │
│  │ file/json│ │ prom_push│ │ arrow_ipc│ │
│  │ 本地文件  │ │  VM/Prom │ │ 实时大盘  │ │
│  └──────────┘ └──────────┘ └──────────┘ │
└─────────────────────────────────────────┘
```

### MetricsRecord

warp-fusion 没有 `DataRecord` 类型。设计一个极简的 key-value map 作为指标传输格式，sink backend 负责序列化（NDJSON 直接 dump，Prometheus 按 label 拼）。

```rust
/// 单条指标记录 —— 就是一组 key-value 对
pub struct MetricsRecord {
    pub fields: Vec<(String, String)>,  // 保持插入顺序
}
```

```rust
// 核心新增代码
impl RuntimeMetrics {
    /// report_interval 触发时调用：drain 所有计数器，产出 MetricsSnapshot
    fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            counters: self.drain_counters(),     // swap(0) AtomicU64
            gauges:   self.read_gauges(),        // 不重置，只读当前值
            histograms: self.drain_histograms(), // swap(empty)，保留原始 bucket counts
        }
    }
}

impl MetricsSnapshot {
    /// 转为 Vec<MetricsRecord>，每条指标一行
    fn to_records(&self) -> Vec<MetricsRecord> {
        let mut records = Vec::new();

        // Counter: per-rule 命中
        for (rule, count) in &self.counters.rule_matches {
            records.push(MetricsRecord { fields: vec![
                ("stage".into(), "match".into()),
                ("name".into(),  "rule_hits".into()),
                ("rule".into(),  rule.clone()),
                ("value".into(), count.to_string()),
            ]});
        }
        // Gauge: per-window 内存
        for (window, bytes) in &self.gauges.window_memory {
            records.push(MetricsRecord { fields: vec![
                ("stage".into(), "window".into()),
                ("name".into(),  "memory_bytes".into()),
                ("window".into(), window.clone()),
                ("value".into(), bytes.to_string()),
            ]});
        }
        // Histogram: 在 snapshot 时计算分位数（在线 O(n) 算法），输出 p50/p99
        for (rule, hist) in &self.histograms.rule_match_latency {
            let p50 = hist.percentile(0.50);
            let p99 = hist.percentile(0.99);
            records.push(MetricsRecord { fields: vec![
                ("stage".into(), "rule".into()),
                ("name".into(),  "match_latency_p50".into()),
                ("rule".into(),  rule.clone()),
                ("value".into(), format!("{:.3}", p50)),
            ]});
            records.push(MetricsRecord { fields: vec![
                ("stage".into(), "rule".into()),
                ("name".into(),  "match_latency_p99".into()),
                ("rule".into(),  rule.clone()),
                ("value".into(), format!("{:.3}", p99)),
            ]});
        }
        records
    }
}
```

### MetricsRecord 序列化示例

```
// Counter 型 → NDJSON
{"stage":"match","name":"rule_hits","rule":"port_scan","value":"12345"}
{"stage":"route","name":"delivered","value":"67890"}

// Gauge 型 → NDJSON
{"stage":"window","name":"memory_bytes","window":"conn_events","value":"1048576"}
{"stage":"window","name":"rows","window":"conn_events","value":"5000"}

// Histogram 型 → NDJSON（snapshot 时在线计算分位数）
{"stage":"rule","name":"match_latency_p50","rule":"port_scan","value":"0.512"}
{"stage":"rule","name":"match_latency_p99","rule":"port_scan","value":"5.237"}
```

> **Histogram 分位数计算**：使用绿色版在线算法（P² algorithm），O(1) 空间、O(1) 插入。snapshot 时直接读取，不需要存储全量 bucket counts 到 MetricsRecord。

### TOML 配置

warp-fusion 的 sink 配置走文件目录模式（`sinks = "sinks"`），不是内联 TOML：

```toml
# wfusion.toml — 主配置
[metrics]
enabled = true
report_interval = "10s"        # snapshot 周期

sinks = "sinks"                # sink 配置目录
```

```toml
# sinks/infra.d/monitor.toml — monitor sink group 配置
[sink_group]
name = "monitor"
batch_size = 1                 # 实时发送，不攒批

# 接收端 1：本地 NDJSON 文件
[[sink_group.sinks]]
name = "json_file"
kind = "file"
[sink_group.sinks.params]
file = "out/metrics.ndjson"
format = "ndjson"

# 接收端 2：Prometheus remote write
[[sink_group.sinks]]
name = "prom_push"
kind = "prometheus"
[sink_group.sinks.params]
endpoint = "http://vm:8428/api/v1/write"
```

### snapshot 驱动循环

保留 `run_metrics_task()` 的 tokio interval 循环，改动输出目标：

```rust
// 改造前（metrics.rs）
async fn run_metrics_task(...) {
    let mut interval = tokio::time::interval(report_interval);
    loop {
        interval.tick().await;
        let snapshot = metrics.snapshot();
        log_metrics_table(&snapshot);             // ← 删除
        render_prometheus_text(&snapshot);        // ← 删除
        // 新增：
        let records = snapshot.to_records();
        let _ = mon_send.send(records).await;
    }
}
```

### 与现有代码的整合

1. **保留** `RuntimeMetrics` 的 AtomicU64 结构（零改动，热路径不变）
2. **新增** `MetricsRecord` 类型（~20 行）
3. **新增** `snapshot()` + `MetricsSnapshot::to_records()`（~100 行）
4. **新增** `MonSend` channel（`mpsc::Sender<Vec<MetricsRecord>>`）→ 连接 metrics_task → monitor sink
5. **新增** `sinks/infra.d/monitor.toml` → monitor sink group，复用 `SinkDispatcher` → backend 管线
6. **改造** `run_metrics_task()` → 输出从 log/Prometheus 改为 `mon_send.send()`
7. **移除** 手写 Prometheus TCP server（`metrics.rs` 中的 `serve_prometheus()` 和 `render_prometheus()`, ~150 行） + 日志表格渲染（~50 行）

---

## 实施计划

### P0（必须）

| 项目 | 说明 | 工作量 |
|------|------|--------|
| snapshot() + to_data_records() | drain AtomicU64 → DataRecord | 0.5d |
| MonSend channel + monitor sink group | 指标进入 sink 管线 | 1d |
| 通道背压 gauge | `metrics.rs` + `alert_task.rs` | 0.5d |
| 端到端延迟 histogram | `metrics.rs` + `rule_task.rs` | 0.5d |

### P1（重要）

| 项目 | 说明 | 工作量 |
|------|------|--------|
| 窗口流入/流出 counter | per-window append/evict/late | 1d |
| file_json_sink connector | 输出 NDJSON 格式 | 0.5d |
| prometheus_sink connector | VictoriaMetrics / Prometheus | 1d |
| 移除手写 TCP server | 替换为 monitor sink | 0.5d |

### P2（增强）

| 项目 | 说明 | 工作量 |
|------|------|--------|
| 分段延迟（append/advance/match） | AtomicU64 inc 包裹 | 1d |
| 数据质量指标 | schema mismatch / null field | 1d |
| TopN（per-rule/window 排序截断） | 规则/窗口数量固定，sort 即可 | 0.5d |

## 告警规则建议

```yaml
- alert: HighLateDropRate
  expr: rate(router_dropped_late_total[5m]) / rate(receiver_rows_total[5m]) > 0.05
  annotations: summary: "迟到丢弃率 > 5%，检查 watermark"

- alert: WindowMemoryNearLimit
  expr: window_memory_bytes / window_max_bytes > 0.85
  annotations: summary: "窗口内存接近上限"

- alert: AlertChannelBackpressure
  expr: rate(alert_channel_full_total[5m]) > 0
  annotations: summary: "告警通道满，可能丢失告警"

- alert: HighDecodeErrorRate
  expr: rate(receiver_decode_errors_total[5m]) / rate(receiver_frames_total[5m]) > 0.01
  annotations: summary: "解码错误率 > 1%，检查 schema 兼容性"

- alert: NoEventsIngested
  expr: rate(receiver_rows_total[5m]) == 0
  for: 5m
  annotations: summary: "5 分钟内无事件摄入"
```
