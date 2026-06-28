# ssh_brute_force — SSH 暴力破解检测

单 IP 短时间内大量 SSH 失败登录，且排除已知漏扫器。

## 场景

- 攻击者使用 hydra / medusa 等工具对多台服务器尝试弱口令
- 同一源 IP 在 5 分钟内产生 ≥ 10 次 SSH 失败登录
- 通过 `join anti` 排除已知漏扫器 IP

## 运行

```bash
# 1. 内联测试
wfl test rules/ssh_brute_force.wfl --schemas "schemas/*.wfs"

# 2. 离线回放
wfl replay rules/ssh_brute_force.wfl --input data/auth_events.ndjson

# 3. 完整引擎（batch 模式）
wfusion batch -c ./wfusion.toml
```

## 规则

```
rule ssh_brute_force {
    events {
        e : auth_events && e.service == "ssh" && e.result == "failed"
    }
    match<sip:5m> {
        on event { e | count >= 10; }
    } -> score(70.0)
    join scanner_whitelist anti on e.sip == scanner_whitelist.sip
    entity(ip, e.sip)
    yield security_alerts (
        sip = e.sip,
        alert_type = "ssh_brute_force",
        detail = "failed login >= 10 in 5min",
        targets = e.dip | values | join(","),
        target_count = e.dip | distinct | count
    )
}
```

- **分组键**: `sip`
- **匹配条件**: 5 分钟内失败 ≥ 10 次
- **白名单**: join anti 排除 `scanner_whitelist`
- **产出**: 攻击源 IP、被攻击目标列表、目标数量

## 测试

| 测试 | 场景 | 预期 |
|------|------|------|
| `brute_force_detected` | 单 IP 对 10 台主机爆破 | 1 hit，entity_id = 10.0.0.99 |
| `below_threshold` | 2 次失败 + 1 次成功，共 3 条 | 0 hit |
| `successful_login_ignored` | 10 次成功登录 | 0 hit（`result == "failed"` 过滤） |
| `isolated_by_sip` | 3 个 IP 各 2 次失败 | 0 hit（按 sip 隔离，各不达阈值） |
