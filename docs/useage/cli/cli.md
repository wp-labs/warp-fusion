# CLI 命令参考

> Admin API 的状态查询、在线 reload 和发布操作详见 [`admin_api.md`](admin_api.md)。

## `wfusion` — 统一入口

```bash
wfusion <subcommand>
```

### `wfusion run` — 启动引擎

```bash
wfusion run -c ./wfusion.toml
```

| 参数 | 说明 |
|------|------|
| `-c, --config` | 配置文件路径（默认 `conf/wfusion.toml`） |
| `--overlay` | 叠加配置文件（可重复） |
| `--var` | 覆盖变量 `KEY=VALUE`（可重复） |
| `--work-dir` | 运行时工作目录 |
| `--metrics` | 启用运行时指标 |
| `--metrics-interval` | 指标上报间隔 |
| `--metrics-listen` | 指标监听地址 |

### `wfusion config` — 配置检查

```bash
# 渲染完整配置（合并 overlay + 变量展开）
wfusion config render -c wfusion.toml [--raw]

# 查看每个配置项的来源文件
wfusion config origins -c wfusion.toml [--path-prefix runtime]

# 查看所有变量的值和来源
wfusion config vars -c wfusion.toml [--var-prefix WORK_]

# 比较两个配置的差异
wfusion config diff -c wfusion.toml --to-config other.toml [--expanded]
```

## `wfgen` — 场景生成（独立工具）

```bash
# 从 .wfg 场景文件生成测试数据
wfgen gen --scenario test.wfg --out /tmp/out

# 校验场景文件
wfgen lint test.wfg

# 对比实际告警与 Oracle 期望
wfgen verify --expected oracle.jsonl --actual alerts.jsonl

# 发送生成事件到引擎（TCP + Arrow IPC）
wfgen send --scenario test.wfg --input events.jsonl

# 压测生成吞吐
wfgen bench --scenario test.wfg
```

### `wfusion rule` — 规则工具

```bash
# 解释编译后的规则（渲染 match plan）
wfusion rule explain --file rules/test.wfl

# Lint 检查
wfusion rule lint --file rules/test.wfl

# 格式化规则文件
wfusion rule fmt rules/*.wfl [--write] [--check]

# 离线回放
wfusion rule replay --file rules/test.wfl --input data/events.ndjson

# 回放 + 验证（对比 Oracle）
wfusion rule verify --file rules/test.wfl --case mycase

# 运行合约测试
wfusion rule test --file rules/test.wfl [--shuffle] [--runs 10]
```

---

## `wfl` — 规则开发（独立工具）

```bash
# 内联测试
wfl test rules/test.wfl --schemas "schemas/*.wfs"

# 离线回放
wfl replay rules/test.wfl --input data/events.ndjson

# 格式化
wfl fmt rules/*.wfl --write

# Lint 检查
wfl lint rules/test.wfl --schemas "schemas/*.wfs"

# 解释编译结果
wfl explain rules/test.wfl --schemas "schemas/*.wfs"

# 回放 + 验证
wfl verify rules/test.wfl --case mycase [--score-tolerance 0.1] [--time-tolerance 5]

# 合约测试
wfl test rules/test.wfl --schemas "schemas/*.wfs" [--runs 100]
```

---

## `wfgen` — 场景生成（独立工具）

```bash
# 从 .wfg 生成测试数据
wfgen gen --scenario test.wfg --out /tmp/out

# 校验场景
wfgen lint --scenario test.wfg

# 验证告警
wfgen verify --expected oracle.jsonl --actual alerts.jsonl

# 发送到引擎
wfgen send --scenario test.wfg --input events.jsonl

# 压测
wfgen bench --scenario test.wfg [--duration 30s]
```
