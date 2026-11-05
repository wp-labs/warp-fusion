# Sink 配置

## 概述

Sink 配置放在 `sinks/` 目录下，wfusion.toml 中通过 `sinks = "sinks"` 指定路径。

```
sinks/
├── infra.d/           # 基础设施 sink
│   ├── default.toml   #   兜底路由
│   ├── error.toml     #   错误兜底
│   └── monitor.toml   #   监控指标（可选）
├── business.d/        # 业务路由 sink
│   └── alerts.toml
├── connectors/        # connector 定义
│   └── sink.d/
│       └── 01-file.toml
└── defaults.toml      # 全局默认值
```

## 业务路由

规则 `yield` 的 `target_window` 匹配 `windows` 列表，命中则走该组 sink。

```toml
# sinks/business.d/alerts.toml
version = "1.0"

[sink_group]
name = "alerts"
windows = ["network_alerts", "security_alerts"]

[[sink_group.sinks]]
connect = "file_json"
name = "alerts_out"
[sink_group.sinks.params]
file = "alerts.ndjson"
```

| 字段 | 说明 |
|------|------|
| `windows` | 匹配的 window 名列表，`["*"]` 匹配所有 |
| `connect` | 引用 `connectors/sink.d/` 中的 connector 名 |
| `name` | sink 实例名 |
| `params` | connector 特定参数 |

## 文件 sink 滚动与归档

`kind = "file"` 的 sink 可以按容量滚动、限制历史文件个数，并把历史文件压缩归档。
仅支持文本协议（`protocol = "txt"`，默认值；内置的 `file_json_sink` /
`file_proto_sink` / `file_proto_text_sink` / `file_kv_sink` / `file_raw_sink`
都属于这一类）。

```toml
# sinks/connectors/sink.d/01-file.toml
[[connectors]]
id = "file_rolling"
type = "file"
allow_override = ["base", "file", "max_size", "max_rolls", "roll_compress"]

[connectors.params]
base = "./data/out_dat"
file = "alerts.ndjson"
fmt = "json"
max_size = "10MB"
max_rolls = 3
roll_compress = "gzip"
```

| 参数 | 说明 | 默认 |
|------|------|------|
| `max_size` | 单文件容量上限；支持 `10MB` / `512k` / `1GiB` 这类写法（也接受整数字节数），`0` 表示不限制 | `0`（不滚动） |
| `max_rolls` | 保留的历史文件个数，上限 `1024`；`0` 表示滚动时直接丢弃旧文件 | `3` |
| `roll_compress` | 历史文件压缩格式：`none` / `gzip` / `zip`（`true` 等价 `gzip`） | `none` |

行为：

- 写入前检查容量：本次写入会超过 `max_size` 时，先把当前文件滚动出去，再写入新文件。
- 历史文件按新到旧编号：`alerts.ndjson.1`（最新）、`.2`、…；超出 `max_rolls` 的最旧文件自动删除。
- 开启压缩后归档名为 `alerts.ndjson.1.gz` / `alerts.ndjson.1.zip`；每个归档都是独立完整的压缩流，可单独解压。
- 滚动检查发生在每次落盘之前；如果单个写入批次本身就大于 `max_size`，该文件会先超过上限一次，下一批次写入时再滚动。
- 压缩失败不丢数据：回退为不压缩的 `alerts.ndjson.1` 归档。
- 只支持 `protocol = "txt"`；`protocol = "arrow"` 搭配滚动参数会在启动校验时报错。
- 在 `[sink_group.sinks.params]` 里覆盖这些参数时，connector 的 `allow_override`
  必须包含对应字段（内置 `file_*_sink` 定义已包含 `max_size` / `max_rolls` / `roll_compress`）。

## 禁用 wfusion 元字段输出

规则告警输出会附带 wfusion 管理的元字段，字段名前缀为 `__wfu_`，例如：

- `__wfu_rule_name`
- `__wfu_score`
- `__wfu_entity_type`
- `__wfu_entity_id`
- `__wfu_origin`
- `__wfu_close_reason`
- `__wfu_fired_at`
- `__wfu_emit_time`
- `__wfu_summary`

如果某个 sink group 不希望输出其中一部分元字段，可以在 `[sink_group]` 中配置
`wf_meta_disable`：

```toml
[sink_group]
name = "alerts"
windows = ["security_alerts"]
wf_meta_disable = ["__wfu_rule_name"]

[[sink_group.sinks]]
connect = "file_json_sink"
name = "alerts_out"
[sink_group.sinks.params]
file = "alerts.ndjson"
```

如果要禁用全部 wfusion 元字段，可以使用 `__wfu_*`：

```toml
[sink_group]
name = "alerts"
windows = ["security_alerts"]
wf_meta_disable = ["__wfu_*"]
```

`wf_meta_disable` 只允许填写 `__wfu_` 前缀的精确字段，或 `__wfu_` 前缀下的 wildmatch
pattern，例如：

```toml
wf_meta_disable = ["__wfu_rule_*", "__wfu_emit_time"]
```

`*`、`*_time` 这类非 `__wfu_` 前缀 pattern 会被拒绝。运行时不会删除业务字段；它会在发送
record 前把匹配的 wfusion 元字段标记为 `Ignore`，由 sink 编码阶段跳过输出。

如果 sink 同时配置了 `fields` 投影，执行顺序是先按 `fields` 投影，再应用
`wf_meta_disable`。因此：

- `fields` 用于选择要输出哪些字段。
- `wf_meta_disable` 用于在该 sink group 中屏蔽指定 `__wfu_*` 元字段。

## 基础设施

### Default（兜底）

未匹配任何业务路由的 window 走这里。

```toml
# sinks/infra.d/default.toml
[sink_group]
name = "default_infra"
windows = ["*"]

[[sink_group.sinks]]
connect = "file_json"
name = "default_out"
[sink_group.sinks.params]
file = "default.ndjson"
```

### Error（错误兜底）

发送失败时走这里。

```toml
# sinks/infra.d/error.toml
[sink_group]
name = "error_infra"

[[sink_group.sinks]]
connect = "file_json"
name = "error_out"
[sink_group.sinks.params]
file = "error.ndjson"
```

### Monitor（监控指标）

```toml
# sinks/infra.d/monitor.toml
[sink_group]
name = "monitor_infra"
windows = ["*"]

[[sink_group.sinks]]
connect = "file_json"
name = "monitor_out"
[sink_group.sinks.params]
file = "metrics.ndjson"
```

## Connector 定义

```toml
# sinks/connectors/sink.d/01-file.toml
version = "1.0"

[connector]
name = "file_json"
kind = "file"

[connector.params]
format = "ndjson"
```

| `kind` | 说明 |
|--------|------|
| `file` | 本地文件 |
| `tcp` | TCP 流 |
| `syslog-tcp` / `syslog-udp` | Syslog 协议 |
| `kafka` | Kafka topic |
| `arrow-ipc` | Arrow IPC 流 |
| `blackhole` | 丢弃（调试用） |

## 默认值

```toml
# sinks/defaults.toml
batch_size = 1024
batch_timeout_ms = 1000
```
