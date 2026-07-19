# Wparse 输出分发到 Window 使用指南

本文面向实际配置：如何把 `warp-parse` 输出的数据投递到 `warp-fusion` 的正确 window。

核心约定只有一个：

```text
warp-parse OML name / full_name = warp-fusion window.stream_tag
```

也就是说，`warp-parse` 里 OML 的输出名是什么，`warp-fusion` 里接收这个数据的 window 就写同一个 `stream_tag`。

## 1. 在 warp-parse 中设置 OML name

OML 示例：

```oml
name : nginx_access
rule : /nginx/*
---
* : auto = take() ;
```

这里的 `nginx_access` 就是下游 `warp-fusion` 的 logical stream tag。

## 2. 在 warp-fusion 中声明 window

WFS 示例：

```wfs
window conn_events {
    stream_tag = "nginx_access"
    time = recv_time
    over = 30m
    fields { sip: ip  recv_time: time  digit: digit  chars: chars }
}
```

`stream_tag = "nginx_access"` 必须和 OML `name : nginx_access` 一致。

## 3. Arrow framed 实时接入

这是 `warp-parse -> TCP Arrow framed -> warp-fusion` 的推荐实时链路。

`warp-parse` sink 只需要声明输出 `arrow_framed`，不要把 stream 名写死在 sink 参数里：

```toml
version = "2.0"

[sink_group]
name = "parsed_nginx_out"
oml = ["*"]
batch_size = 1024
batch_timeout_ms = 300

[[sink_group.sinks]]
connect = "tcp_sink"
name = "arrow_tcp_out"

[sink_group.sinks.params]
protocol = "arrow"
data_format = "arrow_framed"
addr = "127.0.0.1"
port = 9802
```

`warp-fusion` source 读取 Arrow frame tag：

```toml
connect = "tcp_src"
enable = true
key = "nginx_tcp"
addr = "0.0.0.0"
port = "9802"
framing = "len"
data_format = "arrow_framed"
```

不要在这个 source 中配置固定 `stream_tag`。固定 `stream_tag` 会覆盖 Arrow frame tag，导致多 OML / 多 stream 场景全部进入同一个 stream。

数据流向：

```text
OML name nginx_access
  -> Arrow frame tag nginx_access
  -> wfusion source 读取 frame tag
  -> window conn_events(stream_tag = "nginx_access")
```

## 4. NDJSON / JSON 文件接入

如果 `warp-parse` 输出 NDJSON，每行需要携带 `wp_oml_name`：

```json
{"wp_oml_name":"network.netflow","sip":"10.0.0.1","dip":"10.0.0.2"}
{"wp_oml_name":"network.dns","sip":"10.0.0.1","query":"example.com"}
```

`warp-fusion` source 配置：

```toml
connect = "file_src"
enable = true
key = "wparse_json"
path = "data/parsed.ndjson"
data_format = "ndjson"
stream_tag_field = "wp_oml_name"
```

对应 WFS：

```wfs
window conn_events {
    stream_tag = "network.netflow"
    time = event_time
    over = 10m
    fields { sip: ip  dip: ip  event_time: time }
}

window dns_events {
    stream_tag = "network.dns"
    time = event_time
    over = 10m
    fields { sip: ip  query: chars  event_time: time }
}
```

`stream_tag_field` 是字段名，不是 stream tag 的值。当前推荐字段名是 `wp_oml_name`。

## 5. CSV 接入

CSV 和 NDJSON 的逻辑相同，使用 `wp_oml_name` 列：

```csv
wp_oml_name,event_time,sip,dip
network.netflow,2026-01-01T00:00:00Z,10.0.0.1,10.0.0.2
network.netflow,2026-01-01T00:00:01Z,10.0.0.3,10.0.0.4
```

source 配置：

```toml
connect = "file_src"
enable = true
key = "wparse_csv"
path = "data/parsed.csv"
data_format = "csv"
stream_tag_field = "wp_oml_name"
```

## 6. 单 stream 固定路由

如果一个 source 永远只有一种数据，可以使用固定 `stream_tag`：

```toml
connect = "file_src"
enable = true
key = "netflow_file"
path = "data/netflow.ndjson"
data_format = "ndjson"
stream_tag = "network.netflow"
```

这种配置会强制所有输入进入 `network.netflow`，即使 payload 里有 `wp_oml_name` 也不会参与分发。多 OML / 多 stream 输入不要这样配。

## 7. 验证方式

配置完成后先检查项目：

```bash
cd examples/wp-pipeline/streaming/wfusion
wfadm check
```

运行 streaming 示例：

```bash
cd examples/wp-pipeline/streaming
./run.sh
```

预期至少能看到规则对应的告警文件非 0 字节，例如：

```text
wfusion alerts:
  data/alerts/scan.ndjson     489 bytes
  data/alerts/traffic.ndjson  491 bytes
```

多 stream 文件路由示例：

```bash
cd examples/rules/multi_stream_multi_window
./run.sh
```

预期输出：

```text
OK: multi-stream dynamic routing produced 2 expected alerts
```

## 8. 排查清单

没有告警时按顺序检查：

1. OML `name` 是否就是期望的 stream tag。
2. WFS `window.stream_tag` 是否和 OML name 完全一致。
3. `arrow_framed` source 是否误配了固定 `stream_tag`。
4. NDJSON / CSV 是否使用 `wp_oml_name`，不是旧字段 `wp_stream_tag`。
5. NDJSON / CSV source 是否配置了 `stream_tag_field = "wp_oml_name"`，或依赖默认值。
6. 规则是否消费了对应 window。
7. 同一个 stream tag 下输入字段结构是否满足所有订阅 window 的字段声明。

更底层的机制说明见 [Wparse Output To Window Routing](../design/wparse_window_routing.md)。
