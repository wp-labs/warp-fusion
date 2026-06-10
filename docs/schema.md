# Schema 定义 (`.wfs`)

## 事件窗口

有时间和数据流的窗口，接收外部数据。

```
window conn_events {
    stream = "netflow"       # 匹配数据源 [[sources]].stream
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

- `stream`：匹配 `wfusion.toml` 中 `[[sources]].stream` 的值。支持多窗口订阅同一 stream，引擎会自动路由
- `time`：时间字段。引擎按此字段排序和驱逐事件
- `over`：窗口最大范围。`wfusion.toml` 中的 `over_cap` 必须 ≥ 此值

## 输出窗口（告警）

无时间窗口，仅定义告警数据结构。规则 `yield` 的目标。

```
window security_alerts {
    over = 0                  # 0 = 无时间驱逐
    fields {
        sip: ip
        dip: ip
        alert_type: chars
        detail: chars
    }
}
```

## Provider 窗口（外部数据）

静态数据窗口，数据来自 PostgreSQL 或文件，不订阅 stream。

```
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

注意：`digit` 虽是整数但存储为 `Int64`，规则表达式中可以直接参与 `sum`/`avg` 等数值聚合。
