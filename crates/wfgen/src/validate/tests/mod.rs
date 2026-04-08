mod field;
mod inject;
mod oracle;
mod stream;
mod syntax;

use super::*;
use crate::wfg_ast::*;
use std::time::Duration;
use wf_lang::{BaseType, FieldDef, FieldType, WindowSchema};

/// Helper: build a minimal WfgFile.
fn minimal_wfg(streams: Vec<StreamBlock>, injects: Vec<InjectBlock>) -> WfgFile {
    WfgFile {
        uses: vec![],
        scenario: ScenarioDecl {
            name: "test".into(),
            seed: 1,
            time_clause: TimeClause {
                start: "2024-01-01T00:00:00Z".into(),
                duration: Duration::from_secs(3600),
            },
            total: 100,
            streams,
            injects,
            faults: None,
            oracle: None,
        },
        syntax: None,
    }
}

/// Helper: build a WindowSchema.
fn make_schema(name: &str, fields: Vec<(&str, BaseType)>) -> WindowSchema {
    WindowSchema {
        name: name.into(),
        streams: vec![],
        time_field: None,
        over: Duration::from_secs(300),
        fields: fields
            .into_iter()
            .map(|(n, bt)| FieldDef {
                name: n.into(),
                field_type: FieldType::Base(bt),
            })
            .collect(),
    }
}

/// Helper: build a minimal WflFile with one rule that references given event windows.
///
/// Uses the parser to avoid `#[non_exhaustive]` construction issues.
fn make_wfl(rule_name: &str, event_windows: Vec<(&str, &str)>) -> wf_lang::ast::WflFile {
    let events_str: String = event_windows
        .iter()
        .map(|(alias, window)| format!("        {alias} : {window}"))
        .collect::<Vec<_>>()
        .join("\n");
    let first_alias = event_windows.first().map(|(a, _)| *a).unwrap_or("e");
    let wfl_src = format!(
        r#"rule {rule_name} {{
    events {{
{events_str}
    }}
    match<sip : 1m> {{
        on event {{
            {first_alias} | count >= 1;
        }}
    }}
    -> score(1)
    entity(ip, {first_alias}.sip)
    yield AlertWindow()
}}"#
    );
    wf_lang::parse_wfl(&wfl_src)
        .unwrap_or_else(|e| panic!("make_wfl parse failed: {e}\nsource:\n{wfl_src}"))
}

fn stream(alias: &str, window: &str) -> StreamBlock {
    StreamBlock {
        alias: alias.into(),
        window: window.into(),
        rate: Rate {
            count: 10,
            unit: RateUnit::PerSecond,
        },
        overrides: vec![],
    }
}

fn stream_with_override(alias: &str, window: &str, field: &str, expr: GenExpr) -> StreamBlock {
    StreamBlock {
        alias: alias.into(),
        window: window.into(),
        rate: Rate {
            count: 10,
            unit: RateUnit::PerSecond,
        },
        overrides: vec![FieldOverride {
            field_name: field.into(),
            gen_expr: expr,
        }],
    }
}

fn inject(rule: &str, streams: Vec<&str>) -> InjectBlock {
    InjectBlock {
        rule: rule.into(),
        streams: streams.into_iter().map(|s| s.into()).collect(),
        lines: vec![InjectLine {
            mode: InjectMode::Hit,
            percent: 20.0,
            params: vec![],
            use_steps: vec![],
        }],
    }
}
