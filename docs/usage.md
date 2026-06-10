# WarpFusion 使用指南

## 安装

```bash
git clone https://github.com/wp-labs/warp-fusion.git
cd warp-fusion
cargo build --release
```

编译产物：
- `target/release/wfusion` — 统一入口（引擎运行 + 规则工具）
- `target/release/wfgen` — 场景生成工具（可选）
- `target/release/wfl` — 规则开发工具（可选）

---

## 快速开始

以 `port_scan_whitelist` 为例：

```bash
cd examples/port_scan_whitelist

# 1. 内联测试
wfl test rules/port_scan_whitelist.wfl --schemas "schemas/*.wfs"

# 2. 离线回放
wfl replay rules/port_scan_whitelist.wfl --input data/conn_events.ndjson

# 3. 引擎运行
wfusion run -c ./wfusion.toml
```

---

## 配置 (`wfusion.toml`)

### 基础配置

```toml
mode = "batch"   # batch 或 daemon

[runtime]
executor_parallelism = 2    # 并行度
rule_exec_timeout = "30s"   # 规则执行超时
schemas = "schemas/*.wfs"   # schema 文件 glob
rules   = "rules/*.wfl"     # 规则文件 glob
sinks   = "sinks"            # sink 配置目录
```

### 数据源

#### TCP（实时）

```toml
[[sources]]
type = "tcp"
name = "netflow_input"
listen = "0.0.0.0:9800"
```

每帧带 tag（stream name）+ Arrow RecordBatch，格式为 `wp_arrow` IPC。

#### 文件（批处理 / 回放）

```toml
# NDJSON
[[sources]]
type = "file"
name = "events_source"
path = "data/events.ndjson"
stream = "netflow"
format = "ndjson"

# CSV
[[sources]]
type = "file"
path = "data/events.csv"
stream = "auth"
format = "csv"

# Arrow IPC
[[sources]]
type = "file"
path = "data/events.arrow"
stream = "netflow"
format = "arrow-ipc"

# Arrow Framed
[[sources]]
type = "file"
path = "data/events.wparrow"
stream = "netflow"
format = "arrow-framed"
```

### 窗口配置

```toml
[window_defaults]
evict_interval = "30s"
max_window_bytes = "64MB"
max_total_bytes = "256MB"
evict_policy = "time_first"
watermark = "1s"
allowed_lateness = "0s"
late_policy = "drop"

# 单窗口覆盖
[window.conn_events]
mode = "local"
max_window_bytes = "64MB"
over_cap = "30m"       # 必须 >= schema 的 over 值
```

### 监控

```toml
[metrics]
enabled = true
report_interval = "10s"   # 指标采集周期
```

指标通过 monitor sink 输出，配置在 `sinks/infra.d/monitor.toml`：

```toml
[sink_group]
name = "monitor_infra"
windows = ["*"]

[[sink_group.sinks]]
connect = "file_json"
name = "monitor_out"
[sink_group.sinks.params]
file = "metrics.ndjson"
```

### 日志

```toml
[logging]
level = "info"
format = "plain"
file = "wfusion.log"
```

---

## Schema 定义 (`.wfs`)

### 事件窗口

```
window conn_events {
    stream = "netflow"       # 匹配数据源的 stream name
    time = event_time         # 时间字段
    over = 30m                # 窗口最大时长
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

### 输出窗口（告警）

```
window security_alerts {
    over = 0                  # 0 = 无时间窗口
    fields {
        sip: ip
        alert_type: chars
        detail: chars
    }
}
```

### 静态窗口（Provider / Knowledge DB）

```
window<provider> scanner_whitelist {
    fields {
        sip: ip
        note: chars
    }
}
```

### 字段类型

| 类型 | 说明 | 示例值 |
|------|------|--------|
| `ip` | IP 地址 | `10.0.0.1` |
| `digit` | 整数 | `443` |
| `float` | 浮点数 | `70.5` |
| `chars` | 字符串 | `"ssh"` |
| `bool` | 布尔值 | `true` |
| `time` | 时间戳 | `2026-01-01T00:00:00Z` |

---

## 规则编写 (`.wfl`)

### 基本结构

```
use "network.wfs"

rule rule_name {
    events {
        alias : window_name && filter_condition
    }
    match<group_key:window_duration> {
        on event {
            step_label: alias.field | transform | measure cmp threshold;
        }
        and close { total: alias | count >= threshold; }
    } -> score(score_value)
    entity(entity_type, alias.field)
    yield output_window (
        field1 = expr1,
        field2 = expr2
    )
}
```

### 单别名（基础）

```
// 端口扫描检测
rule port_scan {
    events { c : conn_events && action == "syn" }
    match<sip:5m> {
        on event { c.dport | distinct | count >= 10; }
    } -> score(80.0)
    entity(ip, c.sip)
    yield alerts (sip = c.sip, alert_type = "port_scan")
}
```

### 多别名（多步匹配）

```
// 远控扩散：scan → login → xfer
rule rat_propagation {
    events {
        scan  : conn_events && bytes_out < 1000
        login : auth_events && result == "success"
        xfer  : conn_events && bytes_out >= 10000
    }
    match<sip,dip:30m> {
        on event {
            scan | count >= 1;
            login | count >= 1;
            xfer | count >= 1;
        }
    } -> score(95.0)
    entity(ip, scan.sip)
    yield alerts (sip = scan.sip, dip = scan.dip, alert_type = "rat")
}
```

### 事件过滤器

```
// 等值比较
c.service == "ssh" && c.result == "failed"

// 数值比较
c.bytes_out < 1000 && c.dport >= 1024

// 正则匹配（多个模式用 || 组合）
regex_match(h.uri, "'") || regex_match(h.uri, "union.*select")

// 端口集合
c.dport == 22 || c.dport == 445 || c.dport == 3389
```

### 匹配步

```
// 聚合函数：count, sum, avg, min, max, distinct
on event { c | count >= 10; }            // 事件数 ≥ 10
on event { c.dport | distinct | count >= 5; }  // 不同端口 ≥ 5
on event { c.bytes_out | sum >= 100000; }      // 总字节 ≥ 100K

// 多步（顺序匹配）
on event {
    step1: a | count >= 1;
    step2: b | count >= 1;
}

// close 条件
and close { total: c | count >= 20; }        // 累计 ≥ 20
and close { c | count >= 10; }               // 关闭条件满足时才产出

// 分组键
match<sip:5m>           // 按源 IP 分组，5 分钟窗口
match<sip,dip:30m>      // 按源 IP+目标 IP 分组，30 分钟
```

### Join

```
// 反 join（白名单排除）
join scanner_whitelist anti on c.sip == scanner_whitelist.sip

// Snapshot join（IP 富化）
join internal_ips snapshot on c.sip == internal_ips.ip
```

### 测试用例

```
test test_name for rule_name {
  input {
    row(alias, field1 = "value1", field2 = "value2", event_time = "2026-01-01T00:00:00Z");
    row(alias, field1 = "value3", event_time = "2026-01-01T00:00:10Z");
  }
  expect {
    hits == 1;
    hit[0].entity_id == "10.0.0.99";
  }
}
```

---

## 命令参考

### `wfusion` — 统一入口

```bash
# 启动引擎
wfusion run -c ./wfusion.toml

# 配置检查
wfusion config render -c wfusion.toml
wfusion config origins -c wfusion.toml
wfusion config vars -c wfusion.toml
wfusion config diff -c wfusion.toml --to-config other.toml

# 场景生成
wfusion scenario gen --scenario test.wfg --out /tmp/out
wfusion scenario lint --scenario test.wfg

# 规则工具
wfusion rule explain --file rules/test.wfl
wfusion rule lint --file rules/test.wfl
wfusion rule fmt rules/*.wfl
wfusion rule replay --file rules/test.wfl --input data/events.ndjson
wfusion rule verify --file rules/test.wfl --case mycase
wfusion rule test --file rules/test.wfl
```

### `wfl` — 规则开发（独立工具）

```bash
wfl test rules/test.wfl --schemas "schemas/*.wfs"
wfl replay rules/test.wfl --input data/events.ndjson
```

---

## 示例

| 示例 | 检测场景 | 核心模式 |
|------|---------|---------|
| `port_scan_whitelist/` | 端口扫描 + 白名单 | distinct + count + join anti |
| `ssh_brute_force/` | SSH 暴力破解 | count 阈值 + 多目标 |
| `sqli_probe/` | SQL 注入探测 | regex_match + count |
| `rat_propagation/` | 远控扩散（凭据窃取）| 多步 scan→login→xfer |
| `rat_propagation/` | 远控扩散（漏洞利用）| 多步 scan→xfer |

详见 [`examples/README.md`](../examples/README.md)。

---

## 目录结构

```
warp-fusion/
├── wfusion.toml           # 主配置
├── rules/                 # .wfl 规则文件
├── schemas/               # .wfs schema 文件
├── sinks/                 # sink 配置
│   ├── infra.d/           #   基础设施 sink（default/error/monitor）
│   ├── business.d/        #   业务路由 sink
│   ├── connectors/        #   connector 定义
│   └── defaults.toml      #   全局 sink 默认值
├── data/                  # 离线回放数据
├── out/                   # 输出
└── docs/                  # 文档
```
