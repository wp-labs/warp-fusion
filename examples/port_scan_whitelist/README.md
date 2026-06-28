# port_scan_whitelist — 端口扫描检测（已知漏扫工具白名单）

生产环境中大量端口扫描流量的检测与已知漏扫器排除。

## 场景

- 多台服务器出现大规模端口扫描行为
- `10.0.2.1` 是部署在环境中的漏扫工具（已知、可信）
- 需要排除漏扫器，对其他达到阈值的源 IP 告警

## 运行

```bash
# 1. 内联测试
wfl test rules/port_scan_whitelist.wfl --schemas "schemas/*.wfs"

# 2. 离线回放
wfl replay rules/port_scan_whitelist.wfl --input data/conn_events.ndjson

# 3. 完整引擎（batch 模式）
wfusion batch -c ./wfusion.toml
```

## 规则

```
rule port_scan_whitelist {
    events { c : conn_events && action == "syn" }
    match<sip:5m> {
        on event { c.dport | distinct | count >= 10; }
    } -> score(80.0)
    entity(ip, c.sip)
    yield security_alerts (sip = c.sip, alert_type = "port_scan", detail = "distinct ports >= 10")
}
```

- **分组键**：`sip`（源 IP），每个 IP 独立计数
- **匹配条件**：5 分钟内扫描 ≥ 10 个不同目标端口
- **白名单策略**：理想做法是在 event 声明中 `sip != "10.0.2.1"` 过滤，当前 WFL 不支持 IP 类型的值比较，需要在数据采集层处理

## 白名单实现方案

| 方案 | 说明 |
|------|------|
| `_stream` 分流 | 漏扫器流量用不同 stream name，窗口不订阅 |
| schema 扩展 | 增加 `scanner_tag: chars` 字段，`scanner_tag != "vuln_scanner"` |
| 上游过滤 | agent 直接丢弃已知 scanner 的连接日志 |

## 测试

| 测试 | 场景 | 预期 |
|------|------|------|
| `scan_detected` | 单个 IP 扫 10 个端口 | 1 hit |
| `below_threshold` | 单个 IP 扫 3 个端口 | 0 hit |
| `isolated_by_sip` | 两个 IP 各扫 2-3 个端口 | 0 hit（sip 隔离不跨 IP 累计） |
