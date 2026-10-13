use std::collections::{HashMap, HashSet};

use wf_lang::WindowSchema;
use wf_lang::ast::{Expr, FieldRef, JoinClause, JoinMode, RuleDecl};

use super::ValidationError;
use crate::datagen::inject_gen::ENTITY_ID_SPACE;
use crate::datagen::inject_gen::field_ref_field_name;
use crate::wfg_ast::{
    AttrValue, FieldPredicate, InjectCase, RateExpr, SyntaxScenario, ValueSource, WfgFile,
    json_top_level_entries,
};

pub(super) fn validate_syntax(
    wfg: &WfgFile,
    schemas: &[WindowSchema],
    all_rules: &[&RuleDecl],
    skip_wfl: bool,
) -> Vec<ValidationError> {
    let mut errors = Vec::new();
    let Some(syntax) = &wfg.syntax else {
        return errors;
    };
    let duration = wfg.scenario.time_clause.duration;

    validate_scenario_annos(syntax, &mut errors);

    if syntax.background.streams.is_empty() {
        errors.push(ValidationError {
            code: "VN1",
            message: "background block must contain at least one stream".to_string(),
        });
    }

    // VN34 用的已见窗口集合（重复声明检测）。
    let mut seen_streams: HashSet<&str> = HashSet::new();

    for s in &syntax.background.streams {
        // VN34：同一个 `background` 里对**同一窗口**重复声明 `stream`。生成器会各造一份：
        // 流量按速率的**和**叠加、`derive_total` 也按多条流算，而且两条流各有自己的 RNG 与
        // 实体值带（不等于一条合并后的声明）——此前只有 VN3（窗口存在）会过，重复静默生效。
        if !seen_streams.insert(s.stream.as_str()) {
            errors.push(ValidationError {
                code: "VN34",
                message: format!(
                    "background 里重复声明了 stream '{}'：同一窗口只能声明一次（重复会让生成的流量按速率之和叠加，且两条流各有独立的取值带）；请合并成一条 `gen <rate>`",
                    s.stream
                ),
            });
        }
        if s.rate.approx_eps() <= 0.0 {
            errors.push(ValidationError {
                code: "VN2",
                message: format!("stream '{}': rate must be greater than 0", s.stream),
            });
        }
        if !schemas.iter().any(|ws| ws.name == s.stream) {
            errors.push(ValidationError {
                code: "VN3",
                message: format!(
                    "stream '{}' not found in loaded schemas (.wfs windows)",
                    s.stream
                ),
            });
        }
        // VN28：`wave` / `burst` / `timeline` 三个速率形态**语法已定、语义未实现**——降级时
        // 只取 `base=`（`timeline` 取第一段）当常量速率，生成结果与写法不符且不报错
        // （`burst(peak=300/s, …)` 拿到的是 `base` 的平坦流量，压测强度静默失效）。
        // 在语义落地（设计 §7.3 P2）之前先明确拦下，给出改写方向。
        if let Some(kind) = unimplemented_rate_kind(&s.rate) {
            errors.push(ValidationError {
                code: "VN28",
                message: format!(
                    "stream '{}': `gen {kind}` 的随时间变化尚未实现（当前会按 `base=` 的常量速率生成，与写法不符）；请先改用常量速率 `gen 100/s`",
                    s.stream
                ),
            });
        }
    }

    // `replay`：与 WFL 无关（`--no-wfl` 也要发数据），因此不受 `skip_wfl` 门控（设计 §8）。
    validate_replays(syntax, schemas, duration, &mut errors);

    // 注入实体值占用的 id 数（VN27 口径）；VN31 的空间预算也要用。
    let mut total_entity_ids: u128 = 0;

    // VN20（旧的比例形式）在解析期就报错，走不到这里。
    if let Some(inj) = &syntax.injection {
        let background_streams: HashSet<&str> = syntax
            .background
            .streams
            .iter()
            .map(|stream| stream.stream.as_str())
            .collect();
        let schemas_by_name: HashMap<&str, &WindowSchema> = schemas
            .iter()
            .map(|schema| (schema.name.as_str(), schema))
            .collect();

        for case in &inj.cases {
            // 生成器会写进事件的实体键字段（VN12 与 join 块的保护集）——算一次，两处共用。
            let entity_key_fields = case_entity_key_fields(case, all_rules, skip_wfl);
            // VN27 口径 = 生成器实际消耗的实体 id：**每个实体、每个键字段各占一个**
            // （`generate_key_values` 取 `entity_counter + i`，i 走 `names` 的每一位）。
            // 少算这一因子（历史实现只算实体个数），键多的规则会把 id 空间算大，
            // 溢出后用例之间的实体会重叠（hit 与 near_miss 指向同一实体）而无人报错。
            total_entity_ids += entity_ids_consumed(case) * entity_key_fields.len().max(1) as u128;
            // VN30：`join <window> as <key> { … }`（设计 §9 跨流注入）的静态一致性。
            validate_case_joins(
                case,
                all_rules
                    .iter()
                    .copied()
                    .find(|r| r.name == case.target_rule),
                &entity_key_fields,
                &schemas_by_name,
                &mut errors,
            );
            let stream = case.stream.as_str();
            // 用例 stream 的窗口：生成器 `build_alias_map_for_syntax_case` 按别名查 `background`
            // 的 stream 块、取其窗口名。当前语法里 stream 名即窗口名（解析期 `alias` / `window`
            // 同源），所以这里用 stream 名当窗口。
            let stream_window = stream;
            let case_schema = schemas_by_name.get(stream).copied();
            if !background_streams.contains(stream) {
                errors.push(ValidationError {
                    code: "VN10",
                    message: format!(
                        "injection case stream '{}' is not declared in background",
                        stream
                    ),
                });
            }

            // VN33：用例 stream 的窗口必须是目标规则的**事件绑定窗**。窗口对不上时生成器
            // （`build_alias_map_for_syntax_case`）找不到任何可映射的 bind，用例造的事件
            // 无处可放——此前只在生成期报 "cannot be mapped to any event step bind"，
            // 写到一半才发现。提前到校验期拦下。目标规则本身不存在时由 VN14 报，不重复刷屏。
            if !skip_wfl
                && let Some(rule) = all_rules.iter().find(|rule| rule.name == case.target_rule)
            {
                let windows = rule_binding_windows(rule);
                if !windows.iter().any(|window| window == stream_window) {
                    errors.push(ValidationError {
                        code: "VN33",
                        message: format!(
                            "注入用例 stream '{}' 的窗口 '{}' 不是规则 '{}' 的任何事件绑定窗（binding windows: {}）",
                            stream,
                            stream_window,
                            case.target_rule,
                            if windows.is_empty() {
                                "无".to_string()
                            } else {
                                windows.join(", ")
                            }
                        ),
                    });
                }
            }

            if case.entity_count == 0 {
                errors.push(ValidationError {
                    code: "VN21",
                    message: format!("injection case '{}' 实体个数必须大于 0（hit<0>）", stream),
                });
            }
            if case.groups.is_empty() {
                errors.push(ValidationError {
                    code: "VN21",
                    message: format!(
                        "injection case '{}' 至少需要一个 `use ... x N` 事件组",
                        stream
                    ),
                });
            }
            // VN25：`spread` 与 `without ... within` 的时间窗不能超过场景 duration——
            // 超出部分会被截断，窗口内“不得出现匹配事件”的保证会静默失效。
            if let Some(spread) = case.spread
                && spread > duration
            {
                errors.push(ValidationError {
                    code: "VN25",
                    message: format!(
                        "injection case '{}' spread {:?} 超过场景 duration {:?}",
                        stream, spread, duration
                    ),
                });
            }
            for (idx, without) in case.withouts.iter().enumerate() {
                if let Some(within) = without.within
                    && within > duration
                {
                    errors.push(ValidationError {
                        code: "VN25",
                        message: format!(
                            "injection case '{}' 第 {} 个 without 的 within {:?} 超过场景 duration {:?}",
                            stream,
                            idx + 1,
                            within,
                            duration
                        ),
                    });
                }
            }

            // VN22 / VN23：显式实体字段（`hit<sip: 500>`）的静态一致性。
            //
            // 两处漂移都会让注入**静默指向错实体**（生成器拿不到类型时给字符串兜底 /
            // 逐实体变化的字段与规则聚合的 key 不是同一个），最后表现为断言覆盖不到或
            // 断言错对象，因此在校验期就拦下：
            // - VN22：字段必须在该 stream 的 schema 里；
            // - VN23：字段必须与规则**推断**的实体字段一致（单 key `match` = 该 key；
            //   `on each` = `entity(...)` 的单字段）。多 key 规则的实体是 key 元组，
            //   显式写字段是消歧用法，不算不一致（设计 §3.7）。
            if let Some(explicit) = case.entity_field.as_deref() {
                if let Some(schema) = case_schema
                    && !schema.fields.iter().any(|field| field.name == explicit)
                {
                    errors.push(ValidationError {
                        code: "VN22",
                        message: format!(
                            "injection case '{}' 实体字段 '{}' 不在 stream '{}' 的 schema '{}' 里",
                            stream, explicit, stream, schema.name
                        ),
                    });
                }
                if !skip_wfl
                    && let Some(rule) = all_rules.iter().find(|rule| rule.name == case.target_rule)
                    && let Some(inferred) = inferred_entity_field(rule, schemas)
                    && inferred != explicit
                {
                    errors.push(ValidationError {
                        code: "VN23",
                        message: format!(
                            "injection case '{}' 显式实体字段 '{}' 与规则 '{}' 推断的实体字段 '{}' 不一致（去掉显式字段即用推断值；多 key 规则才需要显式消歧）",
                            stream, explicit, case.target_rule, inferred
                        ),
                    });
                }
            }

            // VN12 的口径 = 生成器的**实体键字段**集合（`generate_key_values` 的 `names`，
            // 与 `RuleStructure::effective_entity_field` 同口径）——算一次，两处共用：
            //   `match` 规则 → `match<...>` 的键（生成器无条件写入）；
            //   `on each`    → 没有 match key，生成器改把 `entity(...)` 的单字段当键写；
            //   用例头显式写的实体字段 → 一并算在内（join 块里的同名写法也拦，见
            //   `validate_case_joins`）。
            // `use` 的 predicate_overrides 优先级只在 key_overrides 之后（见
            // `build_event_fields_with_predicates`）→ 写这些字段是静默失效，必须拦下。
            //
            // 别拿「规则推断的实体字段」当口径：`match<seller>` 而 `entity(…, b.auction)`
            // 时（join-then-key）生成器覆盖的是 `seller` 而不是 `auction`——按后者会误报，
            // 把本来能生效的 `use(auction=…)` 拒掉。

            // VN24：`use` 事件组数不得超过**该 stream 上**的事件步骤数（每个
            // `use ... x N` 对应该 stream 上的一个步骤）。生成期也拦
            // （`inject_gen::helpers::plan::plan_use_steps`），这里提前到校验期——
            // 数错组数会静默少注入某个步骤的事件。步数口径见 [`injectable_step_count`]：
            // 规则的语法步骤未必都落在该 stream 的窗口上，只有落在上面的才是有效步骤。
            if !skip_wfl
                && let Some(rule) = all_rules.iter().find(|rule| rule.name == case.target_rule)
            {
                let step_count = injectable_step_count(rule, stream_window);
                if case.groups.len() > step_count {
                    errors.push(ValidationError {
                        code: "VN24",
                        message: format!(
                            "injection case '{}' 的 use 事件组数 {} 超过规则 '{}' 在 stream '{}' 上的事件步骤数 {}（每个 `use ... x N` 对应该 stream 上的一个步骤）",
                            stream,
                            case.groups.len(),
                            case.target_rule,
                            stream_window,
                            step_count
                        ),
                    });
                }
            }

            for (idx, group) in case.groups.iter().enumerate() {
                if group.count == 0 {
                    errors.push(ValidationError {
                        code: "VN21",
                        message: format!(
                            "injection case '{}' 第 {} 个事件组 x 0：每个实体的条数必须大于 0",
                            stream,
                            idx + 1
                        ),
                    });
                }
                // 逐条记录检查（`use from` 的数组形态有多条）：字段级校验必须按记录
                // 分别做——不同记录重复出现同一字段是正常的（每条记录都带实体键字段）。
                for record in source_records(&group.source, &mut errors, stream, idx) {
                    check_predicate_fields(
                        &mut errors,
                        stream,
                        idx,
                        "use",
                        &record,
                        &entity_key_fields,
                        case_schema,
                    );
                }
            }

            // `without(...)` 的谓词做同款字段检查：写错的字段名会静默变成“删不掉该事件”，
            // 窗口保证失效又不报错（设计 §3.8）。
            for (idx, without) in case.withouts.iter().enumerate() {
                check_predicate_fields(
                    &mut errors,
                    stream,
                    idx,
                    "without",
                    &without.predicates,
                    &entity_key_fields,
                    case_schema,
                );
            }
        }

        // VN27：实体 id 总数不得超过 24 位地址空间（设计 §7.2）。用例之间靠**分段**保证
        // 实体值不重叠，而实体值按 24 位地址映射（Ip 写 `10.a.b.c`）；超出后不同用例会
        // 拿到同一个值，`hit` 与 `near_miss` 指向同一实体、两个口径互相污染，且无任何报错。
        // 这里用 u128 累加，避免大数字先把 u64 溢出成小数字而绕过检查。
        if total_entity_ids >= u128::from(ENTITY_ID_SPACE) {
            errors.push(ValidationError {
                code: "VN27",
                message: format!(
                    "injection 实体 id 总数 {} 达到上限 {}（实体值按 24 位地址映射，超出后用例之间的实体会重叠：hit 与 near_miss 会指向同一实体）；请减少各用例的实体个数或 `x N`",
                    total_entity_ids, ENTITY_ID_SPACE
                ),
            });
        }
    }

    // VN31：`entity <window>.<field> zipf(...)` 实体分布（设计 §10）。
    validate_entity_dists(syntax, schemas, total_entity_ids, &mut errors);

    // Rule-presence check (VN14) is skipped when the WFL pipeline is opted out
    // (--no-wfl / --no-expect): there are no rules to reference.
    if !skip_wfl && let Some(inj) = &syntax.injection {
        for case in &inj.cases {
            if !all_rules.iter().any(|rule| rule.name == case.target_rule) {
                errors.push(ValidationError {
                    code: "VN14",
                    message: format!(
                        "injection case '{}' targets rule '{}' not found in WFL files",
                        case.stream, case.target_rule
                    ),
                });
            }
        }
    }

    errors
}

/// 注解值的人话类型名（VN29 消息用）。
fn attr_value_kind(value: &AttrValue) -> &'static str {
    match value {
        AttrValue::Number(_) => "数字",
        AttrValue::Duration(_) => "时长",
        AttrValue::String(_) => "字符串",
        AttrValue::Bool(_) => "布尔",
        AttrValue::Json(_) => "JSON 值",
    }
}

/// VN29：场景注解的**键/值白名单**。
///
/// `#[...]` 只认 `duration`（时长字面量），`<...>` 只认 `seed`（非负数字）。注解列表本身是
/// 泛化解析的，于是其余键此前被**静默忽略**——文档里声明「未实现」的 `tick` / `rows` / `emit`
/// 就是这一类；写错键名（`#[duratoin=10m]`）或写错值类型（`#[duration=10]`）同样会悄悄退回
/// 默认值（60s / seed 0），语义与写法不符且无任何提示。
fn validate_scenario_annos(syntax: &SyntaxScenario, errors: &mut Vec<ValidationError>) {
    for attr in &syntax.attrs {
        if attr.key != "duration" {
            errors.push(ValidationError {
                code: "VN29",
                message: format!(
                    "注解键 '{}' 不支持：`#[...]` 只认 'duration'（tick / rows / emit 从未实现）",
                    attr.key
                ),
            });
        } else if !matches!(attr.value, AttrValue::Duration(_)) {
            errors.push(ValidationError {
                code: "VN29",
                message: format!(
                    "注解 'duration' 的值必须是时长字面量（如 `10m`），实际是{}",
                    attr_value_kind(&attr.value)
                ),
            });
        }
    }
    for attr in &syntax.inline_annos {
        if attr.key != "seed" {
            errors.push(ValidationError {
                code: "VN29",
                message: format!("注解键 '{}' 不支持：`<...>` 只认 'seed'", attr.key),
            });
        } else if !matches!(attr.value, AttrValue::Number(n) if n >= 0.0) {
            errors.push(ValidationError {
                code: "VN29",
                message: format!(
                    "注解 'seed' 的值必须是非负数字，实际是{}",
                    attr_value_kind(&attr.value)
                ),
            });
        }
    }
}

/// VN31：`entity <window>.<field> zipf(...)` 实体分布（设计 §10）。
///
/// 值域**必须与注入实体分区**（注入占底部 `[0, total_entity_ids)`、池占顶部 `pool` 个、
/// 新值带占再往下的 `pool` 个），否则背景噪声会撞上注入实体、污染 INJ1/INJ2 的口径。
fn validate_entity_dists(
    syntax: &SyntaxScenario,
    schemas: &[WindowSchema],
    total_entity_ids: u128,
    errors: &mut Vec<ValidationError>,
) {
    let mut seen: HashSet<String> = HashSet::new();
    for dist in &syntax.background.entities {
        if !seen.insert(format!("{}.{}", dist.window, dist.field)) {
            errors.push(ValidationError {
                code: "VN31",
                message: format!("entity 分布对 '{}.{}' 重复声明", dist.window, dist.field),
            });
        }

        let Some(schema) = schemas.iter().find(|s| s.name == dist.window) else {
            errors.push(ValidationError {
                code: "VN31",
                message: format!(
                    "entity 分布的目标窗 '{}' 不在已加载的 schema 里",
                    dist.window
                ),
            });
            continue;
        };

        if !syntax
            .background
            .streams
            .iter()
            .any(|stream| stream.stream == dist.window)
        {
            errors.push(ValidationError {
                code: "VN31",
                message: format!(
                    "entity 分布的目标窗 '{}' 没有对应的 `background` stream（该声明不会生效）",
                    dist.window
                ),
            });
        }

        match schema.fields.iter().find(|f| f.name == dist.field) {
            None => {
                errors.push(ValidationError {
                    code: "VN31",
                    message: format!(
                        "entity 分布的字段 '{}' 不在窗口 '{}' 的 schema 里",
                        dist.field, dist.window
                    ),
                });
            }
            Some(field) => {
                let supported = matches!(
                    &field.field_type,
                    wf_lang::FieldType::Base(
                        wf_lang::BaseType::Ip
                            | wf_lang::BaseType::Digit
                            | wf_lang::BaseType::Float
                            | wf_lang::BaseType::Chars
                            | wf_lang::BaseType::Hex
                    )
                );
                if !supported {
                    errors.push(ValidationError {
                        code: "VN31",
                        message: format!(
                            "entity 分布的字段 '{}' 类型不支持（只支持 ip / digit / float / chars / hex）",
                            dist.field
                        ),
                    });
                }
            }
        }

        if dist.pool == 0 {
            errors.push(ValidationError {
                code: "VN31",
                message: format!("entity '{}': `pool` 必须 ≥ 1", dist.field),
            });
        }
        if !(dist.exponent.is_finite() && dist.exponent >= 0.0) {
            errors.push(ValidationError {
                code: "VN31",
                message: format!("entity '{}': `exponent` 必须是 ≥ 0 的有限数", dist.field),
            });
        }
        if !(dist.fresh.is_finite() && (0.0..=1.0).contains(&dist.fresh)) {
            errors.push(ValidationError {
                code: "VN31",
                message: format!("entity '{}': `fresh` 必须落在 0..=1", dist.field),
            });
        }

        // 值域预算：注入实体 + 池 + 新值带都必须塞进 24 位空间。
        let need = total_entity_ids + u128::from(dist.pool) * 2;
        if need > u128::from(ENTITY_ID_SPACE) {
            errors.push(ValidationError {
                code: "VN31",
                message: format!(
                    "entity '{}' 的值域超出 24 位地址空间：注入实体 {} + 池 {} + 新值带 {} > {}（背景池与注入实体值必须不重叠）",
                    dist.field, total_entity_ids, dist.pool, dist.pool, ENTITY_ID_SPACE
                ),
            });
        }
    }
}

/// VN30：`join <target_window> as <right_key_field> { use … x N }` 的静态一致性
/// （设计 §9 跨流注入）。
///
/// 右事件的连接键与时间由生成器推导（连接键 = 左实体键字段的值、时间 = 左事件时间），所以这里要
/// 保证推导是**有依据**的：目标窗要能唯一匹配到规则的一个 join 子句，且形态是生成器实现了的
/// 两种（deferred / snapshot，见下）。造错跳流数据会静默改掉断言口径，因此全部按报错处理。
///
/// 不做的事：`within` 下界是否真的包含左事件时间（它取决于左事件时间的具体取值，运行期由
/// 引擎判定；校验期只校验形态与连接键）。
fn validate_case_joins(
    case: &InjectCase,
    rule: Option<&RuleDecl>,
    // 驱动侧实体键字段（与主事件组同一口径）：join 块里写这些字段同样会被生成器覆盖。
    entity_key_fields: &[String],
    schemas_by_name: &HashMap<&str, &WindowSchema>,
    errors: &mut Vec<ValidationError>,
) {
    for join in &case.joins {
        let Some(right_schema) = schemas_by_name.get(join.window.as_str()).copied() else {
            errors.push(ValidationError {
                code: "VN30",
                message: format!(
                    "injection case '{}' 的 `join {}` 目标窗不在已加载的 schema 里",
                    case.stream, join.window
                ),
            });
            continue;
        };

        let matched: Vec<&JoinClause> = rule
            .map(|r| {
                r.joins
                    .iter()
                    .filter(|jc| {
                        // 只认**首条件**：生成器 `extract_rule_structure` 只登记
                        // `join.conds.first()`（多条件 join 的其余条件生成器不管），
                        // 匹配“任一条件”会让用例按一条生成器不实现的路径造数据。
                        jc.target_window == join.window
                            && jc
                                .conditions
                                .first()
                                .is_some_and(|c| field_ref_field_name(&c.right) == join.key_field)
                    })
                    .collect()
            })
            .unwrap_or_default();
        if matched.len() > 1 {
            // 多条 join 子句喊同一个“目标窗 + 首条件右侧字段”时，“右行放哪、连接键写什么”
            // 无从确定（生成器按第一条找，用户看到的不一定是同一条）→ 不猜，报错。
            errors.push(ValidationError {
                code: "VN30",
                message: format!(
                    "injection case '{}' 的 `join {} as {}` 在规则 '{}' 里匹配到 {} 条 join 子句（目标窗与首条件右侧字段都相同），无法确定按哪条生成；请让目标窗 + 连接键在规则里唯一",
                    case.stream,
                    join.window,
                    join.key_field,
                    case.target_rule,
                    matched.len()
                ),
            });
            continue;
        }
        let Some(join_clause) = matched.first().copied() else {
            // 规则本身找不到时由 VN14 报，这里不重复刷屏。
            if rule.is_some() {
                errors.push(ValidationError {
                    code: "VN30",
                    message: format!(
                        "injection case '{}' 的 `join {} as {}` 匹配不到规则 '{}' 的 join 子句（目标窗与**首条件**的右侧连接键要和规则的 `join … on … == …` 一致）",
                        case.stream, join.window, join.key_field, case.target_rule
                    ),
                });
            }
            continue;
        };

        // 生成器支持两种形态（设计 §9）；右事件的放置由形态决定：
        //  - **deferred**：inner + `within` + `emit at` → 右事件与左事件**同刻**（到期评估时
        //    右行必已在窗内）；
        //  - **snapshot**：`snapshot` 且**无** `within` 的点查 → 右事件**前挪 1ms**（驱动事件
        //    被处理时右行必须已可见；常量见 `extract::SNAPSHOT_LEAD_NANOS`）。
        // 其余（`asof` / `anti`、无 `emit at` 的即时 inner、`snapshot` + `within`）明确拒绝：
        // 即时 inner 要求右行更早、而 `within` 下界常就是左事件时间，两者冲突会时好时坏。
        let deferred = matches!(join_clause.mode, JoinMode::Inner)
            && join_clause.emit_at.is_some()
            && join_clause.within.is_some();
        let snapshot =
            matches!(join_clause.mode, JoinMode::Snapshot) && join_clause.within.is_none();
        if !deferred && !snapshot {
            errors.push(ValidationError {
                code: "VN30",
                message: format!(
                    "injection case '{}' 的 `join {}` 形态不支持：只支持 deferred（inner + `within` + `emit at`）与 snapshot（无 `within`）两种；asof / anti、即时 inner（无 `emit at`）、以及 snapshot + `within` 暂不支持",
                    case.stream, join.window
                ),
            });
        }

        if join.groups.is_empty() {
            errors.push(ValidationError {
                code: "VN21",
                message: format!(
                    "injection case '{}' 的 `join {}` 至少需要一个 `use ... x N` 事件组",
                    case.stream, join.window
                ),
            });
        }
        for (idx, group) in join.groups.iter().enumerate() {
            if group.count == 0 {
                errors.push(ValidationError {
                    code: "VN21",
                    message: format!(
                        "injection case '{}' 的 `join {}` 第 {} 个事件组 x 0",
                        case.stream,
                        join.window,
                        idx + 1
                    ),
                });
            }
            // 字段落在**目标窗** schema 里；连接键由生成器写，重复声明按 VN12 报。
            //
            // 「生成器会写」的不只右窗连接键：右行的 `key_overrides` 先整份套上（左实体键 + 显式
            // 实体字段），再叠 `use(...)`——所以在 join 块里写**驱动侧实体键字段**同样会被静默
            // 替换，必须一并拦下（主事件组与 `without(...)` 已是这个口径）。
            let mut protected: Vec<String> = entity_key_fields.to_vec();
            if !protected.iter().any(|field| field == &join.key_field) {
                protected.push(join.key_field.clone());
            }
            for record in source_records(&group.source, errors, &join.window, idx) {
                check_predicate_fields(
                    errors,
                    &join.window,
                    idx,
                    "join use",
                    &record,
                    &protected,
                    Some(right_schema),
                );
            }
        }
    }
}

/// 尚未实现的速率形态名（VN28 的点名用）；`Constant` 返回 `None`。
fn unimplemented_rate_kind(rate: &RateExpr) -> Option<&'static str> {
    match rate {
        RateExpr::Constant(_) => None,
        RateExpr::Wave { .. } => Some("wave(...)"),
        RateExpr::Burst { .. } => Some("burst(...)"),
        RateExpr::Timeline(_) => Some("timeline { ... }"),
    }
}

/// 一个注入用例消耗的实体 id 数（设计 §7.2 的 VN27 口径）。
///
/// 与生成侧逐实体循环同口径：
///
/// - `hit` / `near_miss`：每个实体一个 id → `entity_count`；
/// - `miss`：**每个事件一个独立键**（不然就成簇报警了），一轮用例内每个事件各占一个 id
///   → `entity_count × Σ(N)`。
///
/// 用 `u128` 累加：这两个数都来自用户书写的 `u64`，先做 u64 乘法会把大数字翻成小数字、
/// 反而绕过了上限检查。
fn entity_ids_consumed(case: &crate::wfg_ast::InjectCase) -> u128 {
    let count = u128::from(case.entity_count);
    match case.mode {
        crate::wfg_ast::InjectCaseMode::Miss => {
            let per_entity: u128 = case
                .groups
                .iter()
                .map(|group| u128::from(group.count))
                .sum();
            count * per_entity
        }
        _ => count,
    }
}

/// `replay` 语句的校验（设计 §8.3）。
///
/// - **VN3**：目标 stream 不在已加载的 schema 里（与 `background` 的 stream 同一类错，
///   但 `replay` **不要求**写在 `background` 里——回放流可以完全不要背景噪声）；
/// - **VN26**：文件记录为空、或记录里的时间字段口径不齐（部分记录有、部分没有）——
///   后者会静默把一部分数据摆在错误的时间上，所以在校验期就拦；
/// - **VN25**：文件的时间跨度超过 `#[duration]`（平移后必然溢出，不截断）。
fn validate_replays(
    syntax: &crate::wfg_ast::SyntaxScenario,
    schemas: &[WindowSchema],
    duration: std::time::Duration,
    errors: &mut Vec<ValidationError>,
) {
    let schemas_by_name: HashMap<&str, &WindowSchema> = schemas
        .iter()
        .map(|schema| (schema.name.as_str(), schema))
        .collect();
    let duration_nanos = duration.as_nanos().min(i64::MAX as u128) as i64;

    for stmt in &syntax.replays {
        let schema = schemas_by_name.get(stmt.window.as_str()).copied();
        if schema.is_none() {
            errors.push(ValidationError {
                code: "VN3",
                message: format!(
                    "replay 目标 stream '{}' not found in loaded schemas (.wfs windows)",
                    stmt.window
                ),
            });
        }

        if stmt.records.is_none() {
            errors.push(ValidationError {
                code: "VN26",
                message: format!(
                    "replay 文件 `{}` 尚未解析（未经 loader）；请通过 CLI（wfgen lint / gen）加载场景",
                    stmt.file
                ),
            });
            continue;
        }

        let records = match super::super::datagen::replay_gen::replay_records(stmt) {
            Ok(records) => records,
            Err(err) => {
                errors.push(ValidationError {
                    code: "VN26",
                    message: err
                        .detail()
                        .clone()
                        .unwrap_or_else(|| format!("replay 文件 `{}` 的记录形态非法", stmt.file)),
                });
                continue;
            }
        };
        if records.is_empty() {
            errors.push(ValidationError {
                code: "VN26",
                message: format!("replay 文件 `{}` 为空", stmt.file),
            });
            continue;
        }

        match super::super::datagen::replay_gen::plan_replay_timeline(&records, schema, duration) {
            Ok(timeline) => {
                let span = timeline.offsets_nanos.iter().max().copied().unwrap_or(0);
                if span > duration_nanos {
                    errors.push(ValidationError {
                        code: "VN25",
                        message: format!(
                            "replay 文件 `{}` 的时间跨度 {} 超过场景 duration {:?}（平移后必然溢出）",
                            stmt.file,
                            std::time::Duration::from_nanos(span as u64).as_secs_f64(),
                            duration
                        ),
                    });
                }
            }
            Err(err) => errors.push(ValidationError {
                code: "VN26",
                message: err
                    .detail()
                    .clone()
                    .unwrap_or_else(|| format!("replay 文件 `{}` 的时间字段口径非法", stmt.file)),
            }),
        }
    }
}

/// 规则在某个 stream（窗口）上**可注入**的事件步骤数（设计 §4.1 VN24）。
///
/// 口径 = 生成器实际会走到的步骤数，与
/// `inject_gen::dispatch::build_alias_map_for_syntax_case` 一致：只有 bind 的窗口等于
/// 该用例 stream 的窗口时，生成器才把这个 bind 收进 alias map，`extract_rule_structure`
/// 随后**只**为收进 map 的步骤产出 `StepInfo`。因此：
/// - `match` 普通形态 = `on event` 里绑该窗的步骤数；
/// - `match` 链形态（`on event seq`）= 绑该窗的链步骤数，**negation 步骤不计**（编译器把
///   它们交给 L2 的 `SeqPlan` 强制执行，不产出 use-step）；
/// - `on each` = 1（绑该窗时；生成器为该绑定合成一个步骤）；
/// - stats 形态 = 1（绑该窗时；`match_plan.event_steps` 为空，但生成器为 stats 的
///   **绑定源窗**合成一个步骤——`extract_rule_structure` 与 `build_alias_map_for_syntax_case`
///   的 stats 分支同口径：注 N 条事件即落入该规则的统计桶）。
///
/// 为什么不能直接数规则的**语法**步骤数：两个步骤绑在不同窗、用例只覆盖其中一个时，
/// 另一个步骤落不进该 stream 的 alias map，对应的 `use ... x N` 找不到落点——多写的
/// 那组被静默丢掉（不报错），断言于是少了一半事件。
fn injectable_step_count(rule: &RuleDecl, window: &str) -> usize {
    let window_of = |alias: &str| {
        rule.events
            .decls
            .iter()
            .find(|decl| decl.alias == alias)
            .map(|decl| decl.window.as_str())
    };
    if let Some(each) = &rule.each_clause {
        return usize::from(window_of(&each.alias) == Some(window));
    }
    if let Some(stats) = &rule.stats_clause {
        // 生成器取 stats 的**第一个**度量源别名（无度量时退回第一个 bind，见
        // `extract_rule_structure`），只有它在该 stream 上时才合成那 1 步。
        let alias = stats
            .measures
            .first()
            .map(|measure| measure.source_alias.as_str())
            .or_else(|| rule.events.decls.first().map(|decl| decl.alias.as_str()));
        return usize::from(alias.and_then(window_of) == Some(window));
    }
    match &rule.match_clause.seq {
        Some(chain) => chain
            .steps
            .iter()
            .filter(|step| !step.neg)
            .filter(|step| window_of(&step.branch.source) == Some(window))
            .count(),
        None => rule
            .match_clause
            .on_event
            .iter()
            .filter(|step| {
                step.branches
                    .iter()
                    .any(|branch| window_of(&branch.source) == Some(window))
            })
            .count(),
    }
}

/// 规则上生成器**会映射**到注入 stream 的事件绑定窗（VN33 的口径）。
///
/// 与 `build_alias_map_for_syntax_case` 同源：`match` 步骤的分支源、`on each` 的别名、
/// stats 度量的源别名（无度量时退回第一个 bind）——别名经 `events { alias : window }`
/// 解析成窗口名。只收真正参与匹配的绑定，`events` 里声明了但没用的别名不算。
fn rule_binding_windows(rule: &RuleDecl) -> Vec<String> {
    let mut aliases: Vec<&str> = Vec::new();
    for step in &rule.match_clause.on_event {
        aliases.extend(step.branches.iter().map(|branch| branch.source.as_str()));
    }
    if let Some(chain) = &rule.match_clause.seq {
        aliases.extend(chain.steps.iter().map(|step| step.branch.source.as_str()));
    }
    if let Some(each) = &rule.each_clause {
        aliases.push(each.alias.as_str());
    }
    if let Some(stats) = &rule.stats_clause {
        if stats.measures.is_empty() {
            aliases.extend(rule.events.decls.first().map(|decl| decl.alias.as_str()));
        } else {
            aliases.extend(
                stats
                    .measures
                    .iter()
                    .map(|measure| measure.source_alias.as_str()),
            );
        }
    }

    let mut windows: Vec<String> = Vec::new();
    for alias in aliases {
        if let Some(decl) = rule.events.decls.iter().find(|decl| decl.alias == alias)
            && !windows.iter().any(|window| window == &decl.window)
        {
            windows.push(decl.window.clone());
        }
    }
    windows
}

/// 生成器会**写进事件**的实体键字段（VN12 保护集与 join 块的共用口径）：
/// 规则口径（[`rule_entity_key_fields`]）∪ join 驱动侧连接键 ∪ 实体标识字段
/// （用例头显式写的优先，否则 `entity(...)` 的单字段）——
/// 与生成器 `RuleStructure::keys ∪ entity_key_fields ∪ mirror_join_keys` 逐项对应。
///
/// 三者都必须收进来：生成器写的字段优先级高于 `use`（`build_event_fields_with_predicates`
/// 先套 key_overrides），漏掉一个，对应的 `use(<该字段>=…)` 就是静默失效。
fn case_entity_key_fields(
    case: &InjectCase,
    all_rules: &[&RuleDecl],
    skip_wfl: bool,
) -> Vec<String> {
    let rule_decl = if skip_wfl {
        None
    } else {
        all_rules
            .iter()
            .find(|rule| rule.name == case.target_rule)
            .copied()
    };
    let mut fields: Vec<String> = rule_decl.map(rule_entity_key_fields).unwrap_or_default();
    if let Some(rule) = rule_decl {
        for field in rule_join_left_fields(rule) {
            if !fields.contains(&field) {
                fields.push(field);
            }
        }
        // 实体标识字段：显式写的优先，否则 `entity(...)` 的单字段。生成器**总是**写它
        // （不写就无法与告警 `entity_id` 对齐：断言真空）。
        let identity = case
            .entity_field
            .clone()
            .or_else(|| rule_entity_field(rule));
        if let Some(field) = identity
            && !fields.contains(&field)
        {
            fields.push(field);
        }
    } else if let Some(explicit) = case.entity_field.as_deref()
        && !fields.iter().any(|field| field == explicit)
    {
        // `--no-wfl`：没有规则口径可用，只剩用例头显式写的那个字段。
        fields.push(explicit.to_string());
    }
    fields
}

/// 规则的 join 子句里**驱动侧**的连接键字段（`on <left> == <right>` 的 left），
/// 与生成器 `RuleStructure::mirror_join_keys` 同源：连接键两侧取同一个值，
/// 生成器会把它写进驱动事件。
fn rule_join_left_fields(rule: &RuleDecl) -> Vec<String> {
    let mut fields: Vec<String> = Vec::new();
    for join in &rule.joins {
        // 与生成器同口径：只取**首条件**（`extract_rule_structure` 只支持它，
        // 其余形态由 VN30 拦下）。
        if let Some(field) = join
            .conditions
            .first()
            .and_then(|cond| leaf_name(&cond.left))
            && !fields.contains(&field)
        {
            fields.push(field);
        }
    }
    fields
}

/// 规则里生成器会当作**实体键字段**写入事件的字段名（`RuleStructure::keys` 同口径）：
/// 显式 key 映射 → 映射的来源字段；否则 `match<...>` 的键。
/// `on each` 规则没有 match key，返回空（由 [`rule_entity_field`] 补位）。
fn rule_entity_key_fields(rule: &RuleDecl) -> Vec<String> {
    if let Some(mapping) = &rule.match_clause.key_mapping {
        let names: Vec<String> = mapping
            .iter()
            .filter_map(|item| leaf_name(&item.source_field))
            .collect();
        if !names.is_empty() {
            return names;
        }
    }
    rule.match_clause
        .keys
        .iter()
        .filter_map(leaf_name)
        .collect()
}

/// `entity(...)` 的单字段——`on each` 形态下它就是生成器的实体键字段。
fn rule_entity_field(rule: &RuleDecl) -> Option<String> {
    match &rule.entity.id_expr {
        Expr::Field(fr) => leaf_name(fr),
        _ => None,
    }
}

/// 规则推断出的实体字段（设计 §3.7）：单 key `match` → 该 key；`on each` →
/// `entity(<type>, <field>)` 的单字段表达式；显式 key 映射 → 映射的来源字段
/// （真正逐实体变化的字段）。
///
/// 多 key（实体 = key 元组）、stats 形态、`entity(...)` 是复合表达式时返回 `None`
/// ——此时"推断"不是单一字段，显式字段不被视为不一致。
fn inferred_entity_field(rule: &RuleDecl, schemas: &[WindowSchema]) -> Option<String> {
    if rule.each_clause.is_some() {
        return match &rule.entity.id_expr {
            Expr::Field(fr) => leaf_name(fr),
            _ => None,
        };
    }
    if let Some(mapping) = &rule.match_clause.key_mapping {
        return match mapping.as_slice() {
            [item] => leaf_name(&item.source_field),
            _ => None,
        };
    }
    match rule.match_clause.keys.as_slice() {
        [key] => {
            let name = leaf_name(key)?;
            // join-then-key（设计 §9.6）：单个 key **不在驱动事件的窗口上**、而由某个 snapshot
            // join 的目标窗提供时，驱动侧的实体是 `entity(...)`，不是这个键——否则 VN23 会把
            // 「显式实体字段 = 驱动侧字段」误判成不一致。
            if is_join_then_key(&name, rule, schemas) {
                return match &rule.entity.id_expr {
                    Expr::Field(fr) => leaf_name(fr),
                    _ => None,
                };
            }
            Some(name)
        }
        _ => None,
    }
}

/// 单个 match key 是否取自 join 侧（引擎 `JoinKeyPlan` 的同口径判据）：该字段不在任何
/// bind 的窗口 schema 上，且恰有一个 `snapshot` join 的目标窗提供它。
fn is_join_then_key(field: &str, rule: &RuleDecl, schemas: &[WindowSchema]) -> bool {
    let schema_of = |name: &str| schemas.iter().find(|schema| schema.name == name);
    let has_field = |schema: &WindowSchema| schema.fields.iter().any(|f| f.name == field);

    for decl in &rule.events.decls {
        if schema_of(&decl.window).is_some_and(has_field) {
            return false;
        }
    }
    let providers = rule
        .joins
        .iter()
        .filter(|join| matches!(join.mode, JoinMode::Snapshot))
        .filter(|join| schema_of(&join.target_window).is_some_and(has_field))
        .count();
    providers == 1
}

/// 字段引用的叶子字段名；无法判定（空串）时 `None`，调用方据此跳过一致性检查。
fn leaf_name(fr: &FieldRef) -> Option<String> {
    let name = field_ref_field_name(fr);
    (!name.is_empty()).then(|| name.to_string())
}

/// 事件组的字段覆盖记录：`use({...})` 与 loader 解析后的 `use from` 同构。
///
/// - 顶层 object → 一条记录（顶层键展开为字段）；
/// - 顶层 object 数组 → 多条记录，生成时按事件序号循环取用（设计 §3.3）。
///
/// 记录条数为 0 表示该来源无法物化（VN17 已报），调用方无需再检查字段。
fn source_records(
    source: &ValueSource,
    errors: &mut Vec<ValidationError>,
    stream: &str,
    idx: usize,
) -> Vec<Vec<FieldPredicate>> {
    match source {
        ValueSource::Predicates(predicates) => vec![predicates.clone()],
        ValueSource::Json(json) => match json {
            serde_json::Value::Object(_) => vec![object_record(json)],
            serde_json::Value::Array(items) => {
                let mut records = Vec::with_capacity(items.len());
                for (record_idx, item) in items.iter().enumerate() {
                    if !item.is_object() {
                        errors.push(ValidationError {
                            code: "VN17",
                            message: format!(
                                "injection case '{}' 第 {} 个事件组 use({{...}}) 的记录数组第 {} 个元素不是 JSON object",
                                stream,
                                idx + 1,
                                record_idx + 1
                            ),
                        });
                        return Vec::new();
                    }
                    records.push(object_record(item));
                }
                if records.is_empty() {
                    errors.push(ValidationError {
                        code: "VN17",
                        message: format!(
                            "injection case '{}' 第 {} 个事件组 use({{...}}) 的记录数组为空",
                            stream,
                            idx + 1
                        ),
                    });
                }
                records
            }
            _ => {
                errors.push(ValidationError {
                    code: "VN17",
                    message: format!(
                        "injection case '{}' 第 {} 个事件组 use({{...}}) 的顶层必须是 JSON object 或 object 数组",
                        stream,
                        idx + 1
                    ),
                });
                Vec::new()
            }
        },
        // loader 应已把 `File` 解析成 `Json`（见 loader::resolve_inject_files）；
        // 未解析时 datagen 会明确报错，这里不重复报。
        ValueSource::File(_) => Vec::new(),
    }
}

/// 一条记录：顶层键展开为字段。
fn object_record(json: &serde_json::Value) -> Vec<FieldPredicate> {
    json_top_level_entries(json)
        .unwrap_or_default()
        .into_iter()
        .map(|(field, value)| FieldPredicate {
            field,
            value: AttrValue::Json(value),
        })
        .collect()
}

/// 注入用例里字段覆盖的公共检查（重名 / 命中实体键字段 / 字段不在 schema）。
///
/// `entity_key_fields` = 生成器会写进事件的**实体键字段**（`match<...>` 的键；`on each` 时
/// 为 `entity(...)` 的单字段；join 块为连接键）。这些字段的值由实体 id 分配，写在 `use` /
/// `without` 里会被静默丢弃，所以报 VN12。
fn check_predicate_fields(
    errors: &mut Vec<ValidationError>,
    stream: &str,
    step_idx: usize,
    step_kind: &str,
    predicates: &[FieldPredicate],
    entity_key_fields: &[String],
    case_schema: Option<&WindowSchema>,
) {
    let mut seen = HashSet::new();
    for pred in predicates {
        if !seen.insert(pred.field.as_str()) {
            errors.push(ValidationError {
                code: "VN9",
                message: format!(
                    "injection case '{}' step {} has duplicate field '{}' in {}",
                    stream, step_idx, pred.field, step_kind
                ),
            });
        }
        if entity_key_fields.iter().any(|field| field == &pred.field) {
            errors.push(ValidationError {
                code: "VN12",
                message: format!(
                    "注入用例 '{}' 第 {} 步的 {} 里写了实体键字段 '{}'：该字段的值由生成器按实体 id 分配，这里给的值会被静默丢弃",
                    stream, step_idx, step_kind, pred.field
                ),
            });
        }
        if let Some(schema) = case_schema
            && !schema.fields.iter().any(|field| field.name == pred.field)
        {
            errors.push(ValidationError {
                code: "VN11",
                message: format!(
                    "injection case '{}' step {} field '{}' not found in schema '{}'",
                    stream, step_idx, pred.field, schema.name
                ),
            });
        }
        // VN35：`use` / `without` / `join` 块里不能写 schema 的时间字段（或内部键
        // `_timestamp`）。生成器按事件时间写这一列（oracle 用 `GenEvent.timestamp`、引擎
        // 按 schema 的 `time_field` 读列时间），用例再给一个值等于造出**双时间轴**：
        // 字段里是被覆盖的值、两个消费方各自按另一条时间读，结果静默不一致。
        if let Some(schema) = case_schema {
            let time_field = schema.time_field.as_deref();
            if time_field == Some(pred.field.as_str()) || pred.field == "_timestamp" {
                errors.push(ValidationError {
                    code: "VN35",
                    message: format!(
                        "注入用例 '{}' 第 {} 步的 {} 里写了时间字段 '{}'：时间字段由生成器按事件时间写入，不能由 use/without 覆盖（会造成双时间轴、静默不一致）",
                        stream, step_idx, step_kind, pred.field
                    ),
                });
            }
        }
    }
}
