use std::collections::HashSet;

use crate::error::{self, WfgenReason, WfgenResult};
use crate::wfg_ast::{ExpectBlock, WfgFile};

pub fn unique_expected_rule(expect: &ExpectBlock) -> Option<&str> {
    let mut rules = expect.checks.iter().map(|check| check.rule.as_str());
    let first = rules.next()?;
    if rules.all(|rule| rule == first) {
        Some(first)
    } else {
        None
    }
}

pub fn injected_rule_names(wfg: &WfgFile) -> WfgenResult<HashSet<String>> {
    if let Some(syntax) = wfg.syntax.as_ref() {
        let default_rule = syntax
            .expect
            .as_ref()
            .and_then(unique_expected_rule)
            .map(str::to_string);

        let Some(injection) = syntax.injection.as_ref() else {
            return Ok(HashSet::new());
        };

        let mut rules = HashSet::new();
        for case in &injection.cases {
            if let Some(rule) = case.target_rule.as_deref().or(default_rule.as_deref()) {
                rules.insert(rule.to_string());
                continue;
            }

            return error::fail(
                WfgenReason::Validation,
                format!(
                    "injection case '{}' requires 'for RULE' because expect does not identify a unique target rule",
                    case.stream
                ),
            );
        }
        return Ok(rules);
    }

    Ok(wfg
        .scenario
        .injects
        .iter()
        .map(|inject| inject.rule.clone())
        .collect())
}
