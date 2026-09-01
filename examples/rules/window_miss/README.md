# Window Miss

This case demonstrates dynamic stream routing misses.

Input rows are routed by `wp_oml_name`:

- `netflow` is a known stream and enters `conn_events`.
- `unknown_stream` has no subscribed window schema and should be recorded as `unknown_stream_schema`.
- A row without `wp_oml_name` should be recorded as `missing_stream_tag_field`.

Run:

```bash
./run.sh
```

Expected result: batch replay succeeds and produces one alert from the known `netflow` row. The two bad rows should not fail the source or block the known row.

The case also configures `topology/sinks/infra.d/monitor.toml` and enables runtime metrics with a `1s` report interval. `run.sh` starts `wfusion daemon`, waits for `data/out_dat/metrics.ndjson`, and verifies both monitor counters:

- `window_miss_total{reason="unknown_stream_schema"} = 1`
- `window_miss_total{reason="missing_stream_tag_field"} = 1`
