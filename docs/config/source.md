# 数据源配置 (`[[sources]]`)

格式与 `wp-core-connectors` 一致，connector 特有参数放在 `[sources.params]` 子表中。

## 文件（批处理 / 回放）

```toml
[[sources]]
type = "file"
key = "events_source"            # 可选标识符（默认 file_{N}）
enabled = true

[sources.params]
path = "data/events.ndjson"
stream = "netflow"               # 匹配 schema 中的 window.stream
format = "ndjson"                # ndjson | arrow_ipc
```

### 支持的格式

| format | 说明 | 适用场景 |
|--------|------|---------|
| `ndjson` | 一行一个 JSON 对象 | 回放、调试 |
| `arrow_ipc` | 标准 Arrow IPC File | 大规模批量导入 |

## TCP（实时）

```toml
[[sources]]
type = "tcp"
key = "netflow_input"

[sources.params]
listen = "tcp://0.0.0.0:9800"
```

每帧格式：长度前缀 + tag（stream name）+ Arrow IPC RecordBatch。

## Kafka

```toml
[[sources]]
type = "kafka"
key = "netflow_kafka"

[sources.params]
brokers = "localhost:9092"
topic = "netflow"
group_id = "wfusion"
stream = "netflow"
format = "ndjson"
```

> 需 `rdkafka` crate，当前为占位实现。

## 多源

```toml
[[sources]]
type = "tcp"
[sources.params]
listen = "tcp://0.0.0.0:9800"

[[sources]]
type = "file"
[sources.params]
path = "data/historical.ndjson"
stream = "netflow"
format = "ndjson"
```

## 底层实现

| type | 实现 |
|------|------|
| `file` | `wp_core_connectors::sources::batch::file::FileBatchSource` |
| `tcp` | `wp_core_connectors::sources::batch::tcp::TcpBatchSource` |
| `kafka` | 规划中（通过 `wp_core_connectors` 扩展） |
