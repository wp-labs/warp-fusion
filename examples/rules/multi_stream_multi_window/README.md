# multi_stream_multi_window

演示一个 source 中混合多个 `wp_oml_name`，并分发到多个 window。

- `topology/sources/mixed.toml` 只有一个文件 source，不配置固定 `stream`。
- `topology/sources/mixed.toml` 配置 `stream_tag_field = "wp_oml_name"`。
- `data/mixed_events.ndjson` 每行带 `wp_oml_name`。
- `wp_oml_name = "netflow"` 进入 `conn_events` window。
- `wp_oml_name = "dns"` 进入 `dns_events` window。
- 两个规则分别检测连接侧端口扫描和 DNS 长 TXT 查询，最终都写入 `security_alerts`。

运行：

```bash
wfusion batch --config wfusion.toml --work-dir .
```

期望输出：

- `data/out_dat/alerts.ndjson` 中产生 2 条告警。
- `netflow_syn_scan` 来自 `wp_oml_name = "netflow"` / `conn_events` window。
- `dns_tunnel_probe` 来自 `wp_oml_name = "dns"` / `dns_events` window。
