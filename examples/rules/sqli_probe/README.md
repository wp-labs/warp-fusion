# sqli_probe — SQL 注入探测

URI 中包含 SQL 注入特征（单引号、union select、sleep 等）且短时间高频出现则告警。

## 场景

- 攻击者使用 sqlmap 等自动化工具扫描 Web 应用
- URI 参数中包含编码后的 SQL 片段（`'`、`union select`、`or 1=1`、`sleep()`、`benchmark()`）
- 同一源 IP 在 5 分钟内发起 ≥ 5 次注入尝试

## 运行

```bash
# 1. 内联测试
wfl test rules/sqli_probe.wfl --schemas "schemas/*.wfs"

# 2. 离线回放
wfl replay rules/sqli_probe.wfl --input data/http_events.ndjson

# 3. 完整引擎（batch 模式）
wfusion batch -c ./wfusion.toml
```

## 规则

```
rule sqli_probe {
    events {
        h : http_events && h.method == "GET"
            && (regex_match(h.uri, "'")
                || regex_match(h.uri, "union.*select")
                || regex_match(h.uri, "sleep")
                || regex_match(h.uri, "benchmark")
                || regex_match(h.uri, "1=1"))
    }
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
- **匹配条件**: 5 分钟内 ≥ 5 次注入请求
- **事件过滤**: GET 请求 + `regex_match()` 匹配注入特征（`'`、`union.*select`、`sleep`、`benchmark`、`1=1`），多个模式用 `||` 组合

## 测试

| 测试 | 场景 | 预期 |
|------|------|------|
| `sqli_detected` | sqlmap 扫描，5 次注入请求 | 1 hit |
| `below_threshold` | 单次注入 + 正常请求，共 3 次 | 0 hit |
| `normal_traffic` | 4 次正常访问 | 0 hit |
