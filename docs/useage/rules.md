# WFL 规则

`.wfl` 文件用于声明检测规则、规则输出以及规则内联测试。运行时通过
`wfusion.toml` 的 `[runtime].rules` glob 加载规则文件。

## case 模式匹配表达式

枚举值归一化时，多层 `if/else` 可用 `case` 表达式替代（issue #79 Issue 2）。
`case` 是**值分派表达式**，与规则级 CEP 子句 `match<keys:window> { ... }`
（事件模式匹配触发）区分——`match` 留给事件匹配，`case` 做枚举归一化：

```wfl
severity = case s.severity {
    "emerg" | "alert" | "crit" => "CRITICAL",
    "error" => "HIGH",
    "warning" => "MEDIUM",
    "notice" => "LOW",
    "info" | "debug" => "INFO",
    _ => s.severity,
}
```

语义约定：

- 语法：`case <subject> { <pattern> [ | <pattern> ...] => <value>, ..., [ _ => <default>, ] }`。
- **多模式**：`|` 表示同一分支匹配多个值（分支内任一命中即取该分支值）。
- **默认分支**：`_ => <expr>` 兜底未匹配值；无 `_` 且全部未命中 → 求值 None
  （yield 中回退空串，与字段缺失同语义）。
- **短路**：按书写顺序逐分支比较，命中即返回，不继续求值后续分支/模式。
- **比较语义**：与 `in` 列表一致——数字按值比较、字符串/布尔按相等比较
  （`values_equal`）。模式可以是字面量，也可以是字段/函数表达式（求值后比较）。
- 分支值可以是任意表达式（字段引用/函数调用/嵌套 case/if）。
- `|` 在模式中是**多模式分隔符**（不是逻辑或）；分支间以 `,` 分隔，允许尾逗号。
- 类型检查宽松：subject/模式/分支值递归检查字段引用，分支类型不强制统一
  （引擎求值 None 兜底）。
- 性能：case 表达式走行式求值（列式 gate 不识别，yield cell 自动回落解释器）；
  枚举归一化场景若在热路径且可接受，也可用 `in` + `if/else` 保持列式。

## let 派生字段

一条规则内多个输出字段依赖同一段复杂逻辑时，可以用 `let <name> = <expr>`
声明**只读派生字段**（在 `events` 块之后、`match`/`on each`/`stats` 之前），
同规则内后续表达式按裸名引用（issue #79）：

```wfl
rule sdm_alert {
    events { s : auth_events }
    let tenant = s.tenant_id
    let dedup_key = join_by("|", tenant, s.log_type, s.occur_time)
    let alert_id = join_by("", "alert_", substr(sha256(dedup_key), 0, 24))
    match<s.tenant_id:10m> {
        on event { s | count >= 1; }
    } -> score(50.0)
    entity(chars, tenant)
    yield out (
        tenant_id = tenant,
        dedup_key = dedup_key,
        alert_id = alert_id
    )
}
```

> 注：per-event 上下文取事件字段直接写 `s.tenant_id`——`first()` 是收集类函数
> （依赖实例收集的事件集合），per-event 求值返回 None。

语义约定：

- `let` 绑定在**规则内**按**声明顺序**求值一次（每次输出上下文：每事件/每次
  match/close），后声明的 `let` 可以引用先声明的（链式派生）。
- 引用方式为裸名（`dedup_key`），与字段引用同解析优先级——`let` 名在
  checker 的作用域中注册，yield/where/score/entity 均可引用。
- `let` 求值失败（如引用的字段缺失）→ 不注入 → 后续引用读到空/缺省，与
  事件字段缺失语义一致。
- 求值路径：`on each`、`match`（on-event）、deferred join（`emit at`）、
  `close`（2026-08-31 issue #79 补齐 match/close）；`close` 上下文无触发事件，
  引用窗口聚合字段（`close_ctx_fields`）的 `let` 有值，引用事件字段的求值为空。
- **stats 规则**（`stats<...>` 声明式窗口统计）暂不支持 `let`——checker 显式
  报错（stats 未接入 per-event let 求值）。
- 列式路径（on-each 批量 emit）在编译期内联展开 `let` 引用（`inline_lets`），
  与解释路径逐行注入语义等价；含 `let` 的 match/close 规则暂回落行式求值
  （正确性优先，列式内联为后续优化）。

## 字符串 helper

WFL 提供几个常用字符串 helper，适合在 `yield` 中生成稳定字段：

```wfl
hash8 = sha1_n(@__wfu_id, 8)
joined = join(s.tenant_id, "function_demo", s.empty_part, s.target_host)
joined_by = join_by("|", s.tenant_id, "function_demo", s.empty_part, s.target_host)
```

语义约定：

- `sha1_n(text, length)` 返回 `sha1(text)` 的前 `length` 位小写 hex；`length` 必须是 `1..=40` 的整数。
- `join(value, ...)` 按参数顺序直接拼接，不加分隔符。
- `join_by(separator, value, ...)` 按参数顺序拼接，并在字段之间插入显式分隔符。
- `join` / `join_by` 不 trim、不改大小写、不转义 `%`、不转义 `|`，空字符串按原样参与拼接，取不到的参数按空字符串片段处理。
- `join` / `join_by` 参数支持标量值：`chars`、`digit`、`float`、`bool`、`time`、`ip`、`hex`。

例如：

```wfl
join("tenant|A", "function_demo", "", "host%01")
// tenant|Afunction_demohost%01

join_by("|", "tenant|A", "function_demo", "", "host%01")
// tenant|A|function_demo||host%01
```

## 公共 yield preset

当多条规则需要输出相同字段时，可以把公共输出逻辑放在规则目录下的
`_global.wfl` 中：

```wfl
yield preset base_alerts (
    rule_name = @__wfu_rule_name
)
```

普通规则通过 `yield <window> : <preset>` 继承这个 preset，再补充规则自己的字段：

```wfl
rule scan_detect {
    from e in conn_events
    match {
        close { e | count >= 50; }
    } -> score(70.0)
    entity(ip, e.sip)
    yield scan_alerts : base_alerts (
        sip = e.sip,
        alert_type = "scanner",
        detail = ">=50 req in 5min"
    )
}
```

语义约定：

- `_global.wfl` 是项目级规则 prelude，放在 `[runtime].rules` 所在的规则目录中。
- `_global.wfl` 会在普通规则文件之前加载，供普通规则引用其中的 `yield preset`。
- `_global.wfl` 不作为普通规则文件编译；即使它被 `*.wfl` glob 匹配，也不会产生规则。
- `_global.wfl` 只应声明 `yield preset`，不要放 `rule`。
- 一个 `yield` 可以引用多个 preset：`yield out : base, severity (...)`。
- 多个 preset 按引用顺序合并，后面的同名字段覆盖前面的同名字段。
- 普通规则 `yield (...)` 中的显式字段最后合并，因此可以覆盖 preset 中的同名字段。
- `_global.wfl` 和普通规则文件中不能定义同名 `yield preset`。
- 如果规则目录下只有 `_global.wfl`，运行时会得到 0 条规则；这是合法状态。

适合放入 `_global.wfl` 的内容包括统一的 `rule_name`、告警版本、租户标识、默认时间字段或其他每条告警都要带的字段。

### 参数化 yield preset 设计草案

> 状态：设计草案，当前版本尚未实现。

当公共输出字段需要由规则调用方传入少量差异化值时，可以扩展 `yield preset`
支持尖括号参数列表：

```wfl
yield preset base_alerts <
    severity,
    source = "wfusion"
> (
    rule_name = @__wfu_rule_name,
    severity = $severity,
    source = $source
)
```

普通规则引用 preset 时在 preset 名后传入实参：

```wfl
rule ssh_brute_force {
    from e in auth_events
    match {
        close { e | count >= 5; }
    } -> score(90.0)
    entity(ip, e.sip)
    yield security_alerts : base_alerts<"high"> (
        entity_id = e.sip,
        alert_type = "ssh_brute_force"
    )
}
```

语义约定：

- `yield preset name <...> (...)` 中，`<...>` 是可选参数列表，`(...)` 仍是 preset body。
- 没有默认值的参数必填；带默认值的参数可省略。
- 参数默认值是普通 WFL 表达式，例如 `"wfusion"`、`@__wfu_rule_name` 或字段引用。
- preset body 内通过 `$param_name` 引用参数值，参数只在当前 preset body 内有效。
- preset 引用使用相同的尖括号语法：`yield out : base_alerts<"high", "wfusion"> (...)`。
- 实参按位置绑定到参数；省略尾部带默认值参数时使用默认表达式。
- 展开参数后再执行现有字段合并规则：后引用的 preset 覆盖先引用的 preset，普通 `yield (...)` 的显式字段最后覆盖 preset 字段。
- 参数名应避免与 WFL 字段名混淆；参数引用必须带 `$` 前缀。

建议的错误诊断：

- 缺少必填参数：`yield preset base_alerts missing required argument severity`。
- 实参数量过多：`yield preset base_alerts expects 1..2 arguments, got 3`。
- preset body 引用了未声明参数：`unknown yield preset parameter $severity`。
- 参数默认值或实参展开后类型不匹配目标 `yield` 字段时，沿用现有 yield 类型检查错误。

## on event seq / on event any —— 序列与共现

`on event` 的 `seq` / `any` 修饰符声明步骤的**排序模式**。默认（裸 `on event`）即 `seq`，
向后兼容。

### `on event seq` — 有序序列（攻击链）

表达"先 A 后 B、B 在 A 后 X 内"。运行时顺序语义（step i+1 只在 step i 完成后评估）由引擎
保证；`seq` 在此基础上补充步间时间约束（`within`）、否定步（`not`）和严格相邻（`consec`）。

```wfl
match<sip,dip:30m> {
    on event seq {
        has scan;                    // 存在性步骤：scan 事件至少一次
        has login within 10m;        // login 必须在 scan 完成后的 10m 内
        has xfer;                    // 总跨度由 match 窗口时长（30m）约束
        not has failed within 5m;    // 否定步：scan 后 5m 内不得出现失败登录
    }
} -> score(95.0)
```

- **存在性步骤**：`has <alias>`，等价 `count >= 1`，首个匹配事件到达即完成。
- **聚合步骤**：复用 pipe 语法，如 `spray.user | distinct | count >= 5`，聚合条件首次满足时完成。
- **`within <dur>`**：本步完成时刻 − 上一步完成时刻 ≤ `dur`（首个步骤相对 match 窗口起点）。
- **`not has <alias> within <dur>`**：否定步，自上一完成步骤起 `dur` 内不得出现匹配事件。
- **`consec`**：严格相邻修饰符（默认允许步骤间夹带无关事件）。
- **`skip = to_next`**：重叠匹配（L3，暂未实现）。

### `on event any` — 无序共现

所有步骤**并行评估**，全部满足即触发，**顺序无关**：

```wfl
match<sip,dip:30m> {
    on event any {
        scan | count >= 1;
        login | count >= 1;
        xfer | count >= 1;
    }
} -> score(80.0)
```

`any` 模式下 `login → scan → xfer` 的乱序序列也触发（弱相关性检测）。`any` 步骤不支持
`within` / `not` / `consec` / `skip`（它们依赖顺序，编译期拒绝）。

### 语义约定（seq）

- 步骤按书写顺序完成：login 先于 scan 到达会被 step 0 消费，不保留给 step 1。
- `within` 超限 / 否定违反 → 本次匹配作废并重置。
- 部分匹配（未完成的链）TTL = match 窗口时长，由 evictor 清理。
- 设计与决策点见 [wfl_seq_design.md](../design/wfl_seq_design.md)。
