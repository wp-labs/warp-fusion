//! Project scope — controls which template files are included during init.

use std::str::FromStr;

/// Scope controls which subset of templates is included during `wfadm init`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// All template files (models + topology + conf + connectors)
    Normal,
    /// Rules only: models/{rules,schemas,scenarios} + conf + connectors
    Rules,
    /// Topology & conf only: topology/{sinks,sources} + conf + connectors
    Conf,
}

impl FromStr for Scope {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "full" | "normal" => Ok(Self::Normal),
            "rules" => Ok(Self::Rules),
            "conf" => Ok(Self::Conf),
            _ => Err(format!("unknown scope: '{s}'. Valid: normal, rules, conf")),
        }
    }
}
