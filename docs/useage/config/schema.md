# Schema 定义 (`.wfs`)

## 事件窗口

有时间和数据流的窗口，接收外部数据。

```wfs
window conn_events {
    stream_tag = "netflow"       # 匹配数据源 [[sources]].stream_tag
    time = event_time         # 时间字段名（必须为 time 类型）
    over = 30m                # 窗口最大保留时长
    fields {
        sip: ip
        dip: ip
        dport: digit
        bytes_out: digit
        protocol: chars
        event_time: time
    }
}
```

- `stream_tag`：匹配 `wfusion.toml` 中 `[[sources]].stream_tag` 的值，或匹配输入记录中 `stream_tag_field` 指定字段的值。支持多窗口订阅同一 stream tag，引擎会自动路由
- `time`：时间字段。引擎按此字段排序和驱逐事件
- `over`：窗口最大范围。`wfusion.toml` 中的 `over_cap` 必须 ≥ 此值

## 输出窗口（告警）

无时间窗口，仅定义告警数据结构。规则 `yield` 的目标。

```wfs
window security_alerts {
    over = 0                  # 0 = 无时间驱逐
    fields {
        sip: ip
        dip: ip
        alert_type: chars
        detail: chars
        matched_events: digit
        trigger_count: float
        first_seen: time
        last_seen: time
        rule_window_start: time
        rule_window_end: time
        latest_analysis_time: time
    }
}
```

输出窗口常用于接收规则 `yield` 结果。统计字段通常声明为 `digit` / `float`，时间语义字段通常声明为 `time`，例如：

- `matched_events = stat.count(match_event(label))` → `digit`
- `trigger_count = stat.value(trigger(label))` → `float`
- `first_seen = @event_first_time`、`last_seen = @event_last_time` → `time`
- `rule_window_start = @window_start_time`、`latest_analysis_time = @emit_time` → `time`

## Provider 窗口（外部数据）

静态数据窗口，数据来自 PostgreSQL 或文件，不订阅 stream tag。

```wfs
window<provider> scanner_whitelist {
    fields {
        sip: ip
        note: chars
    }
}
```

配置中通过 `table` 关联 knowdb：

```toml
[window.scanner_whitelist]
mode = "local"
over_cap = "0s"
table = "scanner_whitelist"     # knowdb.toml 中的表名
```

## 字段类型

| 类型 | 对应 Rust/Arrow | 示例值 |
|------|----------------|--------|
| `ip` | String / LargeString | `"10.0.0.1"` |
| `digit` | Int64 | `443` |
| `float` | Float64 | `70.5` |
| `chars` | String / LargeString | `"ssh"` |
| `bool` | Boolean | `true` |
| `time` | Timestamp(Nanosecond) | `2026-01-01T00:00:00Z` |
| `object` | JSON object / Utf8 JSON | `{"risk":90,"labels":["ssh"]}` |
| `array` | JSON array / Utf8 JSON | `["ssh","bruteforce"]` |
| `array/T` | typed JSON array / Utf8 JSON | `array/digit` -> `[22,80,443]` |

注意：`digit` 虽是整数但存储为 `Int64`，规则表达式中可以直接参与 `sum`/`avg` 等数值聚合。

结构化字段约束：

- 输入窗口（声明了 `stream_tag` 的 window）和 provider window 不允许声明 `object`、`array`、`array/T` 字段。源数据中的 JSON object/array 应先以 `chars` 接入。
- 输出窗口可以声明结构化字段，规则中用 WFL 的 `object { ... }` 和 `array [ ... ]` 构造后 `yield`。
- `wfgen` 生成测试数据时，`object` 默认生成 `{}`，未类型化 `array` 默认生成 `[]`，`array/T` 会生成同类型数组。
- `wfgen` 的 scalar generator override（如字符串、数字、`range()`、`ipv4()`）只适用于 base 类型和 `array/T` 的元素类型；对 `object` / 未类型化 `array` 使用 override 会在 lint 阶段报 `SV7`。
