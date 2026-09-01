# wp-pipeline demo

Batch end-to-end example for the shared `examples/wp-pipeline/models` rules:

```text
wpgen -> file -> wparse -> NDJSON -> wfusion -> alerts
```

This demo uses the same model contract as `../streaming`:

- `models/oml/example.oml` declares `name : nginx_access`.
- wparse JSON output carries that value in `wp_oml_name`.
- wfusion source reads `stream_tag_field = "wp_oml_name"`.
- `models/schemas/network.wfs` routes it to `window conn_events { stream_tag = "nginx_access" }`.
- `models/wfl/_global.wfl` defines shared `yield preset base_alerts`; alert rules use
  `yield <alert_window> : base_alerts (...)` to keep common output fields such as
  `rule_name = @__wfu_rule_name` in one place.

## Run

```bash
LINE_CNT=5000 ./run.sh
```

Use release binaries:

```bash
LINE_CNT=5000 ./run.sh release
```

## Output

Generated files are written under this example's `data/` directory:

```text
data/in_dat/gen.dat
data/out_dat/parsed.ndjson
data/alerts/scan.ndjson
data/alerts/traffic.ndjson
data/alerts/default.ndjson
data/alerts/error.ndjson
```

`parsed.ndjson` is the handoff between wparse and wfusion. Each row should include
`wp_oml_name = "nginx_access"` so wfusion can dispatch it to the matching window.
