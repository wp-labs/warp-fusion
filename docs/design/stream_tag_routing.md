# Stream Tag Routing

`warp-fusion` 使用 `stream_tag` 作为输入数据到 window 的分发键。一个 window 在 `.wfs`
中声明 `stream_tag = "..."` 后，运行时会把同名 stream tag 的输入批次投递到该 window。
多个 window 可以订阅同一个 stream tag。

`warp-parse -> warp-fusion` 的完整分发契约见
[Wparse Output To Window Routing](./wparse_window_routing.md)。

## 逻辑语义

跨系统接入时，推荐把上游输出的逻辑身份表达为一个 tag：

```text
logical stream tag = 上游产物的逻辑模型名
```

在 `wparse -> warp-fusion` 场景中，这个值来自 OML 的 `name` / `full_name`。OML
负责完成数据转换，因此由 OML 产物身份决定下游 stream 是最稳定的边界。

## Carrier

同一个 logical stream tag 在不同数据格式中有不同承载方式：

| 输入格式 | carrier | 示例 |
| --- | --- | --- |
| Arrow framed | frame tag（隐式） | `tag = "network.netflow"` |
| JSON / NDJSON | payload 字段（显式） | `"wp_oml_name": "network.netflow"` |
| CSV / 表格文本 | payload 列（显式） | `wp_oml_name` 列 |

`wp_oml_name` 是非 Arrow framed 格式里的显式 payload carrier；Arrow framed
已经有 frame tag，不需要默认再增加同名列。

## 路由优先级

输入侧建议使用以下优先级：

```text
source.stream_tag 固定配置
  > Arrow framed frame tag
  > stream_tag_field 指定的 payload 字段/列
  > 报错
```

`source.stream_tag` 表示强制路由，适合单 stream source。未配置固定 `stream_tag` 时，
运行时才从 frame tag 或 `stream_tag_field` carrier 中提取 logical stream tag。

配置模板中可能显式写：

```toml
stream_tag = ""
```

这和省略固定 `stream_tag` 等价，表示“不要强制路由，使用输入数据自带的 tag”。
这种写法适合默认模板，因为它能让配置读者直接看到该 source 依赖 Arrow frame tag
或 `stream_tag_field`。

## wparse 输出契约

`wparse` 不需要理解 `warp-fusion` 的 window 结构，只需要把 OML 产物身份写入
格式对应的 carrier：

```text
Arrow framed:
  frame tag = OML name/full_name

JSON / NDJSON:
  wp_oml_name = OML name/full_name

CSV / 表格文本:
  wp_oml_name column = OML name/full_name
```

如果某个格式同时输出隐式 tag 和显式 payload carrier，两个值必须一致。路由时
隐式 frame tag 优先，显式字段主要用于非 Arrow framed 格式或调试审计。

## 与 sink 身份的区别

logical stream tag 不等同于 sink 的运行身份：

```text
logical stream tag: network.netflow
sink full_name: business/parsed_netflow
```

前者用于数据分发，后者用于投递端点、日志、指标和运维定位。不要用 sink
`full_name` 作为默认 stream tag。
