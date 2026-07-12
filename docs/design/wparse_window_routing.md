# Wparse Output To Window Routing

本文说明 `warp-parse` 输出的数据如何在 `warp-fusion` 中分发到 window。这是跨组件联动里最容易配错的地方：上游输出名、source 路由字段、Arrow frame tag、WFS `stream_tag` 必须指向同一个逻辑值。

## 核心规则

`warp-fusion` 只认一个运行时分发键：

```text
logical stream tag
```

在 `warp-parse -> warp-fusion` 链路中，这个值应来自 OML 的 `name` / `full_name`。OML 完成数据转换，因此 OML 输出身份就是下游 window 订阅的逻辑 stream。

```text
OML name / full_name
  -> 输出格式中的 carrier
  -> wfusion source 解析为 stream tag
  -> window.stream_tag 匹配
  -> 数据进入对应 window
```

## 不同输出格式

同一个 logical stream tag 在不同格式中的承载方式不同：

| wparse 输出格式 | carrier | wfusion 获取方式 |
| --- | --- | --- |
| `arrow_framed` | Arrow frame tag | source 未配置固定 `stream_tag` 时，读取 frame tag |
| `ndjson` / JSON | payload 字段 `wp_oml_name` | `stream_tag_field = "wp_oml_name"` |
| `csv` | 列 `wp_oml_name` | `stream_tag_field = "wp_oml_name"` |

`wp_oml_name` 是非 Arrow framed 格式中的显式 carrier。Arrow framed 已经有 frame tag，默认不需要再输出 `wp_oml_name` 列；如果为了审计保留，值必须和 frame tag 一致。

## Arrow Framed 链路

`warp-parse` 的 OML：

```oml
name : nginx_access
rule : /nginx/*
---
* : auto = take() ;
```

`warp-parse` 输出 `arrow_framed` 时，应把 OML name 写入 Arrow frame tag：

```text
frame tag = nginx_access
```

`warp-fusion` source 不要写固定 `stream_tag`，让运行时读取 frame tag：

```toml
connect = "tcp_src"
enable = true
key = "nginx_tcp"
addr = "0.0.0.0"
port = "9802"
framing = "len"
data_format = "arrow_framed"
```

`warp-fusion` WFS window 订阅同一个值：

```wfs
window conn_events {
    stream_tag = "nginx_access"
    time = recv_time
    over = 30m
    fields { sip: ip  recv_time: time  digit: digit  chars: chars }
}
```

此时数据分发链路是：

```text
OML name nginx_access
  -> Arrow frame tag nginx_access
  -> source 读取 frame tag
  -> conn_events.stream_tag = nginx_access
  -> 投递到 conn_events
```

不要在 wparse 的 sink 中手写固定 `tag = "nginx_access"` 作为常规方案。这个值应由 OML 输出身份自动产生；手写 tag 容易和 OML name 漂移。

## NDJSON / JSON 链路

如果 `warp-parse` 输出 NDJSON，每行必须带 `wp_oml_name`：

```json
{"wp_oml_name":"network.netflow","sip":"10.0.0.1","dip":"10.0.0.2"}
{"wp_oml_name":"network.dns","sip":"10.0.0.1","query":"example.com"}
```

`warp-fusion` source 通过 `stream_tag_field` 指定字段名：

```toml
connect = "file_src"
enable = true
key = "wparse_json"
path = "data/parsed.ndjson"
data_format = "ndjson"
stream_tag_field = "wp_oml_name"
```

WFS 中不同 window 订阅不同 logical stream tag：

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

## 路由优先级

`warp-fusion` source 的路由优先级是：

```text
source.stream_tag 固定配置
  > Arrow framed frame tag
  > stream_tag_field 指定的 payload 字段/列
  > 报错或路由失败
```

因此：

- 配了 `source.stream_tag`，所有数据都会被强制投递到该 stream，payload 中的 `wp_oml_name` 或 Arrow frame tag 不参与分发。
- `arrow_framed` 推荐不配固定 `stream_tag`，使用 frame tag。
- `ndjson` / `csv` 多流输入推荐不配固定 `stream_tag`，使用 `stream_tag_field = "wp_oml_name"`。
- `arrow_ipc` 不携带 frame tag，必须配置固定 `stream_tag`。

## 常见错误

1. `window.stream_tag` 和 OML name 不一致。

   例如 OML 是 `name : nginx_access`，但 WFS 写 `stream_tag = "netflow"`，数据不会进入该 window。

2. `arrow_framed` source 写了固定 `stream_tag`。

   固定 `stream_tag` 会覆盖 Arrow frame tag。多 OML / 多 stream 混合输入时，这会把所有数据投到同一个 stream。

3. NDJSON 使用了旧字段名。

   `warp-parse` 当前输出字段是 `wp_oml_name`，不是 `wp_stream_tag`。如果 source 没配置 `stream_tag_field`，`warp-fusion` 的默认动态字段也是 `wp_oml_name`。

4. 把 sink name 当成 stream tag。

   sink 的 `name` / `full_name` 用于投递端点、日志和指标，不是数据分发键。数据分发键应来自 OML name / full_name。

5. 同一个 stream tag 对应的多个输入 schema 不一致。

   多个 window 可以订阅同一个 `stream_tag`，但同一个 stream 的输入批次字段结构需要能满足这些 window 的字段声明。不同逻辑结构的数据应使用不同 OML name，也就是不同 stream tag。

## 排查步骤

没有告警或 window 没有数据时，按这个顺序检查：

1. 确认 OML `name` / `full_name` 是期望的 stream tag。
2. 如果是 Arrow framed，确认 frame tag 等于 OML name。
3. 如果是 NDJSON / CSV，确认数据里有 `wp_oml_name`，且 source 使用 `stream_tag_field = "wp_oml_name"` 或依赖默认值。
4. 确认 source 没有误配固定 `stream_tag`。
5. 确认 WFS 中目标 window 的 `stream_tag` 和 OML name 完全一致。
6. 确认规则消费的是该 window。

