# Changelog

All notable changes to `warp-fusion` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [0.1.8] — 2026-06-15

### Added

- **Connector-based source/sink architecture.** Sources and sinks are now built
  through a unified connector factory registry (`wp_core_connectors::registry`),
  symmetric between `SourceFactory` and `SinkFactory`.
- **Kafka source & sink.** `KafkaSourceFactory` and `KafkaSinkFactory` from
  `wp-connectors` are registered at startup, enabling wfusion to consume from
  and produce to Kafka topics.
- **Arrow IPC Stream support for TCP source.** The TCP source now supports
  Arrow IPC Stream format in addition to the legacy length-prefixed framed
  format.
- **External source task spawning.** `spawn_external_source_tasks()` in
  `wf-runtime` discovers source factories from the global registry, builds
  sources, and runs the consume → decode (ndjson) → route loop for any
  connector-based source kind.
- **wp-pipeline examples.** Three end-to-end pipeline examples under
  `examples/wp-pipeline/`:
  - `demo` — file → wparse → file → wfusion → file
  - `streaming` — wpgen → wparse → Arrow IPC over TCP → wfusion → alerts
  - `kafka` — wpgen → wparse → Kafka → wfusion → Kafka (alerts)
- **Detection scenario examples.** Added `port_scan_whitelist`,
  `rat_propagation`, `sqli_probe`, `ssh_brute_force`, `weak_password`, and
  `close_demo` examples with WFL rules, schemas, and topology configs.
- **moju-derive integration.** CLI config types annotated with `MoJu` derive
  for structured metadata generation.

### Changed

- **wp-reactor upgraded from v0.1.3 to v0.1.12.** Brings window validation
  (`validate_over_vs_over_cap`), pipeline internal windows, provider windows,
  and the connector-based sink dispatcher.
- **Added `wp-connectors` (v0.15.4) and `wp-core-connectors` (0.5.0).**
  Replaces inline sink implementations with the shared connector crate
  ecosystem.
- **Added `wf-connector-api` (0.1).** Arrow-native `BatchSource` trait for
  wfusion-native source consumption.
- **Sink building uses `SinkFactoryRegistry`.** `bootstrap.rs` registers
  built-in factories (`File`, `Syslog`, `TCP`, `BlackHole`) and imports
  additional factories from the global registry (`import_from_global_registry`).
- **CLI error handling** uses structured `orion-error` reports throughout
  `wfusion`, `wfl`, and `wfgen`.

### Fixed

- **Kafka pipeline example topic mismatch.** Corrected wparse sink topic from
  auto-generated test name to `wp_nginx_logs` to match wfusion source config.
- **Streaming example output format.** Fixed `run.sh` to check for `.ndjson`
  alert files instead of `.arrow`, and added graceful shutdown to flush
  windows before checking output.

## [0.1.0]

### Added

- Bootstrapped the `warp-fusion` Cargo workspace with three CLI deliverables:
  `wfusion`, `wfgen`, and `wfl`.
- Added the `wfusion` binary as the main WarpFusion runtime / config entrypoint,
  delegating execution to `wf-engine`.
- Added `wfgen` as the scenario-driven test data generator with `gen`, `lint`,
  `verify`, `send`, and `bench` subcommands.
- Added `wfl` as the rule developer toolchain with `explain`, `lint`, `fmt`,
  `replay`, `verify`, and `test` subcommands.
- Added integration coverage for `wfusion config` CLI flows, including
  rendered config output, variable inspection, origins tracing, and expanded
  diff reporting.
- Wired the workspace to reuse core runtime and language crates from the
  shared `wp-reactor` codebase.

### Notes

- This entry captures the initial public workspace baseline currently tracked in
  the repository.
