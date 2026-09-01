# match_let_demo — let 派生字段复用 + case 模式匹配表达式（issue #79）

演示两条 WFL 新语法（2026-08-31，issue #79）：

1. **`let` 派生字段复用**：一条规则内多处输出的复杂逻辑一处定义、链式引用。
2. **`case` 模式匹配表达式**：枚举值归一化，替代多层 `if/else`（值分派；规则级 `match<...>` 保留给 CEP 事件匹配）。

## 规则

`rules/match_let_demo.wfl`：

```wfl
rule derive_and_map {
    events { s : security_events }
    let tenant_id = s.tenant_id
    let dedup_key = join_by("|", tenant_id, s.log_type, s.occur_time)
    let alert_id = join_by("", "alert_", substr(sha256(dedup_key), 0, 24))
    match<s.tenant_id:10m> {
        on event { s | count >= 1; }
    } -> score(50.0)
    entity(chars, tenant_id)
    yield security_alerts (
        tenant_id = tenant_id,
        dedup_key = dedup_key,
        alert_id = alert_id,
        severity = case s.severity {
            "emerg" | "alert" | "crit" => "CRITICAL",
            "error" => "HIGH",
            "warning" => "MEDIUM",
            "notice" => "LOW",
            "info" | "debug" => "INFO",
            _ => s.severity,
        },
        sip = s.sip,
        alert_type = "sdm_alert",
        detail = "let-derived dedup/alert_id + case severity mapping"
    )
    ...
}
```

### let 派生字段

- `dedup_key` 复用 `tenant_id`，`alert_id` 复用 `dedup_key`——三段依赖链，逻辑一处定义。
- 求值路径：match（CEP）规则在事件匹配后逐事件求值注入，`entity`/`yield` 按裸名引用。
- 等价展开（不推荐）：`alert_id` 需要把 `join_by("|", ...)` 整段再抄一遍。

### case 表达式

- `|` 表示同一分支多个模式；`_` 为默认分支。
- 短路求值：按书写顺序比较，命中即返回。
- 比较语义与 `in` 列表一致；模式可以是字面量或任意值表达式。

## 数据

`data/security_events.ndjson` 4 条事件，severity 覆盖 `crit / error / warning / info`
（`crit | alert | emerg` → CRITICAL、`error` → HIGH、`warning` → MEDIUM、
`info | debug` → INFO）。

## 运行

```bash
./run.sh            # debug 构建（lint + 内联测试 + batch 回放 + 断言）
./run.sh release    # release 构建
```

期望：4 条告警，每条含 `dedup_key`（`t1|login|2026-01-01T00:00:01Z`）、
`alert_id`（`alert_` + sha256 截断 24 hex）、归一化后的 `severity`（各档一个）。

## 验证覆盖

- `wfl lint`：新语法通过 checker（match 分支类型检查、let 字段作用域）。
- `wfl test`：内联测试（3 行输入 → 3 命中）。
- `wfusion batch`：完整引擎链路（解析 → 编译 → match 路径 apply_lets → yield 求值）。
- 对应引擎/语言单测见 wp-reactor（`execute_match_applies_lets_before_alert_build`、
  `execute_each_match_expr_yield`、`match_expr_references_let_bindings` 等）。
