# close_demo — 窗口关闭模式演示

演示 WFL 三种窗口关闭触发方式：`eos`（流结束）、`timeout`（超时）、`flush`（刷新）。

## 运行

```bash
# 1. 先运行内联测试（不需要 wfusion 进程）
wfl test rules/close_demo.wfl --schemas "schemas/*.wfs"

# 2. 用 wfusion binary 运行完整管道（batch 模式，读 NDJSON 文件）
cargo run --release -- run -c examples/close_demo/wfusion.toml
```

## 目录结构

```
close_demo/
├── wfusion.toml           # 引擎配置（batch 模式，NDJSON 文件输入）
├── schemas/
│   └── network.wfs        # window schema: conn_events + network_alerts
├── rules/
│   └── close_demo.wfl     # 规则 + 内联测试
├── sinks/
│   └── business.d/
│       └── network_alerts.toml
├── data/
│   └── conn_events.ndjson # 9 条连接事件，3 个 IP 各 3 条
└── out/
    └── alerts.ndjson      # 告警输出
```

## 规则说明

```wfl
events { c : conn_events && bytes >= 200 }
match<sip:5m> {
    on event { c | count >= 1; }
    and close { total: c | count >= 2; }
} -> score(70.0)
entity(ip, c.sip)
yield network_alerts (sip = c.sip, alert_type = "close_demo", detail = "triggered")
```

- **event 过滤**: `bytes >= 200` 排除小包噪音
- **match**: 所有合格事件命中
- **close**: 累计 ≥ 2 个事件才 close
- **AND 模式**: match 标记状态，close 时合并判定

## 样本数据预期

| IP | bytes | 匹配? | 预期 |
|----|-------|-------|------|
| 10.0.0.1 | 100, 200, 300 | 2 条 (200, 300) | 1 hit |
| 10.0.0.2 | 100, 200, 300 | 2 条 (200, 300) | 1 hit |
| 10.0.0.3 | 50, 150, 250 | 1 条 (250) | 0 hit（未达到 close 阈值） |

IP 10.0.0.3 只有 1 条事件 bytes ≥ 200（250），不满足 `total: c | count >= 2`，不会产生告警。
