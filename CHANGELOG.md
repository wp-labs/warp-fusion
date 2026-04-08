# Changelog

All notable changes to `warp-fusion` will be documented in this file.

The format is based on Keep a Changelog. Historical entries created before this
file existed are reconstructed from the repository state and current workspace
layout.

## [Unreleased]

### Changed

- Reserved for changes after `0.1.0`.

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
  adjacent `wp-reactor` repository through path dependencies.

### Notes

- This entry captures the initial public workspace baseline currently tracked in
  the repository.
