# 监控配置 (`[metrics]`)

```toml
[metrics]
enabled = true
report_interval = "10s"       # 指标采集 & 输出周期
```

| 字段 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `enabled` | bool | `false` | 是否启用指标采集 |
| `report_interval` | duration | `"2s"` | 采集周期 |

## 指标输出

指标通过 monitor sink 输出，需配置 `sinks/infra.d/monitor.toml`：

```toml
[sink_group]
name = "monitor_infra"
windows = ["*"]

[[sink_group.sinks]]
connect = "file_json"
name = "monitor_out"
[sink_group.sinks.params]
file = "metrics.ndjson"
```

指标以 NDJSON 格式输出，每条为一行 JSON：

```json
{"stage":"match","name":"rule_hits","label":"port_scan","value":"12345"}
{"stage":"window","name":"memory_bytes","label":"conn_events","value":"1048576"}
{"stage":"rule","name":"match_latency_p50","label":"port_scan","value":"0.512"}
```

## 指标列表

更多运行和排障场景见 [Admin API / CLI 文档](../cli/admin_api.md) 和 [运行时配置](runtime.md)。

| 类别 | 指标（部分） |
|------|-------------|
| Receiver | `connections_total`, `frames_total`, `rows_total`, `decode_errors_total` |
| Router | `route_calls_total`, `delivered_total`, `dropped_late_total` |
| Rule | `events_total`, `matches_total`, `instances` |
| Alert | `emitted_total`, `channel_depth`, `dispatch_total` |
| Evictor | `sweeps_total`, `time_evicted_total` |
| Window | `memory_bytes`, `rows`, `batches`, `append_total`, `evict_total` |
| Latency | `decode_seconds_p50/p99`, `dispatch_seconds_p50/p99`, `e2e_latency_seconds_p50/p99` |
