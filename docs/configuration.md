# 配置参考 (`wfusion.toml`)

## 基础

```toml
mode = "batch"   # batch（批处理） | daemon（常驻服务）

[runtime]
executor_parallelism = 2    # 规则执行并行度
rule_exec_timeout = "30s"   # 单条规则最大执行时间
schemas = "schemas/*.wfs"   # schema 文件 glob
rules   = "rules/*.wfl"     # 规则文件 glob
sinks   = "sinks"            # sink 配置目录路径
```

## 数据源 `[[sources]]`

### TCP（实时）

```toml
[[sources]]
type = "tcp"
name = "netflow_input"   # 可选，默认 tcp_1
listen = "0.0.0.0:9800"
enabled = true           # 默认 true
```

每帧带 tag（stream name）+ Arrow RecordBatch，格式 `wp_arrow` IPC。

### 文件（批处理 / 回放）

```toml
[[sources]]
type = "file"
name = "events_source"     # 可选，默认 file_1
path = "data/events.ndjson"
stream = "netflow"          # 匹配 schema 中的 window stream
format = "ndjson"           # ndjson | csv | arrow-ipc | arrow-framed
enabled = true
```

支持的 `format`：

| 值 | 说明 |
|----|------|
| `ndjson` | 一行一个 JSON 对象，字段须匹配 schema |
| `csv` | 逗号分隔，首行为 header |
| `arrow-ipc` | 标准 Arrow IPC File |
| `arrow-framed` | `wp_arrow` IPC 帧格式 |

## 窗口

### 全局默认 `[window_defaults]`

```toml
[window_defaults]
evict_interval = "30s"       # 驱逐检查间隔
max_window_bytes = "64MB"    # 单窗口最大内存
max_total_bytes = "256MB"    # 全局窗口总内存上限
evict_policy = "time_first"  # time_first | lru
watermark = "1s"             # 水位线延迟
allowed_lateness = "0s"      # 允许迟到时间
late_policy = "drop"         # drop | revise | side_output
```

### 单窗口覆盖 `[window.<name>]`

```toml
[window.conn_events]
mode = "local"                # local | replicated | partitioned
max_window_bytes = "64MB"
over_cap = "30m"              # 必须 >= schema 的 over 值
evict_policy = "time_first"
watermark = "1s"
allowed_lateness = "0s"
late_policy = "drop"
# table = "scanner_whitelist"  # Provider 窗口关联 knowdb 表
```

## 监控 `[metrics]`

```toml
[metrics]
enabled = true
report_interval = "10s"       # 指标采集 & 输出周期
```

指标通过 monitor sink 输出，配置 `sinks/infra.d/monitor.toml`：

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

## 日志 `[logging]`

```toml
[logging]
level = "info"     # trace | debug | info | warn | error
format = "plain"   # plain | json
file = "wfusion.log"
```

## Sink

```toml
sinks = "sinks"    # sink 配置目录，结构如下：
```

```
sinks/
├── infra.d/           # 基础设施 sink
│   ├── default.toml   #   兜底路由（windows = ["*"]）
│   ├── error.toml     #   错误兜底
│   └── monitor.toml   #   监控指标输出
├── business.d/        # 业务路由 sink（按 window 匹配）
│   └── alerts.toml
├── connectors/        # connector 定义
│   └── sink.d/
│       ├── 01-file.toml
│       └── 02-tcp.toml
└── defaults.toml      # 全局 sink 默认值
```

### 业务路由

```toml
# sinks/business.d/alerts.toml
[sink_group]
name = "alerts"
windows = ["network_alerts", "security_alerts"]   # 匹配的 window 名

[[sink_group.sinks]]
connect = "file_json"            # 引用 connectors/sink.d/ 中定义的 connector
name = "alerts_out"
[sink_group.sinks.params]
file = "alerts.ndjson"
```

### 基础设施

```toml
# sinks/infra.d/default.toml —— 兜底（所有未匹配业务路由的 window）
[sink_group]
name = "default_infra"
windows = ["*"]

[[sink_group.sinks]]
connect = "file_json"
name = "default_out"
[sink_group.sinks.params]
file = "default.ndjson"
```

```toml
# sinks/infra.d/error.toml —— 发送失败时的兜底
[sink_group]
name = "error_infra"

[[sink_group.sinks]]
connect = "file_json"
name = "error_out"
[sink_group.sinks.params]
file = "error.ndjson"
```

### Connector 定义

```toml
# sinks/connectors/sink.d/01-file.toml
[connector]
name = "file_json"
kind = "file"

[connector.params]
format = "ndjson"
```
