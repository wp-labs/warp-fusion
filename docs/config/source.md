# 数据源配置 (`[[sources]]`)

配置格式与 `wp-core-connectors` 一致，使用 connector 模式。

## 文件（批处理 / 回放）

```toml
[[sources]]
type = "file"              # connector 类型
key = "events_source"         # 可选标识符（默认 file_{N}）
path = "data/events.ndjson"
stream = "netflow"             # 匹配 schema 中的 window.stream
format = "ndjson"              # ndjson | arrow-ipc
enabled = true
```

### 支持的格式

| format | 说明 | 适用场景 |
|--------|------|---------|
| `ndjson` | 一行一个 JSON 对象 | 回放、调试 |
| `arrow-ipc` | 标准 Arrow IPC File | 大规模批量导入 |

### 字段映射

- 字段名：与 schema `fields` 中的名称一致
- 时间字段：ISO 8601 格式字符串
- IP 字段：字符串
- 数字字段：整数或字符串均可

## TCP（实时）

```toml
[[sources]]
type = "tcp"
key = "netflow_input"   # 可选
listen = "tcp://0.0.0.0:9800"
enabled = true
```

每帧格式：长度前缀 + tag（stream name）+ Arrow IPC RecordBatch。

## Kafka

```toml
[[sources]]
type = "kafka"
key = "netflow_kafka"
brokers = ["localhost:9092"]
topic = "netflow"
group_id = "wfusion"
stream = "netflow"
format = "ndjson"          # ndjson | arrow-ipc
enabled = true
```

> **依赖**：需 `rdkafka` crate，当前为占位实现。

## 多源

```toml
[[sources]]
type = "tcp"
listen = "tcp://0.0.0.0:9800"

[[sources]]
type = "file"
path = "data/historical.ndjson"
stream = "netflow"
format = "ndjson"
```

## 底层实现

| type | 实现 |
|---------|------|
| `file` | `wp_core_connectors::sources::batch::file::FileBatchSource` |
| `tcp` | `wp_core_connectors::sources::batch::tcp::TcpBatchSource` |
| `kafka` | 规划中（通过 `wp_core_connectors` 扩展） |
