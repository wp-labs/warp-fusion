# streaming_ipv6 — IPv6 全链路流式示例

`streaming` 的 IPv6 版本：**wpgen(IPv6 nginx 日志) → TCP → wparse → Arrow TCP → wfusion**。

演示 WFL / wfusion 对 **IPv6 地址**的端到端支持：

- **wpl `ip` 类型**完整解析 IPv6（`2001:db8::1`、`::ffff:192.168.1.10`、`fd00::/8` ULA 等，基于 Rust `std::net::IpAddr`）。
- **wfusion `Ip` 字段**（`sip: ip`）接受 IPv6，规则在 IPv6 上做 match key / `entity(ip, ...)` / yield。
- 规则与 `streaming` 相同（`scan_detect` / `traffic_spike`，阈值 `count >= 50/100`），告警里的 `sip` 是 IPv6。

## 与 streaming 的差异

| | streaming | streaming_ipv6 |
|---|---|---|
| 样本数据 | `models/wpl/sample.dat`（IPv4） | `models/wpl/sample.dat`（**手写 IPv6** nginx CLF 日志） |
| wparse `wpl` | 共享 `../../models/wpl` | 本地 `../models/wpl`（IPv6） |
| wpgen `wpl` | 共享 | 本地（IPv6 样本逐行发送） |
| wfusion 规则/schema/windows | 共享 `../../models/*` | **本地** `../models/{wfl,schemas,windows.toml}`（`scan_detect` / `traffic_spike` 规则自包含） |

> 注：`wpgen sample` 的 `ip` 生成器只产 IPv4，因此 IPv6 样本直接写在 `models/wpl/sample.dat`，
> 由 `wpgen sample` 逐行读取发送（同 `wparse/wp-examples/core/ipv6_examples` 的做法）。

## 运行

```bash
./run.sh [debug|release]
```

流程：wfusion 起在 :9802 → wparse 起在 :9801 → wpgen 发送 IPv6 nginx 日志 → wparse 解析（`sip` 为 IPv6）→ Arrow 到 wfusion → 规则触发 → `data/alerts/*.ndjson`。

`sip` 样本集中在 `2001:db8::/32`、`fd00::/8`、`::ffff:192.168.1.10`（IPv4 映射 IPv6），便于在告警里直观核对 IPv6 地址。
