# warp-fusion

`warp-fusion` 是 WarpFusion 的 CLI / 工具 workspace，负责产出：

- `wfusion`
- `wfgen`
- `wfl`

变更记录见 [CHANGELOG.md](./CHANGELOG.md)。

运行、配置和 Admin API 使用文档见 [docs](./docs/)，其中 Admin API
状态查询、在线 reload 和发布流程见 [docs/useage/cli/admin_api.md](./docs/useage/cli/admin_api.md)。

核心运行时库来自 `https://github.com/wp-labs/wp-reactor.git` 的 `v0.1.3` tag，
这里通过 git dependency 复用 `wf-engine`、`wf-config`、`wf-lang`、`wf-core`
和 `wf-vars` 等 crate。

## Workspace 结构

```text
warp-fusion/
├── Cargo.toml
├── src/main.rs        # wfusion 二进制入口
└── crates/
    ├── wfgen/         # 测试数据生成工具
    └── wfl/           # 规则开发工具
```

## 常用命令

构建全部 CLI：

```bash
cargo build --manifest-path Cargo.toml
```

运行 `wfusion`：

```bash
cargo run --manifest-path Cargo.toml --bin wfusion -- --help
```

运行 `wfgen`：

```bash
cargo run --manifest-path Cargo.toml -p wfgen -- --help
```

运行 `wfl`：

```bash
cargo run --manifest-path Cargo.toml -p wfl -- --help
```

测试：

```bash
cargo test --manifest-path Cargo.toml
```

## wfgen 基准工具链（nexmark_pk）

nexmark_pk 基准的三段式工具（`wfgen` 子命令），全部 Rust 实现：

| 子命令 | 作用 | 说明 |
|---|---|---|
| `gen-nexmark <count> [--seed N] [--no-sort]` | 生成 NEXMark 事件 JSONL | **默认按事件时间排序**（30s 桶序，内存有界）：批次事件时间跨度从 phase-major 的 ~24min 降到几秒，让 `over=10m` 时间驱逐恢复（旧版窗口持全量、RSS 20GB+）。事件集合与 phase-major 版逐字节一致（仅输出顺序不同），`--no-sort` 保留旧行为 |
| `dump-frames` | JSONL → 预编码 Arrow 帧（`[ASCII长度][空格][payload]`） | rayon **并行解析** JSONL（23GB 级），顺序保持；帧缓存带 `DATA_VER` 指纹 |
| `diff <a> <b> [--detail]` | **分层文件比对** | L1 流式哈希判相同（O(N)，10M 行 ~2s）→ L2 Myers 精确差异比例（git diff 同款；行数差 >10% 自动降级排序归并防退化）→ L3 `--detail` 差异行定位。退出码 0=相同/1=不同，供脚本判定 |
| `verify-nexmark <count> [--seed N]` | **Q2-Q21 ground-truth 模拟器**（Rust） | 输出各规则期望 EMIT（JSON），key 与旧 Python 版 `verify_ground_truth.py` 一致；**按 auction 分片并行**（watermark 全局预计算，结果与分片数无关）：10M ~3s / 30M ~10s / 100M ~44s（Python 版 3.5min / 10min+ / >1h）。`WFGEN_VERIFY_SHARDS=N` 可调分片数 |

### verify-nexmark 的逻辑与边界

模拟器**复用 `gen-nexmark` 的事件生成器**（同一 rng 序列、同一 30s 桶序，跳过 JSONL 中间产物），逐事件镜像 wf-engine 的规则执行语义：滑动窗口 `match<key:10m>` 懒过期堆、per-key pending 去重（引擎 `push_expiry_candidate` 的 dedup）、`on event` fire+reset / `on event<accu>` rearm、watermark 单调推进、q16 固定 10m 桶、q21 anti join。

**并行化**：q5/q6/q7/q15-q20 的状态按 auction 隔离，按 `auc % N` 分片并行（person/auction 事件无状态直通）；sweep 的 watermark 不能取分片局部值（会滞后导致过期漂移），而是在 30s 桶序扫描时**全局预计算**每事件的前缀 max ns（只含 bid 事件，与原版 `if ns > watermark` 语义一致），各分片用同一时间线 → 输出与分片数无关（1/3/7/16 分片验证一致）。

**定位**：它是引擎行为的**回归金丝雀**（引擎改动后行为漂移会暴露），不是独立规范 oracle——它与引擎共享实现假设（若引擎语义有 bug，模拟器会镜像同样的 bug）。已知边界：

- **q16**：模拟器给**理想值**（固定桶全部 close），引擎受 `MAX_EXPIRY_SCAN_BUDGET` 丢早桶，EMIT 更低（10m：381,156 vs 理想 628,923）；
- **q21**：模拟器给 naive 0（假设 bidder 都在 person 窗口），引擎实际保留少量（10m：33,231）；
- **q1/q11/q12/q14/q22**：未建模（on-each / per-shard 会话 / 全局 top-N conv / asof join），靠端到端 `[clean]` + 确定性验证。

正确性锚点：10M 输出与 Python 版**逐 key 一致**；30M 与既有 ground truth 吻合（q2=224,289 / q3=1,800,000 / q7=10,350,961）。
