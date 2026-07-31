# Changelog (English)

## [0.1.40 Unreleased]

### wfgen — split `--no-oracle` and `--no-wfl` (#58 follow-up)

- In 0.1.39 `--no-oracle` was an alias of `--no-wfl`, which made `--no-oracle` skip injection `use()` fixed values too (empty `rule_plans` → `has_inject = false`), so generated fields were all random. They are now distinct, orthogonal flags:
  - `--no-wfl`: skip the entire WFL pipeline (no compilation, no injection, no oracle); baseline background events.
  - `--no-oracle`: still compiles WFL (keeps injection `use()` fixed values), only skips oracle/expected output.
  - Verified: `brute_force.wfg` with `--no-oracle` yields 60000 events including 48000 with `action=failed/success` fixed values; with `--no-wfl` it yields 0 fixed values (all random). Use `--no-oracle` for inject-aware events without oracle files; use `--no-wfl` for plain baseline events.

### wfusion — logging level now authoritative (#59)

- **Fixed #59**: `[logging] level` is now the single source of truth for the log level and is no longer silently overridden by the `RUST_LOG` env var. `init_tracing` previously switched to `EnvFilter::from_default_env()` (ignoring `[logging] level`) whenever `RUST_LOG` was set — even to an empty string — so `level = "info"` still emitted DEBUG/TRACE. It now always builds `EnvFilter` from `[logging]` (`level` + `modules`) and never reads `RUST_LOG`. For per-module overrides, use the supported `[logging].modules` (e.g. `wf_runtime::receiver = "debug"`).
- **Dependency**: `wf-engine` / `wf-config` / `wf-lang` / `wf-data` / `wf-runtime` aligned to `wp-reactor` v0.1.39 (includes the logging fix).
- **Verified**: running `wfusion batch` on close_demo with `level = "info"` and `RUST_LOG=debug` set yields 0 DEBUG / 0 TRACE lines (previously leaked); batch still produces the expected alerts.

## [0.1.39 Unreleased]

### wfgen — CLI: optional `--out`, `--no-oracle` renamed to `--no-wfl`

- **`--out` now optional**: `wfgen gen --out` is no longer required; it decouples from `--send` into four combinations: `--out` only (write files), `--send` only (stream over TCP, no disk), both, or neither (returns a clear usage error instead of a silent no-op).
- **`--no-oracle` renamed to `--no-wfl`**: semantics widened from "skip oracle output only" to "skip the entire WFL pipeline" — no rule compilation, no `_global.wfl` / yield-preset evaluation, no oracle/expected output, and no compile-time warnings such as `unknown yield preset`. With `rule_plans` empty, generation falls back to baseline background events (**behavior change**: `--no-wfl` no longer produces inject-aware hit/near-miss cluster events). `--no-oracle` is retained as a backward-compatible alias with identical behavior.
- **Oracle skip warning**: when the scenario requests oracle/expected output but only `--send` is given (no `--out`), prints `Warning: oracle/expected output requested but --out not set; skipping expected generation` instead of silently dropping it.
- **Tests**: added CLI parse tests (`--send` without `--out`, `--no-wfl`, `--no-oracle` alias) and a `run` arg-validation unit test (returns `no output target` error when neither sink is given).

## [0.1.22] — 2026-07-07

### wfusion — admin API binding and TLS loading

- **Fixed**: allow `admin_api.bind = "0.0.0.0:..."` with `admin_api.tls.enabled = false`; non-loopback admin listeners no longer require TLS.
- **Fixed**: initialize the rustls ring `CryptoProvider` from the production TLS loading path, avoiding TLS startup panics when the provider was not installed elsewhere.
- **Tests**: added coverage for non-loopback admin API startup with TLS disabled and kept HTTPS coverage for non-loopback binds.

### wfgen

- **Added**: top-level `wfgen --version` output.
- **Tests**: added a regression test for the clap version flag.

## [Unreleased] — 2026-07-06

### wfusion — path base changed from config-file-relative to working-dir-relative

- **Break**: Default `runtime_base_dir` changed from `config_path.parent()` (the `conf/` dir containing the config file) to `current_dir()` (process working directory). `wfadm check` updated accordingly.
- Impact: all relative paths in `wfusion.toml` (`sources_dir` / `sinks` / `schemas` / `rules`) must remove one `..` level.
  - Before: `"../topology/sources"` → After: `"topology/sources"`
  - Before: `"../../../models/schemas/"` → After: `"../../models/schemas/"`
- `base` paths in `business.d/*.toml` also remove one `..` level (`"../../data/alerts"` → `"../data/alerts"`).
- Unifies path resolution with wparse (both now working-dir-relative), eliminating inconsistent `..` counts within the same project.
- `--work-dir` CLI flag behavior unchanged (explicit override takes priority).

### Example pipelines — fixed

- **streaming**: Added missing `protocol = "arrow"` in `parsed_netflow.toml` to fix Arrow IPC decode errors.
- **streaming**: Added `[models].wpl` in `wpgen.toml` to share models directory with `wparse.toml`.
- **streaming / kafka**: Changed `wpgen sample` output port to integer (avoids connector param type mismatch), replaced fixed `sleep` with `wait_port` readiness probes in `run.sh`.
- **kafka**: Changed wfusion source `data_format` from `arrow_framed` to `ndjson` to match wparse kafka sink's JSON output.
- **kafka**: Removed `demo.toml` (debug sink whose `oml = ["*"]` matched first in OML routing, preventing kafka sink from receiving records).

## [Unreleased] — 2026-06-22

### Dependencies — Centralized & Upgraded

- **arrow** 54 → 59 (IPC encoding compatibility)
- **wp-arrow** 0.1 → 0.2 (arrow 59 support)
- **wp-core-connectors** 0.5.5 → 0.5.6
- **toml** 0.9 → 1.0
- **wf-connector-api** 0.1 → 0.2
- **sha2** 0.10 → 0.11
- **rand** pinned to `=0.9.0` (prevents 0.10 upgrade breaking `random_range` API)

### Workspace — Dependency Centralization

All crate-level dependency versions moved to `[workspace.dependencies]`:

| Dependency | Crates |
|-----------|--------|
| `serde_json`, `chrono`, `clap`, `tokio`, `rand` | wfgen, wfl, wfusion |
| `wp-arrow`, `wp-connector-api`, `tracing` | wfgen, wfusion |

This ensures a single source of truth for version management and prevents
drift between crates.

### wfgen — Deterministic Scenario Timestamps

- Default scenario start time changed from `Utc::now()` to fixed
  `"2026-01-01T00:00:00Z"`. Fixes non-deterministic test failures
  (`test_fault_deterministic`) and ensures reproducible data generation.

### wfgen — Chunked TCP Send in Stream Mode

- Stream command splits generated events into 1000-row chunks before
  sending via `TcpArrowSink`. Prevents wfusion's TCP source (64KB
  batch cap) from choking on single giant frames.

### Tests — e2e Tests Self-Contained

- Copied schemas, rules, sinks, and connectors from `wp-reactor/examples/`
  into `crates/wfgen/examples/`. e2e tests no longer require `wp-reactor`
  to be checked out alongside `warp-fusion`. CI can now build and test
  with only the `warp-fusion` repository.
- Updated all `.wfg` scenario files to use local relative paths
  (`../schemas/`, `../rules/`).

### Docs — AI Agent Skills Guide

- Added `skills/test-pipeline-guide.md`: an AI-agent-oriented
  troubleshooting guide covering the wf-rules test pipeline
  (wfgen → wfusion → alerts). Documents common failure modes,
  diagnostic techniques, and quick verification commands.

---

## [0.1.11] — 2026-06-21

### wfgen — Use wp-core-connectors TcpArrowSink for TCP Send

- **Dependencies**: Added `wp-core-connectors`, `wp-connector-api`, `tokio`
- **Refactor**: `tcp_send.rs` rewritten from raw `TcpStream` + manual Arrow IPC
  encoding → `TcpArrowSink::connect()` + `encode_batch_payload_with_tag()` +
  `send_payload()`
  - Arrow IPC encoding via `encode_ipc_frame` (compatible with `wp_arrow::ipc::encode_ipc`)
  - Framing: RFC6587 octet-counted (`<len> <payload>`), matching wfusion `tcp_src` `framing = "len"`
  - Transport: `NetWriter` with backpressure
- **Async**: `cmd_stream`, `cmd_send`, `cmd_bench`, `cmd_gen` all converted to `async fn`
- **Dependency**: `wp-core-connectors` 0.5.2 → 0.5.5 (exposes `encode_batch_payload_with_tag` as public API)
