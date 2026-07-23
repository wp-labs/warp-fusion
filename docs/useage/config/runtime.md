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

## 规则目录的 `_global.wfl`

当 `[runtime].rules` 指向一组 `.wfl` 文件时，运行时会在规则目录下约定查找
`_global.wfl`。如果文件存在，它会作为项目级 WFL prelude 先加载，用来定义所有规则都
可以复用的 `yield preset`：

```toml
[runtime]
rules = "../../models/wfl/*.wfl"
```

对应目录：

```text
models/wfl/
├── _global.wfl
├── 01_scan_detect.wfl
└── 02_traffic_spike.wfl
```

`_global.wfl` 示例：

```wfl
yield preset base_alerts (
    rule_name = @__wfu_rule_name
)
```

普通规则引用：

```wfl
yield scan_alerts : base_alerts (
    sip = e.sip,
    alert_type = "scanner"
)
```

注意：

- `_global.wfl` 只用于声明 `yield preset`，不作为普通规则编译。
- `*.wfl` 可以匹配到 `_global.wfl`，运行时会自动排除它的普通规则编译结果。
- 一个规则可以引用多个 preset：`yield out : base, severity (...)`；后引用的 preset
  会覆盖先引用 preset 中的同名字段，普通 `yield (...)` 中的显式字段最后合并。
- `_global.wfl` 和普通规则文件中不能定义同名 `yield preset`。
- 如果规则目录只有 `_global.wfl`，运行时返回 0 条规则。
