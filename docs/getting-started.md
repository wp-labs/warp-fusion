# 快速开始

## 安装

```bash
git clone https://github.com/wp-labs/warp-fusion.git
cd warp-fusion
cargo build --release
```

编译产物：
- `target/release/wfusion` — 统一入口（引擎 + 规则工具）
- `target/release/wfgen` — 场景生成（可选）
- `target/release/wfl` — 规则开发（可选）

## 第一个示例

以 `port_scan_whitelist` 为例：

```bash
cd examples/port_scan_whitelist

# 1. 内联测试 —— 验证规则逻辑
wfl test rules/port_scan_whitelist.wfl --schemas "schemas/*.wfs"

# 2. 离线回放 —— 用历史数据验证
wfl replay rules/port_scan_whitelist.wfl --input data/conn_events.ndjson

# 3. 引擎运行 —— 完整管道
wfusion run -c ./wfusion.toml
```

## 目录结构

```
warp-fusion/
├── wfusion.toml           # 主配置
├── rules/                 # .wfl 规则文件
├── schemas/               # .wfs schema 文件
├── sinks/                 # sink 配置
│   ├── infra.d/           #   基础设施 sink（default/error/monitor）
│   ├── business.d/        #   业务路由 sink
│   ├── connectors/        #   connector 定义
│   └── defaults.toml      #   全局 sink 默认值
├── data/                  # 离线回放数据
├── out/                   # 输出目录
├── examples/              # 检测场景示例
└── docs/                  # 文档
```

## 示例

| 示例 | 检测场景 | 核心模式 |
|------|---------|---------|
| `port_scan_whitelist/` | 端口扫描 + 白名单 | distinct + count + join anti |
| `ssh_brute_force/` | SSH 暴力破解 | count 阈值 + 多目标 |
| `sqli_probe/` | SQL 注入探测 | regex_match + count |
| `rat_propagation/` | 远控扩散 | 多步 scan→login→xfer |

详见 [`examples/README.md`](../examples/README.md)。

## 文档索引

| 文档 | 内容 |
|------|------|
| [`configuration.md`](configuration.md) | `wfusion.toml` 完整配置参考 |
| [`wparse-window-routing.md`](wparse-window-routing.md) | `warp-parse` 输出如何分发到 window |
| [`schema.md`](schema.md) | `.wfs` Schema 定义 |
| [`rules.md`](rules.md) | `.wfl` 规则编写 |
| [`cli.md`](cli.md) | CLI 命令参考 |
| [`monitoring-design.md`](monitoring-design.md) | 监控方案设计 |
