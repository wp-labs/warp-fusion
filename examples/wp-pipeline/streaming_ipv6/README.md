# streaming_ipv6 — IPv6 全链路流式示例

`streaming` 的 IPv6 版本：**wpgen(IPv6 nginx 日志) → TCP → wparse → Arrow TCP → wfusion**。

演示 WFL / wfusion 对 **IPv6 地址**的端到端支持：

- **wpl `ip` 类型**解析 IPv6 客户端地址。
- **wfusion `Ip` 字段**（`sip: ip`）接受 IPv6，规则在 IPv6 上做 match key / `entity(ip, ...)` / yield。
- 规则 `scan_detect`（`count >= 50`）/ `traffic_spike`（`count >= 100`）在 IPv6 地址上触发，告警 `sip` / `entity_id` / match scope 均为 IPv6。

## 与 streaming 的差异

| | streaming | streaming_ipv6 |
|---|---|---|
| 样本数据 | `models/wpl/sample.dat`（IPv4） | `models/wpl/sample.dat`（**手写 IPv6** nginx CLF 日志） |
| wparse `wpl` | 共享 `../../models/wpl` | 本地 `../models/wpl`（IPv6） |
| wpgen `wpl` | 共享 | 本地（IPv6 样本逐行发送） |
| wfusion 规则/schema/windows | 共享 `../../models/*` | **本地** `../models/{wfl,schemas,windows.toml}`（自包含） |

> **样本约束**（实测）：
> - `wpgen sample` 的 `ip` 生成器只产 IPv4，IPv6 样本直接写在 `models/wpl/sample.dat`，由 `wpgen sample` 逐行读取发送。
> - 当前 wparse `ip` 类型 + 本 parse 结构对**压缩短格式** IPv6（`2001:db8::1`、`::ffff:x.x.x.x`、`fd00::/8`）解析失败（`group[1]`），**完整形式** IPv6（如 `2001:db8:85a3::8a2e:370:7334`）正常。wfusion 引擎本身对两种形式都支持（`std::net::IpAddr`）。
> - `http/agent` 类型只接受 Mozilla 风格 UA（`curl/8.5.0`、`python-requests/2.31.0` 会解析失败）。

## 运行

```bash
./run.sh [debug|release]
```

流程：wfusion 起在 :9802 → wparse 起在 :9801 → wpgen 发送 IPv6 nginx 日志 → wparse 解析（`sip` 为 IPv6）→ Arrow 到 wfusion → 规则触发 → `data/alerts/*.ndjson`。

默认 `LINE_CNT=600` 时 5 个 IPv6 地址各触发 scan + traffic 告警（5+5 条）。样本集中在 `2001:db8:85a3::/48`，便于在告警里核对 IPv6 地址。
