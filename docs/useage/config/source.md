# 数据源配置 (`[[sources]]`)

格式与 `wp-core-connectors` 一致，connector 特有参数可以直接放在 `[[sources]]` 中，也兼容 `[sources.params]` 子表；`[sources.params_override]` 是 `[sources.params]` 的别名。

如果同一个参数同时出现在 `[[sources]]` 平铺字段和 `[sources.params]` 中，平铺字段优先。`vars` 是配置加载保留字段，不能作为 source 参数名使用。

每个来源都支持 `enable` 开关，默认 `true`。`enable = false` 的来源不会启动，也不会参与 daemon/batch 模式的运行参数校验；名称仍需保持唯一，避免重新启用时产生冲突。

```toml
[[sources]]
connect = "kafka_src"
enable = false
key = "reserved_kafka"
stream_tag = "netflow"
brokers = "localhost:9092"
topic = "netflow"
data_format = "ndjson"
```

## Stream 分发

`window` 中的 `stream_tag = "..."` 是运行时分发键。source 可以用固定 stream tag，
也可以让每条数据携带 tag 来动态分发。

`warp-parse -> warp-fusion` 输出数据如何进入 window 的完整链路见
[Wparse 输出分发到 Window 使用指南](../wparse-window-routing.md)，底层机制见
[Wparse Output To Window Routing](../../design/wparse_window_routing.md)。

推荐的分发优先级：

```text
source.stream_tag 固定配置
  > Arrow framed frame tag
  > stream_tag_field 指定的 payload 字段/列
  > 报错
```

### 固定 stream tag

固定 stream tag 适合一个 source 只生产一种逻辑数据的场景：

```toml
[[sources]]
connect = "file_src"
key = "netflow_file"
path = "data/netflow.ndjson"
data_format = "ndjson"
stream_tag = "netflow"
```

此时所有输入行都进入 `stream_tag = "netflow"` 的 window。即使 payload 中存在
`_stream` 或 `wp_oml_name`，也不参与路由。

### 动态 tag

没有固定 `stream_tag` 时，source 可以从输入数据携带的 tag 中得到 stream tag。
在 `wparse -> warp-fusion` 场景中，tag 值应来自 OML `name` / `full_name`。

```toml
[[sources]]
connect = "file_src"
key = "wparse_json"
path = "data/parsed.ndjson"
data_format = "ndjson"
stream_tag_field = "wp_oml_name"
```

对应输入：

```json
{"wp_oml_name":"network.netflow","sip":"10.0.0.1","dip":"10.0.0.2"}
{"wp_oml_name":"network.dns","sip":"10.0.0.1","query":"example.com"}
```

对应 window：

```wfs
window conn_events {
    stream_tag = "network.netflow"
    time = event_time
    over = 10m
    fields { sip: ip, dip: ip, event_time: time }
}

window dns_events {
    stream_tag = "network.dns"
    time = event_time
    over = 10m
    fields { sip: ip, query: chars, event_time: time }
}
```

`stream_tag_field` 表示“当格式没有隐式 Arrow frame tag 时，从哪个字段/列读取等价 tag”。
它是字段名，不是 tag 值。`wparse` 的推荐字段名是 `wp_oml_name`。

batch NDJSON / CSV 回放已支持不配置固定 `stream_tag` 的动态分发；外部 NDJSON
source 也支持按 `stream_tag_field` 分组路由。`stream_tag_field = "wp_oml_name"` 是与
wparse 输出契约对齐的推荐配置。

### Arrow framed tag

`arrow_framed` 的 tag 是 frame 级元数据，不是 payload 字段：

```toml
[[sources]]
connect = "tcp_src"
key = "wparse_arrow"
listen = "tcp://0.0.0.0:9800"
data_format = "arrow_framed"
```

每个 Arrow frame 的 tag 作为 stream tag。对于 wparse 输出，frame tag 应等于
OML `name` / `full_name`。Arrow framed 默认不需要再增加 `wp_oml_name` 列；
如果为了调试或审计同时保留该列，列值必须和 frame tag 一致。

## 文件（批处理 / 回放）

```toml
[[sources]]
type = "file"
enable = true
key = "events_source"            # 可选标识符（默认 file_{N}）

[sources.params]
path = "data/events.ndjson"
stream_tag = "netflow"               # 匹配 schema 中的 window.stream_tag
format = "ndjson"                    # ndjson | csv | arrow_framed | arrow_ipc
```

### 支持的格式

| format / data_format | stream tag 来源 | 适用场景 |
|----------------------|----------------|---------|
| `ndjson` | 固定 `stream_tag`，或 `stream_tag_field` 指定的 JSON 字段 | 回放、调试、wparse JSON 输出 |
| `csv` | 固定 `stream_tag`，或 `stream_tag_field` 指定的列 | 表格文件回放 |
| `arrow_framed` | frame tag，或固定 `stream_tag` 覆盖 | wparse Arrow framed 输出 |
| `arrow_ipc` | 必须配置固定 `stream_tag` | 标准 Arrow IPC 文件 |

`data_format` 和历史 `format` 都可被识别；新配置建议使用 `data_format`。

## TCP（实时）

```toml
[[sources]]
type = "tcp"
enable = true
key = "netflow_input"

[sources.params]
listen = "tcp://0.0.0.0:9800"
```

TCP source 的业务格式由 `data_format` 决定：

- `arrow_framed`：每个 frame 自带 tag，未配置固定 `stream_tag` 时按 frame tag 路由。
- `ndjson`：未配置固定 `stream_tag` 时按 `stream_tag_field` 指定字段路由。
- `arrow_ipc`：不携带业务路由 tag，必须配置固定 `stream_tag`。

## Kafka

```toml
[[sources]]
type = "kafka"
enable = true
key = "netflow_kafka"

[sources.params]
brokers = "localhost:9092"
topic = "netflow"
group_id = "wfusion"
stream_tag = "netflow"
format = "ndjson"
```

> 需 `rdkafka` crate，当前为占位实现。

## 多源

```toml
[[sources]]
type = "tcp"
enable = true

[sources.params]
listen = "tcp://0.0.0.0:9800"

[[sources]]
type = "file"
enable = false

[sources.params]
path = "data/historical.ndjson"
stream_tag = "netflow"
format = "ndjson"
```

## 底层实现

| type | 实现 crate | 说明 |
|------|-----------|------|
| `file` | `wp_core_connectors` | `FileBatchSource` → NDJSON → Arrow RecordBatch |
| `tcp` | `wp_core_connectors` | `TcpBatchSource` → Arrow IPC |
| `kafka` | 规划中 | 通过 `wp_core_connectors` 扩展 |
