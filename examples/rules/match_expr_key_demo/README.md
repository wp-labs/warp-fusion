# match_expr_key_demo — 表达式派生分组 key（issue #80）

演示 **match 分组 key 使用函数派生表达式**（`coalesce` 实体归一）：普通字段
key 与派生 key 混用，缺失值稳定归组不丢事件。

## 规则

`rules/match_expr_key_demo.wfl`：

```wfl
rule group_by_normalized_entity {
    events { s : security_events }
    // 表达式派生 key（issue #80）：实体归一——ip/主机名/用户名缺一取余
    let attacker_key = coalesce(s.source_ip, s.source_host, s.source_user, "unknown")
    let target_key = coalesce(s.target_ip, s.target_host, "unknown")
    // 普通字段位（log_type）与表达式派生 key 混用
    match<s.log_type, attacker_key, target_key:10m> {
        on event { s | count >= 1; }
    } -> score(30.0)
    entity(chars, attacker_key)
    yield security_alerts (
        attacker_key = attacker_key,
        target_key = target_key,
        ...
    )
}
```

要点：

- **表达式作 key**：`coalesce(...)` 求值结果直接作分组键（`match<attacker_key:10m>`），
  无需预先物化字段；`coalesce` / `concat` / `case` / 二元运算 / 字面量均可，`let`
  引用链编译期展开为纯事件表达式；
- **混用**：同一 `match<>` 内普通字段（`s.log_type`）与派生 key 逐位混写；
- **缺失语义**：引擎的 `coalesce` 把空串视为空值，兜底参数需非空（如 `"unknown"`）
  ——字段缺失时归入 `unknown` 组，事件不丢、分组稳定；无任何回退且求值为空时，
  与普通 key 缺失语义一致（事件不进入任何实例）；
- `yield` / `entity` 按裸名引用派生值（同一 `let` 多处复用）。

## 数据

`data/security_events.ndjson` 5 条事件 → 5 个派生组：

| 行 | attacker_key | target_key | 归一路径 |
|---|---|---|---|
| 1 | `10.0.0.8` | `192.168.1.10` | 双方 ip 命中 |
| 2 | `alice` | `192.168.1.11` | source_ip/host 缺 → 回退 source_user |
| 3 | `bob-ws` | `db-02` | ip 缺 → 回退 host（两侧） |
| 4 | `unknown` | `172.16.0.1` | 源标识全缺 → 非空兜底 |
| 5 | `10.0.0.9` | `unknown` | target 全缺 → 兜底 |

## 运行

```bash
./run.sh            # debug 构建（lint + 内联测试 + batch 回放 + 断言）
./run.sh release    # release 构建
```

期望：5 条告警，逐组键值断言（ip 命中 / 用户回退 / 主机回退 / `unknown` 兜底 ×2 侧）。

## 验证覆盖

- `wfl lint`：表达式派生 key 通过 checker（无状态纯事件函数约束、标量类型校验）。
- `wfl test`：内联测试（5 行输入 → 5 命中）。
- `wfusion batch`：完整引擎链路（分组/输出与内联测试一致）。
- 对应引擎/语言单测见 wp-reactor（issue #80：checker/编译装配/引擎行式+列式对拍、
  fanout 表达式分片）。
