# rat_propagation — 远控木马扩散检测

检测内网主机被远控后向其他主机扩散的行为。提供两条规则覆盖两种扩散模式：

| 规则 | 文件 | 检测链 | score | 适用场景 |
|------|------|--------|-------|---------|
| `rat_propagation` | `rules/rat_propagation.wfl` | scan → login → xfer | 95 | 凭据窃取后 SSH/SMB/RDP 扩散 |
| `rat_propagation_exploit` | `rules/rat_propagation_exploit.wfl` | scan → xfer | 90 | 漏洞利用 / 自有协议扩散（无登录日志） |

---

## 数据来源

三道事件来自**两个完全不同的数据源**，部署时每一层都必须到位：

```
                          ┌──────────────────────────────────┐
                          │     每台目标机器的 auth.log       │
                          │  /var/log/auth.log               │
                          │  "Accepted password for root     │
                          │   from 10.0.0.99 port 22"        │
               login ────►│                                  │
                          │  解析为 NDJSON 后上报             │
                          └──────────────────────────────────┘

                          ┌──────────────────────────────────┐
                          │  网络流 (netflow / IPFIX)         │
                          │  路由器/交换机镜像                │
                          │  sip → dip:dport, bytes_out      │
                scan ────►│                                  │◄──── xfer
                          │  解析为 Arrow IPC 后上报          │
                          └──────────────────────────────────┘
```

| 事件 | 别名 | 数据源 | 所在机器 | 采集方式 |
|------|------|--------|---------|---------|
| `scan` | conn_events | 网络流 (netflow/IPFIX) | 路由器/交换机 | sFlow / port mirror → netflow collector → Arrow IPC |
| `login` | auth_events | 系统认证日志 | **每台目标机器** | Filebeat / rsyslog / agent → NDJSON |
| `xfer` | conn_events | 网络流 (netflow/IPFIX) | 路由器/交换机 | 同 scan，同一流的不同统计维度 |

**关键约束**：`login` 事件来自目标机器本地 `/var/log/auth.log`。如果目标机器上没有部署日志采集，`login` 事件永远为空，`rat_propagation` 规则不会产生任何告警。

### 数据流架构

```
每台服务器                        采集层                           检测层
┌──────────┐   auth.log   ┌──────────────┐   NDJSON    ┌─────────────────┐
│ 目标 B    │ ──────────► │ filebeat/     │ ──────────► │                 │
│ 目标 C    │ ──────────► │ rsyslog/      │ ──────────► │  warp-fusion    │
│ 目标 D    │ ──────────► │ agent         │ ────────►  │  receiver       │
└──────────┘             └──────────────┘            │  (TCP :9800)    │
                                                      │                 │
┌──────────┐   netflow   ┌──────────────┐  Arrow IPC │  ┌───────────┐  │
│ 路由器    │ ──────────► │ netflow      │ ──────────► │  │ window    │  │
│ 交换机    │ ──────────► │ collector    │ ──────────► │  │ state     │  │
└──────────┘             └──────────────┘            │  │ machine   │  │
                                                      │  └───────────┘  │
                                                      │       │         │
                                                      │       ▼         │
                                                      │  ┌───────────┐  │
                                                      │  │ alert     │  │
                                                      │  │ (告警)    │  │
                                                      │  └───────────┘  │
                                                      └─────────────────┘
```

---

## 运行

```bash
# 1. 内联测试
wfl test rules/rat_propagation.wfl --schemas "schemas/*.wfs"
wfl test rules/rat_propagation_exploit.wfl --schemas "schemas/*.wfs"

# 2. 离线回放
wfl replay rules/rat_propagation.wfl --input data/demo_events.ndjson
wfl replay rules/rat_propagation_exploit.wfl --input data/demo_events.ndjson

# 3. 完整引擎（batch 模式）
wfusion batch -c ./wfusion.toml
```

> **多步匹配与多源 replay 的已知限制**：`rat_propagation` 依赖 `match<sip,dip:30m>` 三步顺序匹配（scan → login → xfer）。`wfl test` 中事件按时间顺序注入，三步可正确触发。`wfusion batch` batch 模式使用两个 `[[sources]]` 分别回放 conn_events 和 auth_events，两个文件并发读取，事件到达状态机的顺序不可控，会导致多步匹配的序列断裂。**因此告警逻辑的验证请以 `wfl test` 为准。** 实时部署中，各 source 独立推送事件，窗口内由 event timestamp 保证时间顺序，不存在此问题。
```

---

## 规则 1: rat_propagation（凭据窃取型扩散）

```
rule rat_propagation {
    events {
        scan  : conn_events && (dport == 22 || dport == 445 || dport == 3389) && bytes_out < 1000
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
    yield security_alerts (
        sip = scan.sip, dip = scan.dip,
        alert_type = "rat_propagation",
        detail = "scan -> login -> xfer"
    )
}
```

- **分组键**: `sip,dip`（每对源-目标独立跟踪）
- **多步匹配**: scan → login → xfer 顺序触发，缺任一步都不命中
- **score 95**: 三步全链 = 高置信度攻击行为

## 规则 2: rat_propagation_exploit（漏洞利用型扩散）

```
rule rat_propagation_exploit {
    events {
        scan : conn_events && (dport == 22 || dport == 445 || dport == 3389) && bytes_out < 1000
        xfer : conn_events && bytes_out >= 10000
    }
    match<sip,dip:30m> {
        on event {
            scan | count >= 1;
            xfer | count >= 1;
        }
    } -> score(90.0)
    entity(ip, scan.sip)
    yield security_alerts (
        sip = scan.sip, dip = scan.dip,
        alert_type = "rat_propagation_exploit",
        detail = "scan -> xfer (no login, possible exploit-based spread)"
    )
}
```

- 不依赖 `auth_events`，即使目标机器未部署日志采集也能工作
- score 90（低于凭据窃取型），因为缺少 login 步，误报可能性稍高

---

## 设计要点

| 要素 | 选择 | 原因 |
|------|------|------|
| 分组键 `sip,dip` | 每对 IP 独立窗口 | 不同目标的攻击链不能跨目标累计 |
| `scan` bytes_out < 1000 | 低字节 = 扫描特征 | 正常 SSH/RDP 登录后会有交互流量 |
| `scan` dport 22\|445\|3389 | 三类扩散协议 | SSH(22)、SMB(445)、RDP(3389) |
| `login` result == "success" | 仅成功登录 | 排除失败尝试（暴力破解由另一规则覆盖） |
| `xfer` bytes_out >= 10000 | 高字节 = 数据外传 | 排除正常敲命令的小流量 |
| 窗口 30m | 中时间窗口 | 扫描→登录→传输可能需要几分钟到十几分钟 |

## 内存估算

每个 `sip,dip` 创建一个状态机实例：~96 bytes。正常内网 1000 源 IP × 10 目标 = 10000 实例 ≈ **0.9 MB**。详见 [monitoring-design.md](../../docs/monitoring-design.md)。

## 测试

### rat_propagation

| 测试 | 场景 | 预期 |
|------|------|------|
| `full_chain_detected` | 单 IP 对 3 个目标完成 scan→login→xfer | 3 hits |
| `missing_xfer_step` | 3 个目标有 scan+login 但无 xfer | 0 hit |
| `admin_scan_only` | 4 个目标仅扫描 | 0 hit |
| `single_target_full_chain` | 1 个目标完成全链 | 1 hit |

### rat_propagation_exploit

| 测试 | 场景 | 预期 |
|------|------|------|
| `exploit_spread` | 单目标 scan→xfer | 1 hit |
| `exploit_no_xfer` | 单目标仅扫描 | 0 hit |
