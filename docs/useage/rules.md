# WFL 规则

`.wfl` 文件用于声明检测规则、规则输出以及规则内联测试。运行时通过
`wfusion.toml` 的 `[runtime].rules` glob 加载规则文件。

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
