# 基础配置

## 运行模式

```toml
mode = "batch"   # batch（批处理） | daemon（常驻服务）
```

| 模式 | 说明 |
|------|------|
| `batch` | 文件源回放完成后自动退出 |
| `daemon` | TCP 源持续监听，等待信号退出 |

## Runtime

```toml
[runtime]
executor_parallelism = 2    # 规则执行并行度
rule_exec_timeout = "30s"   # 单条规则最大执行时间
schemas = "schemas/*.wfs"   # schema 文件 glob
rules   = "rules/*.wfl"     # 规则文件 glob
sinks   = "sinks"            # sink 配置目录
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `executor_parallelism` | int | 同一规则最多同时执行的 match 数 |
| `rule_exec_timeout` | duration | 单条规则最大执行时间，超时返回错误 |
| `schemas` | glob | `.wfs` 文件匹配模式 |
| `rules` | glob | `.wfl` 文件匹配模式 |
| `sinks` | path | sink 配置目录 |
