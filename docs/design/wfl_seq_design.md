# WFL 序列语义设计（P2）— `on event seq` / `on event any`

> 状态：**已评审确认**（P2 范围，决策点见 §8，全部 8 项已定稿），尚未实现。
>
> 范围：序列语义的 **L1（时间有序共现）+ L2（`within` / `not`）**。L3（实例级 NFA）记为后续。
>
> 相关文档：[rules.md](../useage/rules.md) · [schema.md](../useage/config/schema.md) ·
> [runtime.md](../useage/config/runtime.md) · [metrics.md](../useage/config/metrics.md)

## 1. 背景与目标

### 1.1 现状与真实缺口

**顺序语义已经存在**：`wf-engine` 的 `CepStateMachine` 用 `current_step` 顺序推进——step 0
满足才评估 step 1；login 先于 scan 到达会被 step 0 消费而不保留给 step 1（`seq_order` 测试已证实）。
所以 `on event { scan|count>=1; login|count>=1; xfer|count>=1; }` 本身就会顺序触发。

真正的缺口是顺序**之外**的语义：

1. **`within`**：步间时间 gap 不存在。scan 后 3 小时才到的 login 依然满足，无法约束
   "必须在 scan 后 10m 内"。
2. **`not`**（否定步）：无法表达"scan 之后 5m 内不得出现失败登录"。
3. **`consec`**（严格相邻）：现有引擎是 gap 语义（步骤间夹带无关事件不影响），无法要求
   "scan 的下一条必须是 login"。
4. **显式 DSL**：`has X` / `seq` 让攻击链意图自文档化，替代裸 `count >= 1`。

`tree-sitter-wfl` grammar 中的 `seq_block` / `seq_use_step` / `seq_not_step`
（`use(...) with(count, within)` / `not(...) within(...)`）**已经是完整的序列语法**，
但目前只用在 wfgen 的场景注入（`injection` 块）里，未进入运行时。本设计把这套序列概念
提升为运行时规则语言的一等构造，并落地 within/not/consec。

### 1.2 目标：三级演进

| 级别 | 语义 | 状态成本 | 解决的问题 |
|---|---|---|---|
| **L0 顺序（已有）** | 现有引擎 `current_step` 已顺序推进 | 已有 | "login 必须在 scan 之后"（已实现） |
| **L1 DSL + within** | `seq`/`has` 显式语法 + 步间 `within` gap | O(steps) per key，无实例膨胀 | 意图自文档化 + "login 须在 scan 后 10m 内" |
| **L2 not + consec** | 否定步 + 严格相邻 | O(steps) + 每 key 一个否定窗口 | "期间无失败记录"、"scan 后必须是 login" |
| **L3 实例级 NFA** | 步骤关联到具体事件实例（xfer.dip == login.dip）、量化符 | 部分匹配数随实例膨胀，需上限/TTL | 跨步骤同一实体的强关联 |

**工程判断：P2 只做 L1+L2。** L1 每步只记"首次完成时间"，无需 NFA、无组合爆炸。
L3 只在出现"步骤间必须关联到同一事件实例"的真实需求后再做。

## 2. 语法（EBNF 扩展）

### 2.1 现有相关语法（tree-sitter-wfl `grammar.js`，不变）

```ebnf
match_clause   := "match" "<" match_params ">" "{" [key_block] on_event_block [close_block] "}"
match_params   := [field_ref {"," field_ref}] ":" window_spec
window_spec    := (duration | variable) ":" "fixed"          (* tumbling *)
                | "session" "(" (duration | variable) ")"
                | duration | variable
step_branch    := [label ":"] source_expression ["&&" expression] pipe_chain
source_expression := alias ["." field | "[" string "]"]
pipe_chain     := ("|" "distinct")* "|" measure comparison_operator primary
measure        := "count" | "sum" | "avg" | "min" | "max"
```

### 2.2 新增语法

```ebnf
match_clause     := "match" "<" match_params ">" "{" [key_block] match_body "}"
match_body       := event_match | on_event_mode
event_match      := on_event_block [close_block]                      (* 现有，向后兼容 *)
on_event_mode    := "on" "event" ("seq" | "any") ["consec"] ["skip" "=" ("past_last" | "to_next")]
                    "{" seq_step+ "}"
seq_step         := ["not"] seq_step_body ";"
seq_step_body    := "has" alias ["&&" expression] ["within" duration]   (* 存在性步骤：替代 count>=1 *)
                 | step_branch ["within" duration]                      (* 聚合步骤：显式 distinct/measure *)
```

`seq` 修饰符 = 有序（+ `within`/`not`/`consec`/`skip`）；`any` 修饰符 = 无序共现
（全部满足、顺序无关，不支持 `within`/`not`/`consec`/`skip`）。裸 `on event { ... }` 等价 `seq`，
向后兼容。

约定：

- **存在性步骤**以 `has <alias>` 表达（等价于 `count >= 1`，但语义是"事件发生/存在"，
  与 L1 的 `first_seen` 实现一一对应，不隐含计数机制）。
- **聚合步骤**复用现有 `step_branch`（`<alias>.<field> | distinct | count >= N`），
  在聚合条件**首次满足**时完成。
- `within <duration>` 挂在步骤尾，表示相对上一步完成时刻的时间 gap。
- `not` 前缀把该步声明为否定步（`not has <alias> within <dur>`）。
- `consec` / `skip` 是 `seq` 块级修饰符。
- `has` 已确认为保留关键字：现有 `.wfl`/`.wfs`/`.wfg` 无同名标识符，grammar 无冲突
  （`have` 弃用，保持单一形式）。

### 2.3 与 wfgen 注入 seq 的关系

运行时 `seq` 与 wfgen 注入的 `seq_block` 语义同构，但载体不同：

| | wfgen 注入（现有） | 运行时 seq（本设计） |
|---|---|---|
| 步骤内容 | `use(field=value, ...) with(count, within)` 值断言 | 引用 `events` 块别名 + 谓词 + measure |
| 否定步 | `not(field=value, ...) within(dur)` | `not <alias> within <dur>` |
| 顺序载体 | `entity seq { ... }` 面向测试注入 | `match<key:window> { on event seq { ... } }` 面向运行时 |

两者共享时序/`within` 概念，后续可统一为一个语义内核。

**对拍结论（已确认）**：注入侧 `extract_syntax_case_overrides` 只消费 `SeqStep::Use`
（生成 `count` 个匹配谓词的事件），`SeqStep::Not` 作为约束跳过（不生成违反事件）——运行时
`seq` 的否定步因此天然满足。seq 的 use-步与 on_event 步编译为相同的 `event_steps` 结构，
注入生成器无需改动。hit / near_miss / miss 分类由运行时 seq 验证：`seq_examples` 契约测试
（`full_chain_detected` = hit；`missing_xfer_step` / `success_too_late` = 近 miss；
`spray_only` / `admin_scan_only` = miss）。完整 `wfgen → wfusion → verify` 管线需在 warp-fusion
接入本地 wp-reactor 后跑通（见 §7 跨仓库注意）。

### 2.4 实现修正（review 后）

- `not` 步骤带字段聚合（`not b.bytes | sum >= 100`）运行时按"任意匹配事件"处理，
  **checker 升级为 Error**（防止静默误报）。否定约束请用 `not has <alias> && <谓词>`。
- `skip = to_next` 延后到 L3，checker 对使用它的规则发 Warning。
- 否定窗口仅在上一完成步骤后激活；`consec` 断链重置保留否定违例标志。


## 3. 语义表

| 构造 | 语义 |
|---|---|
| `on event seq { ... }` | 有序步骤容器。步骤按书写顺序构成时序链。**默认 gap 语义**（followedBy：允许两步骤之间出现其他事件），因为真实流量必有间隔。 |
| `consec` | 严格相邻（next 语义）：两步骤之间不允许出现任何其他事件。 |
| `skip = past_last` | （默认）一次完整匹配触发后，重置该 key 的 `SeqState`（fire-and-reset）。 |
| `skip = to_next` | 保留除首步外的部分状态，允许重叠匹配。**P2 不实现**（依赖 L3 NFA）。 |
| `has scan;` | 存在性步骤（替代 `count >= 1`）：匹配 `scan` 的首个事件到达时步骤完成，记录 `first_seen`。 |
| `has scan && pred;` | 存在性步骤带内联谓词，与 `events` 块声明中的谓词取 AND。 |
| `spray.user \| distinct \| count >= 5;` | 聚合步骤：步骤在聚合条件**首次满足**时完成（`first_seen` = 满足时刻）。 |
| `has login within 10m;` | 时间 gap：本步完成时刻 − 上一步完成时刻 ∈ [0, 10m]。**负 gap（乱序完成）同样视为违反**。首个步骤的 `within` 相对 match 窗口起点。 |
| `not has failed within 5m;` | 否定步：**否定窗口仅在前一完成步骤之后激活**（该步骤完成前的事件不构成违例）；自其完成起 5m 内不得出现匹配 `failed` 的事件。违反 → 本次匹配作废并重置该 key 状态（`consec` 断链的重置**保留**违例标志）。 |
| 顺序约束 | 步骤 i+1 只能在步骤 i 完成后开始累积（login 先于 scan 到达时不会记入步骤 2）。 |
| 总跨度 | 首步 `first_seen` → 末步 `first_seen` ≤ `match_params` 的窗口时长。 |
| 触发 | 末步完成且全部约束满足 → 发射 + 重置。 |

gap vs consec 时序示例：

```
事件序列: scan → dns_query → login → xfer
- 默认 gap:  on event seq { has scan; has login; has xfer; }    dns_query 夹在中间不影响 → 匹配
- consec:  on event seq consec { has scan; has login; ... }  scan 后必须是 login，dns_query 破坏 → 不匹配
```

与现有 `match` 的对应与差异：

| 现有构造 | seq 对照 |
|---|---|
| `match<key:window>` | 不变，key 与窗口时长沿用 |
| `on event { step; }` 并行条件 | → 有序步骤，步骤按序累积 |
| `and close { ... }` | 无直接对应。seq 默认末步完成即发射（更实时）。close 变体列为后续 |
| `join ... on ...` | 保持 stage 级 join 不变；**seq 步骤内不直接支持 join**（P2 范围） |

### 3.1 `on event any` — 无序共现

`on event any { ... }` 声明无序共现：所有 step **并行评估**，每个事件对每个未满足的 step
做一次评估（累计/判阈值），**全部满足即触发**，**顺序无关**。

```wfl
match<sip,dip:10m> {
    on event any {
        scan | count >= 1;
        login | count >= 1;
        xfer | count >= 1;
    }
} -> score(80.0)
```

- 引擎侧：并行求值路径（无 `current_step` 门控），`satisfied_flags` 跳过已满足步骤。
- `login → scan → xfer` 的乱序序列同样触发（弱相关性检测）。
- `within` / `not` / `consec` / `skip` 在 `any` 模式下**编译期拒绝**（依赖顺序）。
- 触发后 fire-and-reset，同 key 可再次触发新链。

## 4. 编译映射（到现有执行链）

现有执行链（见 metrics.md）：`receiver → router(stream_tag) → window(保留事件) → rule(match plan) → alert → sink`。

```
seq 步骤编译产物（rule stage 持有，按 key）:
  RuleSpec.seq: Vec<SeqStep>
  SeqStep = {
    alias, window_id, predicate,        // 来自 events 块声明
    measure: Option<Measure>,           // 省略 = count>=1
    within: Option<Duration>,           // 步骤尾 within
    neg: bool,                          // not 步
  }

运行时（rule stage，逐事件 e，按 key 路由）:
  state: SeqState { first_seen: Vec<Option<Ts>>, neg_violated: bool, agg: Vec<AggState> }

  on_event(e):
    1. 否定步扫描：若 e 匹配某 neg 步的 window+predicate，
       且 e.time ∈ [prev_step.first_seen, prev_step.first_seen + within] → neg_violated = true
    2. 顺序累积：对第一个未完成的 use 步 s：
       - 若 e 匹配 s 的 window+predicate 且 s 聚合首次满足
         → s.first_seen = e.time（并校验 s.within 相对上一步完成时刻）
    3. 触发检查：所有 use 步 first_seen 就绪 且 首末跨度 ≤ 窗口时长 且 !neg_violated
       → emit + 重置 SeqState
```

要点：

- **顺序由"步骤 i+1 仅在步骤 i 完成后开放累积"天然保证**，无需事件级排序比较。
- **L1 只需要 first_seen，不保留原始事件**——比现有聚合 match（需保留事件做计数）内存更省。
- 乱序输入由 window 层的 `event_time` 排序与 watermark 处理兜底，seq 不感知。
- `wfusion rule explain` 需渲染步骤表（别名 / 谓词 / measure / within / neg 标记）；
- `rule_mapping.dat`（rule→window 映射）需扩展：**序列规则跨多个 window**（scan/xfer→conn_events、login→auth_events）。

## 5. 状态与内存

| 项 | 设计 |
|---|---|
| `SeqState` | 每 key O(steps) 的 `first_seen` 数组 + 聚合步骤的聚合态（与现有聚合等价）。 |
| 部分匹配 TTL | 未完成的部分匹配超过 match 窗口时长 → 清理。节奏复用现有 evictor。 |
| 上限 | 复用 `limits { max_instances; on_exceed = throttle; }`（部分匹配数 ≤ max_instances）。 |
| 新指标 | `rule.seq_partial_active`（活动部分匹配数）、`rule.seq_completed_total`、`rule.seq_reset_total`、`rule.seq_violated_total`。 |
| reload | P2 采用 `reload_state_policy = "clear"`（规则版本变化即清空 seq 状态）。`"keep"`（保留至 within 过期）列为后续。 |

## 6. 示例：改造前后对照

### 6.1 `rat_propagation` — 顺序链 + within（L2 主菜）

**改造前**（现有——顺序已由引擎 `current_step` 保证，但无 within/not，链意图不显式）：

```wfl
rule rat_propagation {
    events {
        scan  : conn_events && (dport == 22 || dport == 445 || dport == 3389) && bytes_out < 1000
        login : auth_events && result == "success"
        xfer  : conn_events && bytes_out >= 10000
    }
    match<sip,dip:30m> {
        on event {
            scan | count >= 1;
            login | count >= 1;
            xfer | count >= 1;
        }
    } -> score(95.0)
    entity(ip, scan.sip)
    yield security_alerts (
        sip = scan.sip,
        dip = scan.dip,
        alert_type = "rat_propagation",
        detail = "scan -> login -> xfer on multiple hosts"
    )
    limits {
        max_memory = "64MB";
        max_instances = 10000;
        on_exceed = throttle;
    }
}
```

**改造后**（seq：顺序 + login 必须在 scan 后 10m 内）：

```wfl
rule rat_propagation {
    events {
        scan  : conn_events && (dport == 22 || dport == 445 || dport == 3389) && bytes_out < 1000
        login : auth_events && result == "success"
        xfer  : conn_events && bytes_out >= 10000
    }
    match<sip,dip:30m> {
        on event seq {
            has scan;
            has login within 10m;
            has xfer;
        }
    } -> score(95.0)
    entity(ip, scan.sip)
    yield security_alerts (
        sip = scan.sip,
        dip = scan.dip,
        alert_type = "rat_propagation",
        detail = "scan -> login -> xfer"
    )
    limits {
        max_memory = "64MB";
        max_instances = 10000;
        on_exceed = throttle;
    }
}
```

行为差异：改造后，`login` 距 `scan` 超过 10m 的样本**不再触发**（`within` 生效）；乱序样本本就由
引擎顺序语义处理（login 先于 scan 不触发）；意图显式化（`has`/`seq`）。现有四个内联测试
（full_chain=3、missing_xfer=0、admin_scan_only=0、single_target=1）结果不变
（这些用例本身就是顺序正确的）。

### 6.2 `password_spraying` — 聚合步骤 + 成功终结步

**改造前**（现有，仅聚合——`>=5 用户失败` 即告警，不区分是否得手）：

```wfl
rule password_spraying {
    events {
        e : auth_events && e.result == "failed"
    }
    match<password_hash:5m> {
        on event { e.user | distinct | count >= 5; }
    } -> score(75.0)
    entity(credential, e.password_hash)
    yield security_alerts (
        sip = e.sip,
        user = e.user,
        alert_type = "password_spraying",
        detail = "single credential tried against >= 5 users in 5min"
    )
    limits {
        max_memory = "64MB";
        max_instances = 10000;
        on_exceed = throttle;
    }
}
```

**改造后**（seq：单步聚合 + 成功登录终结步，显著提高置信度）：

```wfl
rule password_spraying {
    events {
        spray : auth_events && result == "failed"
        ok    : auth_events && result == "success"
    }
    match<password_hash:10m> {
        on event seq {
            spray.user | distinct | count >= 5;  // 步骤 1（聚合）：单口令打 >= 5 个不同用户
            has ok within 5m;                      // 步骤 2（存在性）：随后 5m 内有成功登录
        }
    } -> score(85.0)
    entity(credential, spray.password_hash)
    yield security_alerts (
        sip = ok.sip,
        user = ok.user,
        alert_type = "password_spraying",
        detail = "sprayed >= 5 users then a success followed"
    )
    limits {
        max_memory = "64MB";
        max_instances = 10000;
        on_exceed = throttle;
    }
}
```

说明：步骤 1 的聚合语义与现有 `on event { ... | distinct | count >= 5; }` 一致，
只是现在它成为序列的"前导步"；`ok` 作为终结步把告警从"疑似"升级为"得手"。

### 6.3 `c2_beaconing` — 反面示例（不该 seq）

**保持现有聚合写法不变**：

```wfl
rule c2_beaconing {
    events {
        c : conn_events
            && c.bytes_out < 1000
            && c.duration < 2
            && c.action == "syn"
    }
    match<sip,dip:10m> {
        on event { c | count >= 20; }
        and close { c | count >= 20; }
    } -> score(70.0)
    entity(ip, c.sip)
    yield network_alerts (
        sip = c.sip,
        dip = c.dip,
        alert_type = "c2_beaconing",
        detail = ">= 20 short low-bytes connections to same dest in 10min",
        request_count = 20
    )
    limits {
        max_memory = "64MB";
        max_instances = 10000;
        on_exceed = throttle;
    }
}
```

**不改的原因**：beaconing 是**频率/周期**问题（固定间隔、小包、长持续），本质是聚合 + 间隔统计，
不是事件先后序列问题。seq 无法表达"间隔方差小"这类度量。注释中的建议成立——精确周期性
应在 ETL 侧打 `beacon_score` 标签后用 `on each` 判定，而非 seq。**判断标准：能写成
"先 A 后 B、B 在 A 后 X 内"的才用 seq；需要"多少、多频繁、多规律"的保持聚合。**

### 6.4 `on event any` — 无序共现示例（新增能力）

`on event any` 用于**窗口内共现**检测（弱相关性），顺序无关。示例——"同一主机短时间内
既有扫描又有成功登录"（不关心先后）：

```wfl
rule scan_and_login_cooccur {
    events {
        scan  : conn_events && dport in (22, 445, 3389)
        login : auth_events && result == "success"
    }
    match<sip:10m> {
        on event any {
            scan | count >= 1;
            login | count >= 1;
        }
    } -> score(70.0)
    entity(ip, scan.sip)
    yield security_alerts (
        sip = scan.sip,
        alert_type = "scan_and_login_cooccur",
        detail = "scan and successful login co-occurred within 10m"
    )
}
```

与 `on event seq` 的差别：`login → scan` 的乱序序列在 `any` 下也触发（共现），在 `seq` 下不触发
（顺序约束）。适合"这两类事件在窗口内都出现过"的相关性检测；攻击链的步骤顺序检测用 `seq`。

## 7. 影响面清单

| 组件 | 位置 | 改动 |
|---|---|---|
| grammar | `tree-sitter-wfl` | 新增 `on_event_mode_block` / `seq_rule_step` 产生式，扩展 `match_clause` |
| 解析/编译 | `wf-lang`（wp-reactor） | AST + 编译：`on event` 并行条件 → 有序步骤 |
| 执行 | `wf-engine`（wp-reactor） | rule stage 新增 seq 求值路径 + `SeqState` + TTL + 指标 |
| 规则工具 | warp-fusion（wfl/wfusion rule） | lint 新检查、fmt、`explain` 渲染步骤表 |
| 测试 | warp-fusion | 内联 `test` 块支持时序断言；wfgen oracle 对拍（注入 seq 已就绪） |
| 映射/reload | warp-fusion | `rule_mapping.dat` 支持单规则多窗口；`reload_state_policy="clear"` |
| 文档 | warp-fusion | `rules.md` 补 seq 章节 |

> **跨仓库注意**：`wf-lang` / `wf-engine` 位于 wp-reactor 仓库（当前以 git dependency
> 引用）。seq 的解析与执行改动落在 wp-reactor，需要发布新 tag 并在 warp-fusion 升版本，
> 或先本地 path 依赖联调（`Cargo.toml` 中的 `local_reactor` 块已预留）。

## 8. 待确认的语法决策点

1. ✅ **存在性步骤用 `has`**（`has scan;` = "发生过 scan"；`have` 弃用，保持单一形式）
2. ✅ **默认 gap（followedBy），严格相邻用 `consec`**
3. ✅ **`within` 挂步骤尾，相对上一步完成时刻**
4. ✅ **`skip = to_next` 延后到 L3 NFA**（P2 只实现 `past_last` fire-and-reset）
5. ✅ **部分匹配 TTL = match 窗口时长**，复用 evictor 节奏
6. ✅ **`reload_state_policy` 默认 `clear`**（规则版本变化清空 seq 状态）
7. ✅ **seq 步骤内不直接支持 join**（stage 级 join 仍可用）
8. ✅ **`option` 延后到 L3**（会改 fire 模型为"必选步骤全集"，牵涉 score 加权）。
   与 `has` 语义重复的 `some` 不加；事件重复数统一走 `X | count >= N` 聚合与注入 `with(count)`。

## 9. 落地清单

- [ ] P1 窗口增量（hop/time_mode/stream_tag watermark）——已决定暂缓
- [x] grammar 扩展 + wf-lang 解析（`on event seq` / `on event any`）
- [x] wf-engine seq 求值（L1）→ lint/fmt/explain 支持
- [x] `within` / `not`（L2）→ TTL（`rule.seq_*` 指标埋点延后，`rule.instances` 已覆盖部分匹配 gauge）
- [x] 示例规则改造（rat_propagation / password_spraying → `on event seq`）+ 内联测试
- [x] wfgen oracle 对拍验证
- [x] 文档（rules.md / spec / CHANGELOG）
