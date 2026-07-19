# Examples — WarpFusion 安全检测场景

| 场景 | 目录 | 检测类型 | 核心模式 |
|------|------|---------|---------|
| 端口扫描 + 白名单 | `port_scan_whitelist/` | 扫描检测 | distinct + count 阈值 + join anti 排除 |
| SSH 暴力破解 | `ssh_brute_force/` | 暴力破解 | count 阈值 + 多目标聚合 |
| SQL 注入探测 | `sqli_probe/` | Web 攻击 | URI 模式匹配 + count 阈值 |
| 单 stream 多 window | `single_stream_multi_window/` | 路由演示 | 一个 stream 同时分发到 conn_events / dns_events |
| 多 stream 多 window | `multi_stream_multi_window/` | 路由演示 | 一个 source 中的 netflow / dns `wp_oml_name` 分别进入两个 window |
| Window miss | `window_miss/` | 路由诊断 | unknown / missing `wp_oml_name` 进入内置 miss 诊断，合法 stream 继续处理 |
| 远控扩散（凭据窃取） | `rat_propagation/` | 攻击链 | 多步匹配 scan→login→xfer |
| 远控扩散（漏洞利用） | `rat_propagation/` | 攻击链 | 多步匹配 scan→xfer |
| DNS 隧道 | `dns_tunneling/` | 数据外泄 | 长域名 + TXT 查询统计 |
| 横向移动 | `lateral_movement/` | 内网渗透 | 内部 IP 间 SMB/RDP 扫描 |
| C2 Beacon | `c2_beacon/` | 远控回连 | 周期性低字节心跳检测 |

---

## 场景详情

### 1. SSH 暴力破解 `ssh_brute_force`

单 IP 在短时间内大量 SSH 失败登录，且目标主机分散。

```
rule ssh_brute_force {
    events { c : auth_events && service == "ssh" && result == "failed" }
    match<sip:5m> {
        on event { c | count >= 10; }
        and close { total: c | count >= 30; }
    } -> score(70.0)
    join scanner_whitelist anti on c.sip == scanner_whitelist.sip
    entity(ip, c.sip)
    yield security_alerts (
        sip = c.sip,
        alert_type = "ssh_brute_force",
        detail = "failed attempts >= 10",
        targets = c.dip | values | join(",")
    )
}
```

- **分组键**: `sip`
- **匹配**: 5 分钟内失败 ≥ 10 次，总失败 ≥ 30 次
- **排除**: join anti 排除已知漏扫器

### 2. DNS 隧道 `dns_tunneling`

检测通过 DNS TXT 查询外传数据的行为——长域名、高查询量、字节总量异常。

```
rule dns_tunneling {
    events { d : dns_events && d.qtype == "TXT" && d.name_len >= 40 }
    match<sip:10m> {
        on event { d.name_len | sum >= 500; }
        and close { total: d | count >= 20; }
    } -> score(85.0)
    entity(ip, d.sip)
    yield security_alerts (
        sip = d.sip,
        alert_type = "dns_tunneling",
        detail = "suspicious TXT queries with long domain names"
    )
}
```

- **分组键**: `sip`
- **匹配**: 10 分钟内 TXT 查询 ≥ 20 次，域名总长度 ≥ 500

### 3. 横向移动 `lateral_movement`

内网主机之间 SMB(445) 或 RDP(3389) 连接，短时间内接触 ≥ 5 个不同内部目标。

```
rule lateral_movement {
    events { c : conn_events && c.dport == 445 || c.dport == 3389 && is_internal(c.sip) && is_internal(c.dip) }
    match<sip:10m> {
        on event { c.dip | distinct | count >= 5; }
        and close { total: c | count >= 20; }
    } -> score(75.0)
    entity(ip, c.sip)
    yield alerts (
        sip = c.sip,
        alert_type = "lateral_movement",
        detail = "SMB/RDP scanning >= 5 internal hosts"
    )
}
```

- **分组键**: `sip`
- **匹配**: 10 分钟内连接 ≥ 5 个不同内部目标，总连接 ≥ 20 次

### 4. C2 Beacon `c2_beacon`

检测周期性、低字节的对外连接——典型的 C2 心跳特征。

```
rule c2_beacon {
    events { c : conn_events && c.bytes_out < 200 && c.bytes_in < 500 }
    match<sip,dip,dport:24h> {
        on event { c | interval_variance <= 60; }
        and close { total: c | count >= 12; }
    } -> score(90.0)
    entity(ip, c.sip)
    yield security_alerts (
        sip = c.sip,
        dip = c.dip,
        alert_type = "c2_beacon",
        detail = "periodic low-byte heartbeat detected"
    )
}
```

- **分组键**: `sip,dip,dport`（同一连接目标）
- **匹配**: 24 小时内 ≥ 12 次，间隔方差 ≤ 60s

### 5. SQL 注入探测 `sqli_probe`

上游采集侧通过 `regex_match()` 标记注入特征（单引号、union select、sleep 等），规则侧通过 `sqli_tag` 字段过滤。

```
rule sqli_probe {
    events { h : http_events && h.method == "GET" && h.sqli_tag == "true" }
    match<sip:5m> {
        on event { h | count >= 5; }
        and close { total: h | count >= 5; }
    } -> score(80.0)
    entity(ip, h.sip)
    yield security_alerts (
        sip = h.sip,
        alert_type = "sqli_probe",
        detail = "SQL injection attempt detected",
        sample_uri = h.uri
    )
}
```

- **分组键**: `sip`
- **匹配**: 5 分钟内 ≥ 5 次注入请求

### 6. 单 stream 多 window `single_stream_multi_window`

演示 `window` 里的 `stream_tag = "netflow"` 如何作为数据分发键。示例只有一个 `file_src`，输出 `stream_tag = "netflow"`；schema 中 `conn_events` 和 `dns_events` 都绑定同一个 stream，因此同一份输入数据会同时进入两个 window。

```
window conn_events {
    stream_tag = "netflow"
    time = event_time
    over = 10m
    fields { sip: ip, dip: ip, dport: digit, protocol: chars, action: chars, event_time: time }
}

window dns_events {
    stream_tag = "netflow"
    time = event_time
    over = 10m
    fields { sip: ip, dip: ip, qtype: chars, query: chars, query_len: digit, event_time: time }
}
```

- **输入**: `data/netflow_events.ndjson`
- **分发**: 一个 `netflow` source 同时写入 `conn_events` 与 `dns_events`
- **规则**: 端口扫描规则消费 `conn_events`；DNS 长 TXT 查询规则消费 `dns_events`
- **验证**: 使用 `wfl replay`，输入行通过 `_stream = "netflow"` 触发分发

### 7. 多 stream 多 window `multi_stream_multi_window`

演示 batch 模式下一个 source 混合多个 `wp_oml_name`，并通过 `stream_tag_field = "wp_oml_name"` 分发到多个 window。`wp_oml_name = "netflow"` 进入 `conn_events`，`wp_oml_name = "dns"` 进入 `dns_events`，两个规则最终写入同一个 `security_alerts` 输出 window。

```
window conn_events {
    stream_tag = "netflow"
    time = event_time
    over = 10m
    fields { sip: ip, dip: ip, dport: digit, protocol: chars, action: chars, event_time: time }
}

window dns_events {
    stream_tag = "dns"
    time = event_time
    over = 10m
    fields { sip: ip, dip: ip, qtype: chars, query: chars, query_len: digit, event_time: time }
}
```

- **输入**: `data/mixed_events.ndjson`
- **分发**: `wp_oml_name = "netflow"` -> `conn_events`，`wp_oml_name = "dns"` -> `dns_events`
- **验证**: `wfusion batch --config wfusion.toml --work-dir .`，期望 `data/out_dat/alerts.ndjson` 产生 2 条告警

### 8. Window miss `window_miss`

演示动态路由输入中的坏 stream tag 不应阻塞同批次合法事件。示例使用
`stream_tag_field = "wp_oml_name"`，输入文件同时包含：

- `wp_oml_name = "netflow"`：合法 stream，进入 `conn_events` 并产生 1 条告警。
- `wp_oml_name = "unknown_stream"`：没有任何 window schema 订阅，记录为
  `unknown_stream_schema`。
- 缺失 `wp_oml_name`：记录为 `missing_stream_tag_field`。

- **输入**: `data/window_miss_events.ndjson`
- **分发**: 合法 `netflow` 进入业务 window；miss row 进入 runtime 诊断路径
- **验证**: `./run.sh`，期望合法 stream 产生 1 条告警，并在
  `data/out_dat/metrics.ndjson` 中检查到两条 `window_miss_total` monitor 统计

---

## 通用 Schema

所有场景共用或可扩展以下窗口定义：

```wfs
window auth_events {
    stream_tag = "auth"
    time = event_time
    over = 24h
    fields {
        sip: ip
        dip: ip
        service: chars
        result: chars
        event_time: time
    }
}

window dns_events {
    stream_tag = "dns"
    time = event_time
    over = 24h
    fields {
        sip: ip, qtype: chars, name: chars, name_len: digit
        event_time: time
    }
}

window http_events {
    stream_tag = "http"
    time = event_time
    over = 24h
    fields {
        sip: ip, dip: ip, method: chars, uri: chars, status: digit
        event_time: time
    }
}

window security_alerts {
    over = 0
    fields {
        sip: ip, alert_type: chars, detail: chars
        targets: chars, sample_uri: chars
    }
}
```

## 运行示例

```bash
# 全量规则验证：lint + 内联测试
./run_all.sh

# 单规则内联测试
cd ssh_brute_force
wfl test rules/ssh_brute_force.wfl --schemas "schemas/*.wfs"

# 离线回放
wfl replay rules/<scenario>.wfl --input data/events.ndjson

# 完整引擎（batch 模式）
wfusion batch -c ./wfusion.toml
```

`run_all.sh` 会先遍历各示例目录下的 `rules/*.wfl`，对每条规则执行：

1. `wfl lint <rule> --schemas "schemas/*.wfs"`
2. `wfl test <rule> --schemas "schemas/*.wfs"`

随后对可离线回放的示例调用 `wfusion batch --config wfusion.toml --work-dir .`，并按 case 校验期望告警数。`rat_propagation` 的 batch 多源顺序不可控、`weak_password` 的 join snapshot 告警由完整回放语义覆盖，因此期望告警数为 0，只要求 batch 成功退出且输出符合预期。`weak_password2` 依赖 Redis，默认跳过；准备好依赖后可用 `RUN_EXTERNAL=1 ./run_all.sh` 纳入验证。没有内联 `test` 的规则只要求 lint 通过，并标记为 `no inline tests`。脚本最后会打印逐 case `PASS` / `FAIL` / `SKIP` 汇总。
