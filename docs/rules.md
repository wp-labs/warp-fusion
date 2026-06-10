# 规则编写 (`.wfl`)

## 基本结构

```
use "network.wfs"                  # 引用 schema 文件

rule rule_name {
    events {                       # 事件声明（支持多别名）
        alias : window_name && filter_condition
    }
    match<group_key:window_duration> {   # 匹配窗口
        on event {                        # 匹配步
            step_label: alias.field | transform | measure cmp threshold;
        }
        and close { total: alias | count >= threshold; }  # 关闭条件（可选）
    } -> score(score_value)
    join window_name join_type on condition    # Join（可选）
    entity(entity_type, alias.field)           # 实体
    yield output_window (                      # 输出
        field1 = expr1,
        field2 = expr2
    )
}
```

## 事件声明

### 单别名

```
events { c : conn_events && action == "syn" }
```

### 多别名（多步匹配用）

```
events {
    scan  : conn_events && bytes_out < 1000
    login : auth_events && result == "success"
    xfer  : conn_events && bytes_out >= 10000
}
```

多别名之间用换行分隔，不加分号。每个别名绑定到一个 window，引擎按 stream 自动路由事件到正确的别名。

## 事件过滤器

```
// 等值比较
c.service == "ssh" && c.result == "failed"

// 数值比较
c.bytes_out < 1000 && c.dport >= 1024

// 端口集合
c.dport == 22 || c.dport == 445 || c.dport == 3389

// 正则匹配（多个模式用 || 组合）
(regex_match(h.uri, "'") || regex_match(h.uri, "union.*select"))
```

## 匹配步

### 聚合

| 聚合 | 说明 | 示例 |
|------|------|------|
| `count` | 事件数 | `c \| count >= 10` |
| `sum` | 求和 | `c.bytes_out \| sum >= 100000` |
| `avg` | 平均值 | `c.score \| avg >= 70` |
| `min` / `max` | 最小/最大值 | `c.score \| max >= 90` |
| `distinct` | 去重 | `c.dport \| distinct \| count >= 5` |

### 单步

```
// 5 分钟内同一 IP 产生 ≥ 10 次事件
match<sip:5m> {
    on event { c | count >= 10; }
}
```

### 多步（顺序匹配）

```
// scan → login → xfer 三步序列
match<sip,dip:30m> {
    on event {
        scan | count >= 1;
        login | count >= 1;
        xfer | count >= 1;
    }
}
```

关键：多步在同一个 `on event` 块内顺序执行。只有前一步满足后，状态机才推进到下一步。缺任一步都不命中。

### Close 条件

```
// 关闭条件：所有步满足 + 总数 ≥ 10 时才产出
and close { total: c | count >= 10; }
```

### 分组键

| 分组键 | 含义 |
|--------|------|
| `match<sip:5m>` | 按源 IP 分组，5 分钟窗口 |
| `match<sip,dip:30m>` | 按源 IP + 目标 IP 分组，30 分钟 |
| `match<:1h>` | 无分组键，全局 1 小时窗口 |

## Join

### Anti Join（白名单排除）

```
join scanner_whitelist anti on c.sip == scanner_whitelist.sip
```

匹配 `scanner_whitelist` 中相同 `sip` 的事件被排除。

### Snapshot Join（富化）

```
join internal_ips snapshot on c.sip == internal_ips.ip
```

匹配时从 `internal_ips` 获取 `department`、`owner` 等字段，可在 `yield` 中引用。

## Yield（输出）

```
yield security_alerts (
    sip = c.sip,
    dip = c.dip,
    alert_type = "port_scan",
    detail = "detected"
)
```

yield 中可直接引用事件字段、join 窗口字段、字符串常量。聚合操作（`| count`、`| distinct`）**不能**用于 yield，只能在匹配步中使用。

## 测试用例

```
test test_name for rule_name {
  input {
    row(alias, field1 = "value1", field2 = "value2", event_time = "2026-01-01T00:00:00Z");
    row(alias, field1 = "value3", event_time = "2026-01-01T00:00:10Z");
  }
  expect {
    hits == 1;                        // 期望命中数
    hit[0].entity_id == "10.0.0.99";  // 期望的实体 ID
  }
}
```

- `row(alias, ...)` — alias 对应 `events` 中声明的别名
- `input` — 事件按时间顺序注入，支持 `Tick(duration)` 推进时间
- `expect` — `hits` 期望命中数；`hit[i].field` 检查特定命中的字段值
- 一个 `.wfl` 文件只能有一条规则，测试用例必须在该规则内

## 完整示例

```
use "network.wfs"

rule port_scan {
    events { c : conn_events && action == "syn" }
    match<sip:5m> {
        on event { c.dport | distinct | count >= 10; }
    } -> score(80.0)
    join scanner_whitelist anti on c.sip == scanner_whitelist.sip
    entity(ip, c.sip)
    yield alerts (sip = c.sip, alert_type = "port_scan", detail = ">= 10 distinct ports")
}

test scan_detected for port_scan {
  input {
    row(c, sip = "10.0.0.99", dip = "192.168.1.1", dport = "80", action = "syn", event_time = "2026-01-01T00:00:00Z");
    // ... 9 more events to reach threshold
  }
  expect { hits == 1; }
}
```
