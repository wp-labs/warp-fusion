use wf_lang::ast::{BinOp, Expr, FieldRef};
use wf_lang::ast::{FieldAssign, InputStmt, TestBlock};
use wf_lang::plan::RulePlan;

/// 一个反例变体：在原 test input 末尾追加一条违反某 bind guard 的行，
/// 期望 hits 不变（该行被 bind 过滤，不进入窗口状态）。
#[derive(Debug, Clone)]
pub struct NegativeCase {
    /// 人类可读描述，如 `fail: action == "failed" → 反例 action="allowed"`。
    pub desc: String,
    /// 追加了反例行的 input（其余与原 test 完全一致）。
    pub input: Vec<InputStmt>,
}

/// 从一个规则 plan 中收集"简单等值/不等值 guard"反演所需的结构。
struct SimpleGuard {
    /// bind 的 alias（test row 也用同名 alias）。
    alias: String,
    /// guard 引用的字段名（bind 作用域内的裸字段名）。
    field: String,
    /// 比较运算符（Eq / Ne）。
    op: BinOp,
    /// guard 比较的字面量。
    literal: Expr,
}

/// 若 bind filter 是 `field == literal` / `field != literal` 形态，提取之。
/// 复杂 guard（函数、嵌套路径、逻辑组合）返回 None —— 生成器如实跳过。
fn simple_eq_guard(filter: &Expr) -> Option<(String, BinOp, Expr)> {
    match filter {
        Expr::BinOp { op, left, right } if matches!(op, BinOp::Eq | BinOp::Ne) => {
            match (field_name(left), literal_of(right)) {
                (Some(field), Some(lit)) => Some((field, *op, lit)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// 表达式如果是裸字段引用，返回字段名。
fn field_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Field(FieldRef::Simple(name)) => Some(name.clone()),
        _ => None,
    }
}

/// 表达式如果是字面量，返回其克隆（Number/String/Bool）。
fn literal_of(expr: &Expr) -> Option<Expr> {
    match expr {
        Expr::Number(_) | Expr::StringLit(_) | Expr::Bool(_) => Some(expr.clone()),
        _ => None,
    }
}

/// 构造一个违反 guard 的字段值。
/// - Eq  `field == X` → 返回与 X 不同的同型字面量（string: "not-{X}"，number: X+1，bool: !X）。
/// - Ne  `field != X` → 返回 X 本身（违反 != 的最短反例：值等于被排除项）。
fn violating_value(op: BinOp, literal: &Expr) -> Expr {
    if op == BinOp::Ne {
        return literal.clone();
    }
    match literal {
        Expr::StringLit(s) => Expr::StringLit(format!("not-{s}")),
        Expr::Number(n) => Expr::Number(if n.is_sign_positive() { n + 1.0 } else { n - 1.0 }),
        Expr::Bool(b) => Expr::Bool(!b),
        other => other.clone(),
    }
}

/// 为一个 test 生成 bind-guard 反例变体。
///
/// 对 test input 中每条引用 bind alias 的 `row`，若规则存在对应 bind 的
/// 简单 guard（`field == X` / `field != X`），克隆该行并把 guard 字段改为
/// 违反值，追加为一条新行。反例若被 bind 正确过滤 → hits 不变；
/// 若 guard 失效（字段拼错/类型错/比较反）→ 反例漏进 → hits 变化。
///
/// 只处理简单 guard；复杂 guard（函数/嵌套/逻辑组合）如实跳过不生成。
/// 每个产出 case 只追加**一条**反例行（保持原 test 的行数/时序语义可对比）。
pub fn gen_negative_cases(plan: &RulePlan, test: &TestBlock) -> Vec<NegativeCase> {
    let guards = extract_simple_guards(plan);
    gen_cases_for_guards(test, &guards)
}

/// 从 plan 的 binds 中提取简单等值/不等值 guard。
fn extract_simple_guards(plan: &RulePlan) -> Vec<SimpleGuard> {
    plan.binds
        .iter()
        .filter_map(|b| {
            let (field, op, literal) = simple_eq_guard(b.filter.as_ref()?)?;
            Some(SimpleGuard {
                alias: b.alias.clone(),
                field,
                op,
                literal,
            })
        })
        .collect()
}

/// 给定 guard 列表，为 test 生成反例变体（纯逻辑，可单测）。
fn gen_cases_for_guards(test: &TestBlock, guards: &[SimpleGuard]) -> Vec<NegativeCase> {
    if guards.is_empty() {
        return Vec::new();
    }

    let mut cases = Vec::new();

    for stmt in &test.input {
        let InputStmt::Row { alias, fields } = stmt else {
            continue; // 只对 row 反演；tick 时序行不参与
        };

        // 找该 alias 命中的 guard（同一 alias 可能有多条 guard，取第一条
        // 已足够覆盖"反例被过滤"的主路径；多 guard 组合留后续）。
        let Some(g) = guards.iter().find(|g| g.alias == *alias) else {
            continue;
        };

        // 该 row 已显式设置 guard 字段（否则 guard 值来自 schema 默认，
        // 反演无从下手——如实跳过）。
        if !fields.iter().any(|fa| fa.name == g.field) {
            continue;
        }

        let neg_value = violating_value(g.op, &g.literal);
        let op_str = match g.op {
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            _ => "?",
        };
        let lit_str = format_expr_short(&g.literal);
        let neg_str = format_expr_short(&neg_value);

        // 克隆该 row，改 guard 字段为违反值，追加为新行。
        let mut neg_fields: Vec<FieldAssign> = fields.clone();
        if let Some(fa) = neg_fields.iter_mut().find(|fa| fa.name == g.field) {
            fa.value = neg_value.clone();
        }
        let mut new_input = test.input.clone();
        new_input.push(InputStmt::Row {
            alias: alias.clone(),
            fields: neg_fields,
        });

        cases.push(NegativeCase {
            desc: format!(
                "bind `{}`: {} {op_str} {} → 反例 {}= {}（期望被过滤，hits 不变）",
                g.alias, g.field, lit_str, g.field, neg_str
            ),
            input: new_input,
        });
    }

    cases
}

fn format_expr_short(expr: &Expr) -> String {
    match expr {
        Expr::StringLit(s) => format!("\"{s}\""),
        Expr::Number(n) => n.to_string(),
        Expr::Bool(b) => b.to_string(),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wf_lang::parse_wfl;

    fn lit_string(s: &str) -> Expr {
        Expr::StringLit(s.to_string())
    }

    fn guard_eq(alias: &str, field: &str, lit: Expr) -> SimpleGuard {
        SimpleGuard {
            alias: alias.to_string(),
            field: field.to_string(),
            op: BinOp::Eq,
            literal: lit,
        }
    }

    fn guard_ne(alias: &str, field: &str, lit: Expr) -> SimpleGuard {
        SimpleGuard {
            alias: alias.to_string(),
            field: field.to_string(),
            op: BinOp::Ne,
            literal: lit,
        }
    }

    /// 从真实 wfl 源解析出 sample test（绕过 non_exhaustive 构造）。
    fn sample_test() -> TestBlock {
        let src = r#"use "sec.wfs"
rule r1 {
    events { fail : auth_events && action == "failed" }
    match<sip:5m> {
        on event { fail | count >= 3; }
        and close { fail | count >= 1; }
    } -> score(70.0)
    entity(ip, fail.sip)
    yield alerts (sip = fail.sip)
}
test t1 for r1 {
    input {
        row(fail, sip = "10.0.0.1", action = "failed");
    }
    expect { hits == 1; }
}"#;
        let file = parse_wfl(src).expect("parse");
        assert_eq!(file.tests.len(), 1);
        file.tests.into_iter().next().unwrap()
    }

    /// 构造带 N 条同 guard 行的 test（clone sample + 扩展 input）。
    fn sample_test_with_rows(n: usize) -> TestBlock {
        let mut t = sample_test();
        let first = t.input[0].clone();
        while t.input.len() < n {
            t.input.push(first.clone());
        }
        t
    }

    #[test]
    fn eq_guard_generates_one_negative_row() {
        let guards = vec![guard_eq("fail", "action", lit_string("failed"))];
        let test = sample_test();
        let cases = gen_cases_for_guards(&test, &guards);

        assert_eq!(cases.len(), 1);
        let case = &cases[0];
        assert!(case.desc.contains("action"));
        // 反例行追加在末尾（原行 + 反例行 = 2）
        assert_eq!(case.input.len(), 2);
        let last = case.input.last().unwrap();
        let InputStmt::Row { fields, .. } = last else {
            panic!("expected row");
        };
        let action = fields
            .iter()
            .find(|fa| fa.name == "action")
            .expect("action field");
        assert_ne!(action.value, lit_string("failed"));
        // 非 guard 字段（sip）保持原值
        let sip = fields.iter().find(|fa| fa.name == "sip").unwrap();
        assert_eq!(sip.value, lit_string("10.0.0.1"));
    }

    #[test]
    fn row_per_guard_row_generates_row_count_cases() {
        // 2 行 input × 1 guard → 2 个 case（每行各派生一条反例）
        let guards = vec![guard_eq("fail", "action", lit_string("failed"))];
        let cases = gen_cases_for_guards(&sample_test_with_rows(2), &guards);
        assert_eq!(cases.len(), 2);
        for c in &cases {
            assert_eq!(c.input.len(), 3); // 原 2 行 + 1 反例
        }
    }

    #[test]
    fn no_guard_matching_alias_yields_no_cases() {
        let guards = vec![guard_eq("other", "action", lit_string("failed"))];
        assert!(gen_cases_for_guards(&sample_test(), &guards).is_empty());
    }

    #[test]
    fn guard_field_not_set_in_row_is_skipped() {
        // row 没设置 guard 字段（action）——反演无从下手，如实跳过
        let guards = vec![guard_eq("fail", "action", lit_string("failed"))];
        let mut test = sample_test();
        if let InputStmt::Row { fields, .. } = &mut test.input[0] {
            fields.retain(|fa| fa.name != "action");
        }
        assert!(gen_cases_for_guards(&test, &guards).is_empty());
    }

    #[test]
    fn tick_rows_are_preserved_and_not_duplicated() {
        let guards = vec![guard_eq("fail", "action", lit_string("failed"))];
        let mut test = sample_test();
        test.input.push(InputStmt::Tick(std::time::Duration::from_secs(300)));
        let cases = gen_cases_for_guards(&test, &guards);
        assert_eq!(cases.len(), 1);
        // 反例 clone 原 input（row,tick）后 push → 顺序：row, tick, row反例
        let c = &cases[0];
        assert_eq!(c.input.len(), 3);
        assert!(matches!(c.input[0], InputStmt::Row { .. }));
        assert!(matches!(c.input[1], InputStmt::Tick(_)));
        assert!(matches!(c.input[2], InputStmt::Row { .. }));
    }

    #[test]
    fn ne_guard_negative_equals_literal() {
        // guard `field != "blocked"`：反例必须等于 "blocked"（违反 !=），
        // 而不是任意不同值（那会被正确过滤、测不到 guard 失效）。
        let guards = vec![guard_ne("fail", "action", lit_string("blocked"))];
        let test = sample_test();
        let cases = gen_cases_for_guards(&test, &guards);
        assert_eq!(cases.len(), 1);
        let last = cases[0].input.last().unwrap();
        let InputStmt::Row { fields, .. } = last else {
            panic!("expected row");
        };
        let action = fields
            .iter()
            .find(|fa| fa.name == "action")
            .expect("action field");
        assert_eq!(action.value, lit_string("blocked"));
        // 描述反映 != 语义
        assert!(cases[0].desc.contains("!="));
    }

    #[test]
    fn complex_guard_is_not_extracted() {
        // 函数/嵌套 guard 不应被 simple_eq_guard 提取（如实跳过）
        let complex = Expr::FuncCall {
            qualifier: None,
            name: "startswith_any".to_string(),
            args: vec![
                Expr::Field(FieldRef::Simple("action".to_string())),
                Expr::Array(vec![lit_string("fail")]),
            ],
        };
        assert!(simple_eq_guard(&complex).is_none());

        // 左值不是裸字段（函数调用）也不提取
        let not_field = Expr::BinOp {
            op: BinOp::Eq,
            left: Box::new(Expr::FuncCall {
                qualifier: None,
                name: "len".to_string(),
                args: vec![Expr::Field(FieldRef::Simple("action".to_string()))],
            }),
            right: Box::new(lit_string("3")),
        };
        assert!(simple_eq_guard(&not_field).is_none());

        // 右值不是字面量（字段比较）不提取
        let field_cmp = Expr::BinOp {
            op: BinOp::Eq,
            left: Box::new(Expr::Field(FieldRef::Simple("a".to_string()))),
            right: Box::new(Expr::Field(FieldRef::Simple("b".to_string()))),
        };
        assert!(simple_eq_guard(&field_cmp).is_none());
    }
}
