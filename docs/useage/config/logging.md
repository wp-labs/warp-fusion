# 日志配置 (`[logging]`)

```toml
[logging]
level = "info"       # trace | debug | info | warn | error
format = "plain"     # plain | json
file = "wfusion.log"
```

| 字段 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `level` | string | `"info"` | 日志级别 |
| `format` | string | `"plain"` | `plain`（人类可读）或 `json`（结构化） |
| `file` | string | 无（仅 stderr） | 日志文件路径 |

### JSON 格式

设置 `format = "json"` 后，每行一个 JSON 对象：

```json
{"timestamp":"2026-01-01T00:00:00.000Z","level":"INFO","domain":"sys","message":"engine bootstrap complete","schemas":3,"rules":1}
```
