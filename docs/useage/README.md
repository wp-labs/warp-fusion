# WarpFusion 使用文档

本目录存放面向使用者的文档：安装运行、规则编写、Schema、配置、CLI、Admin API，以及与 warp-parse 的接入说明。设计和协议取舍文档仍保留在 `../design/`。

## 快速入口

| 文档 | 内容 |
|------|------|
| [getting-started.md](./getting-started.md) | 快速开始、示例项目结构、文档索引 |
| [configuration.md](./configuration.md) | `wfusion.toml`、source / sink / runtime 配置入口 |
| [schema.md](./schema.md) | `.wfs` window 和字段类型 |
| [rules.md](./rules.md) | `.wfl` 规则编写、yield 时间变量、稳定统计上下文 |
| [wparse-window-routing.md](./wparse-window-routing.md) | warp-parse 输出如何分发到 WarpFusion window |
| [cli/cli.md](./cli/cli.md) | CLI 命令参考 |
| [cli/admin_api.md](./cli/admin_api.md) | Admin API、状态查询、在线 reload / 发布 |

## 配置参考

| 文档 | 内容 |
|------|------|
| [config/runtime.md](./config/runtime.md) | 运行模式和 runtime 参数 |
| [config/source.md](./config/source.md) | TCP / file source、`stream_tag`、`stream_tag_field` |
| [config/window.md](./config/window.md) | window 默认值、内存和时间窗口配置 |
| [config/sink.md](./config/sink.md) | sink 路由、connector、`wf_meta_disable` |
| [config/metrics.md](./config/metrics.md) | metrics 配置 |
| [config/logging.md](./config/logging.md) | logging 配置 |
| [config/knowdb.md](./config/knowdb.md) | knowdb / provider window 配置 |

## 推荐阅读顺序

1. 先读 [getting-started.md](./getting-started.md)，跑通一个最小示例。
2. 读 [schema.md](./schema.md) 和 [rules.md](./rules.md)，理解 `.wfs` / `.wfl` 的职责边界。
3. 接入真实数据源时读 [configuration.md](./configuration.md)、[config/source.md](./config/source.md) 和 [config/sink.md](./config/sink.md)。
4. 与 warp-parse 联动时读 [wparse-window-routing.md](./wparse-window-routing.md)。
5. 需要在线 reload 或发布时读 [cli/admin_api.md](./cli/admin_api.md)。
