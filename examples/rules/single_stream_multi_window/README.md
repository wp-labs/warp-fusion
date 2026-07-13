# single_stream_multi_window

演示一个 source stream tag 同时分发到多个 window。

`topology/sources/netflow.toml` 只有一个文件 source，声明 `stream_tag = "netflow"`。`schemas/netflow.wfs` 中 `conn_events` 和 `dns_events` 都声明 `stream_tag = "netflow"`，因此同一份 `data/netflow_events.ndjson` 会同时进入两个 window：

- `conn_events` 只关心连接字段，用于检测 SYN 端口扫描。
- `dns_events` 只关心 DNS 字段，用于检测长 TXT 查询。

运行回放验证：

```bash
wfl replay rules/multi_window_from_netflow.wfl --schemas "schemas/*.wfs" --input data/netflow_events.ndjson
```

期望输出：

- 5 条输入事件产生 2 个 match。
- 两条告警来自同一份输入数据，但分别由不同 window 上的规则触发。

说明：`data/netflow_events.ndjson` 中显式包含 `_stream = "netflow"`，用于 `wfl replay` 的路由。`wfusion.toml` 和 `topology/sources/netflow.toml` 保留了 batch/source 配置形态，用来展示真实 source 中 `stream_tag = "netflow"` 的配置位置。
