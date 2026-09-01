# 窗口配置

## 全局默认 `[window_defaults]`

所有窗口的默认值，可被单窗口覆盖。

```toml
[window_defaults]
evict_interval = "30s"       # 驱逐检查间隔
max_window_bytes = "64MB"    # 单窗口最大内存
max_total_bytes = "256MB"    # 全局窗口总内存上限
evict_policy = "time_first"  # 驱逐策略
watermark = "1s"             # 水位线延迟
allowed_lateness = "0s"      # 允许迟到时间
late_policy = "drop"         # 迟到处理策略
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `evict_interval` | duration | 驱逐检查周期，如 `"30s"`、`"1m"` |
| `max_window_bytes` | bytes | 单窗口内存上限，如 `"64MB"`、`"1GB"` |
| `max_total_bytes` | bytes | 所有窗口总内存上限 |
| `evict_policy` | string | `time_first` 或 `lru` |
| `watermark` | duration | 水位线 = 最大事件时间 - 此值，早于水位线的不能追加 |
| `allowed_lateness` | duration | 允许迟到的最大时间，超过视为迟到 |
| `late_policy` | string | `drop`（丢弃）、`revise`（修正水位线）、`side_output`（旁路输出） |

## 单窗口覆盖 `[window.<name>]`

窗口名须与 `.wfs` schema 中的 `window` 名一致。

```toml
[window.conn_events]
mode = "local"                # local | replicated | partitioned
max_window_bytes = "64MB"
over_cap = "30m"              # 必须 >= schema 的 over 值
evict_policy = "time_first"
watermark = "1s"
allowed_lateness = "0s"
late_policy = "drop"
# table = "scanner_whitelist"  # 仅 Provider 窗口使用
```

| 字段 | 说明 |
|------|------|
| `mode` | 分布式模式：`local`（本节点处理）、`replicated`（复制到所有节点）、`partitioned`（按 key 分区） |
| `over_cap` | 窗口容量上限，**必须 ≥ schema 中的 `over` 值** |
| `table` | Provider 窗口关联的 knowdb 表名，普通窗口不需要 |

### Duration 格式

```
30s  = 30 秒
5m   = 5 分钟
2h   = 2 小时
1d   = 1 天
```

### Bytes 格式

```
64MB  = 64 兆字节
1GB   = 1 吉字节
256MB = 256 兆字节
```

## Provider 窗口

Provider 窗口通过 `table` 关联 knowdb 中的数据：

```toml
# wfusion.toml
[window.internal_ips]
mode = "local"
max_window_bytes = "1MB"
over_cap = "0s"
table = "internal_ips"
```

```toml
# knowdb.toml
[table.internal_ips]
source = "pg"
query = "SELECT ip, department, owner FROM internal_ips"
```
