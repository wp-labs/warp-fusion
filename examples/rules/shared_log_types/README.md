# shared_log_types — 顶层列表 + `use` 导入（issue #73）

演示 WFL 的**跨规则列表复用**：一组 `log_type` 允许列表只定义一次，告警/实体/证据三条规则分别 `use` 导入、以 `in` / `not in` 引用同一份数据。

## 要解决的问题

编写告警、实体、证据等关联规则时，多个规则通常需要完全相同的日志类型允许列表。传统写法是每个规则里复制粘贴同一组 `s.log_type in ("a", "b", ...)`：

- 新增/删除日志类型要同步改多个位置
- 任意一个规则漏改、拼写错误或列表不一致，告警/实体/证据输出就不一致

## 本示例的写法

工程布局遵循 wp-pipeline 标准：`models/` 放模型（规则/schema/列表/窗口），`wfusion/` 放引擎配置与拓扑（配置相对路径以 `wfusion/` 为基准）：

```
examples/rules/shared_log_types/
├── models/
│   ├── wfl/
│   │   ├── alert_rule.wfl                # use + `in` → 告警
│   │   ├── alert_entity_rule.wfl         # use + `in` → 实体（按 src_ip）
│   │   ├── event_evidence_rule.wfl       # use + `not in` → 证据
│   │   └── shared/security_log_types.wfl # ★ 列表只定义一处
│   ├── schemas/sdm.wfs
│   └── windows.toml
├── wfusion/
│   ├── conf/wfusion.toml
│   └── topology/                         # source + 3 个业务 sink
├── data/sdm_events.ndjson                # 8 条事件（5 安全 + 3 普通）
└── run.sh                                # 一键：lint + test + batch
```

**列表文件**（`models/wfl/shared/security_log_types.wfl`）——顶层裸绑定，无关键字、无可见性控制（WFL 规模小，`use` 导入的文件其全部顶层列表都可见）：

```wfl
security_log_types = (
    "edr_alert_log",
    "fw_ips_protect_log",
    "topas_waf_virus",
    "ngsoc_threat_alert_send"
)
```

**规则文件**（`models/wfl/alert_rule.wfl`）——`use` 导入后直接引用（`use` 相对路径以规则文件所在目录为基准）：

```wfl
use "shared/security_log_types.wfl"

rule alert_rule {
    events { s : sdm_event && s.log_type in security_log_types }
    match<:1m> { on event { s | count >= 1; } } -> score(80.0)
    entity(ip, s.src_ip)
    yield security_alerts (
        log_type = s.log_type,
        src_ip = s.src_ip,
        detail = fmt("security log: {}", s.log_type)
    )
}
```

`alert_entity_rule`（`in` + 按 src_ip 聚合）、`event_evidence_rule`（`not in`）引用**同一份**列表——改 `models/wfl/shared/security_log_types.wfl` 一处，三条规则同时生效。

## 语言能力（issue #73）

| 能力 | 说明 |
|------|------|
| 顶层列表声明 | `name = ("a", "b", ...)` 裸绑定（在规则之前） |
| `use "file.wfl"` | **include 语义**：目标文件的全部顶层列表并入当前作用域（flatten、无限定名；递归传播、相对路径） |
| `expr in <name>` / `not in <name>` | 编译期展开为字面列表，与手写 `in (...)` 逐字节等价 |
| 类型检查 | 列表元素推断同类型；元素混合类型、`in` 左值类型与元素不兼容 → 编译报错（字面与命名列表统一路径） |
| 错误面 | 未知名 / use 目标缺失 / 循环引用 / 重名 → 可定位报错 |

## 运行

```bash
# 一键（lint + test + batch + 结果校验）
./run.sh

# 分步
wfl lint models/wfl/alert_rule.wfl -s "models/schemas/*.wfs"
wfl test models/wfl/alert_rule.wfl -s "models/schemas/*.wfs"
(cd wfusion && wfusion batch -c conf/wfusion.toml)
```

需要先构建：`cargo build -p wfl -p wfusion`（或 `run_all.sh release` 用 release 二进制）。

## 预期结果

`data/sdm_events.ndjson` 8 条事件——5 条命中安全日志列表（`edr_alert_log` ×2、`fw_ips_protect_log`、`topas_waf_virus`、`ngsoc_threat_alert_send`）、3 条普通日志（`app_access_log`、`db_query_log`、`mail_smtp_log`）：

| 输出 | 规则 | 条件 | 预期 |
|------|------|------|------|
| `security_alerts.ndjson` | alert_rule | `in` 列表 | **5** 条 |
| `alert_entities.ndjson` | alert_entity_rule | `in` 列表（按 src_ip） | **5** 条 |
| `event_evidence.ndjson` | event_evidence_rule | `not in` 列表 | **3** 条 |

> 修改 `models/wfl/shared/security_log_types.wfl` 增删日志类型 → 三个输出同时变化，无需编辑任何规则文件。
