# warp-fusion

`warp-fusion` 是 WarpFusion 的 CLI / 工具 workspace，负责产出：

- `wfusion`
- `wfgen`
- `wfl`

变更记录见 [CHANGELOG.md](./CHANGELOG.md)。

核心运行时库仍位于相邻仓库 `../wp-reactor`，这里通过 path dependency 复用 `wf-engine`、`wf-runtime`、`wf-config`、`wf-lang` 等 crate。

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
cargo run --manifest-path Cargo.toml --bin wfusion -- run --config ../wp-reactor/examples/wfusion.toml
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
