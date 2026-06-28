# Examples — WarpFusion 安全检测场景

| 场景 | 目录 | 检测类型 | 核心模式 |
|------|------|---------|---------|
| 端口扫描 + 白名单 | `port_scan_whitelist/` | 扫描检测 | distinct + count 阈值 + join anti 排除 |
| SSH 暴力破解 | `ssh_brute_force/` | 暴力破解 | count 阈值 + 多目标聚合 |
| SQL 注入探测 | `sqli_probe/` | Web 攻击 | URI 模式匹配 + count 阈值 |
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

---

## 通用 Schema

所有场景共用或可扩展以下窗口定义：

```wfs
window auth_events {
    stream = "auth"
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
    stream = "dns"
    time = event_time
    over = 24h
    fields {
        sip: ip, qtype: chars, name: chars, name_len: digit
        event_time: time
    }
}

window http_events {
    stream = "http"
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
# 规则内联测试
wfl test rules/<scenario>.wfl --schemas "schemas/*.wfs"

# 离线回放
wfl replay rules/<scenario>.wfl --input data/events.ndjson

# 完整引擎（batch 模式）
wfusion batch -c ./wfusion.toml
```
