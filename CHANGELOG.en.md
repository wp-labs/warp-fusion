# Changelog (English)

This file records user-facing changes to `wfusion` / `wfl` / `wfgen` / `wfadm`.
Internal implementation details, dependency alignment, and test counts are not covered here.

## [0.1.45 Unreleased]

### Language (WFL)

- `on event<accu>` within-window accumulation: after the threshold is met, the window's count and evidence keep accumulating without reset, and each subsequent qualifying event re-fires with the running cumulative values and full evidence, until the window expires (aligned with `wp-reactor` v0.4.0).

### Examples

- The `ssh_brute_force` example now uses `on event<accu>`: after brute force is detected (count >= 10), every subsequent failed login re-fires with the running cumulative count and the accumulating evidence.

### wfgen

- `--no-wfl` skips the entire WFL pipeline (no rule load/compile, no injection) and generates pure baseline random events.
- `--no-oracle` still compiles WFL (keeping injection `use()` fixed values) and only skips oracle/expected output (no `.except.*` sidecars).
- `yield preset` declared in a rule-directory `_global.wfl` is auto-merged, so `yield <target> : <preset>` reuses common output fields.

### wfusion

- `[metrics] console_output = false` disables the periodic console stats log.

## [0.1.43]

- Version bump; aligned to `wp-reactor` v0.1.42 (includes its internal engine fixes).

## [0.1.42]

- Version bump (alpha); aligned to `wp-reactor` internal fixes; no new user-facing features.

## [0.1.41]

### Language (WFL)

- `on event seq { ... }` ordered sequences and `on event any { ... }` unordered co-occurrence: attack-chain detection with per-step `within` gaps, `not has ... within` negation steps, and `consec` strict adjacency (`skip = to_next` deferred to L3).

### wfusion

- Rule instances auto-expire by their window TTL when input is idle, instead of lingering.

## [0.1.40]

### wfusion

- `[logging] level` is the single source of truth for the log level and is no longer overridden by `RUST_LOG`; use `[logging].modules` for per-module overrides.

## [0.1.39]

### wfgen

- `--out` is now optional and decoupled from `--send` (four combinations; a clear usage error when neither is given).
- `--no-oracle` renamed to `--no-wfl` (skips the whole WFL pipeline); `--no-oracle` retained as a backward-compatible alias.

## [0.1.38]

### Language (WFL)

- Project `_global.wfl` rule prelude: declare shared `yield preset`, reuse via `yield <target> : <preset>`.
- String helpers: `sha1_n(text, n)`, `join(...)`, `join_by(sep, ...)`.
- DEBUG funnel logs and state-machine progress diagnostics for rules (locate which step a rule is stuck on).

### wfusion

- Stream windows accept `object` / `array` / `array/T` input fields; `merge()` for shallow object enrichment.

## [0.1.35-alpha] — 2026-07-22

### Language (WFL)

- `object { ... }` / `array [ ... ]` structured literals and `merge()`; structured input fields on stream windows.

### wfadm

- `wfadm self update` (incl. `self check`), installing the warp-fusion suite.

## [0.1.34-alpha] — 2026-07-20

### wfadm

- `self update` installs the full warp-fusion binaries; manifest channel selection fix.

## [0.1.32-alpha] — 2026-07-20

### wfadm

- `self update` picks the remote manifest URL by channel (`alpha` / `beta`), avoiding reading another branch's manifest.

## [0.1.31-alpha] — 2026-07-19

### wfadm

- `self update` aligned with `wpadm`: new `--channel` / `--updates-base-url` / `--updates-root` / `--json` / `--yes` / `--dry-run` / `--force`; update source uses the manifest canonical target triple, fixing macOS arm64 short-name 404s.

## [0.1.30-alpha] — 2026-07-19

### wfusion

- Built-in `__window_miss` diagnostic window: unknown stream schema / missing stream-tag field observable via the monitor sink.

## [0.1.29-alpha] — 2026-07-14

### wfusion

- Sink groups support `wf_meta_disable` with wildmatch patterns (`__wfu_*`, etc.) to suppress metadata fields.
- Admin API reload semantics: requires-restart changes return `restart_required` instead of a 409 failure.

## [0.1.28] — 2026-07-13

### Language (WFL)

- Helpers: `now()` / `now_s()` / `now_ms()` / `now_us()` / `now_ns()`, `is_blank()` / `null_if_blank()` / `default_if_blank()`, `md5()` / `sha1()` / `sha256()` / `hex()` / `stable_id()`.
- Source-aware WFL parse/compile diagnostics (file, line/column, source snippet on error).

### wfusion

- `.wfs` uses `window.stream_tag` as the distribution key (replacing the old `stream`); upstream carrier unified to `wp_oml_name`.

## [0.1.24] — 2026-07-09

### wfusion — Admin API publish protocol aligned with wparse (Break)

- `POST /admin/v1/reloads/model` params now `wait` / `update` / `version` / `group` / `timeout_ms` / `reason`; old `full` / `update_remote` semantics removed.
- Non-loopback `admin_api.bind` must enable TLS; `admin_api.auth.mode` accepts only `bearer_token`.

## [0.1.23] — 2026-07-08

### wfusion / wfadm

- daemon supports `update=true` remote update (git fetch + version resolution + managed-dir sync) followed by hot reload, with automatic rollback on failure.
- `wfadm conf update` delegates to the remote-update API.

## [0.1.22] — 2026-07-07

### wfusion / wfgen

- Fixed: non-loopback admin API can start with TLS disabled.
- `wfgen --version`.

## [0.1.21] — 2026-07-06

### wfusion — path base change (Break)

- Relative paths are now resolved against the **working directory**, not the config file's directory. Relative paths in `wfusion.toml` (`schemas` / `rules` / `sinks` / `sources_dir`, etc.) must drop one `..` level; `business.d/*.toml` `base` paths likewise. An explicit `--work-dir` takes priority.

## [0.1.17] — 2026-07-01

### wfusion

- Online hot reload via the admin API: `POST /admin/v1/reloads/model` (L1 rule swap / L2 add window / L3 partial rebuild / L4 restart).
- **Break**: `wfusion config` subcommand removed (moved to `wfadm config`).

### wfadm

- `conf update` (remote rule-source sync), `init --repo` (project bootstrap from a remote template).

## [0.1.16] — 2026-06-28

### wfusion

- **Break**: `wfusion run` split into `wfusion daemon` / `wfusion batch`; `mode` is set by the CLI, not the config.
- **Break**: `wfusion rule` removed (duplicated `wfl`).
- Admin API HTTP server (`GET /admin/v1/runtime/status`).

### wfadm

- `check` (deep WFL/WFS/WFG validation), `conf diff`, `engine status` / `engine reload`, `self-update`.

## [0.1.11] — 2026-06-21

### wfgen

- Send data via `wp-core-connectors` TcpArrowSink (RFC6587 framing, matching wfusion `tcp_src`).
