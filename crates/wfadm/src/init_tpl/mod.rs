//! `init_tpl` — wf-rules project templates.
//!
//! Every template file from `docker/default_setting/` is embedded via
//! `include_str!` macros.  The public API returns slices of `(path, content)`
//! filtered by [`Scope`].
//!
//! # Example
//!
//! ```ignore
//! use init_tpl::{templates_for, Scope};
//!
//! for (path, content) in templates_for(Scope::Rules) {
//!     // write `content` to `path` in the target project directory
//! }
//! ```

mod scope;
mod templates;

pub use scope::Scope;
use templates::TemplateFile;

use templates::{
    // conf
    CONF_WFUSION,
    // rules
    RULES_BEACONING,
    RULES_DATA_BULK_EXPORT,
    RULES_DATA_UPLOAD,
    RULES_DGA,
    RULES_DNS_TUNNELING,
    RULES_FIRST_SEEN,
    RULES_LATERAL_SPREAD,
    RULES_NEW_ACCOUNT,
    RULES_OFF_HOURS,
    RULES_PASS_THE_HASH,
    RULES_PASSWORD_SPRAYING,
    RULES_PORT_SCAN,
    RULES_PRIVILEGED_ANOMALY,
    RULES_SCAN_LOGIN_XFER,
    RULES_SCHEDULED_TASK,
    RULES_SSH_BRUTE,
    RULES_WEAK_PASSWORD_REDIS,
    // scenarios
    SCENARIOS_ATTACK_CHAIN,
    SCENARIOS_PORT_SCAN,
    SCENARIOS_PORT_SCAN_QUICK,
    SCENARIOS_SSH_BRUTE_FORCE,
    SCENARIOS_SSH_BRUTE_QUICK,
    // schemas
    SCHEMAS_AUTH,
    SCHEMAS_DATA,
    SCHEMAS_DNS,
    SCHEMAS_HTTP,
    SCHEMAS_MANAGEMENT,
    SCHEMAS_NETWORK,
    // scripts
    SCRIPT_RUN,
    SCRIPT_SMOKE,
    // test
    TEST_BATCH_CONFIG,
    // sinks
    TOPO_SINKS_CONN_FILE,
    TOPO_SINKS_DEFAULT,
    TOPO_SINKS_DEFAULTS,
    TOPO_SINKS_DNS,
    TOPO_SINKS_ERROR,
    TOPO_SINKS_HTTP,
    TOPO_SINKS_INSIDER,
    TOPO_SINKS_MANAGEMENT,
    TOPO_SINKS_NETWORK,
    TOPO_SINKS_SECURITY,
    // sources
    TOPO_SOURCES_INGRESS,
    WINDOWS,
};

/// Returns all template files, filtered by the given [`Scope`].
///
/// Files are grouped:
/// - `conf/` — always included
/// - `connectors/` — generated at runtime by `connectors` module
/// - `models/` — included in Normal and Rules scopes
/// - `topology/` — included in Normal and Conf scopes
/// - scripts — always included
pub fn templates_for(scope: Scope) -> &'static [TemplateFile] {
    match scope {
        Scope::Normal => ALL,
        Scope::Rules => RULES_ONLY,
        Scope::Conf => CONF_ONLY,
    }
}

// ── pre-built slices ───────────────────────────────────────────────────

/// All template files (normal/full scope).
const ALL: &[TemplateFile] = &[
    CONF_WFUSION,
    // rules
    RULES_PORT_SCAN,
    RULES_PASSWORD_SPRAYING,
    RULES_SSH_BRUTE,
    RULES_WEAK_PASSWORD_REDIS,
    RULES_FIRST_SEEN,
    RULES_LATERAL_SPREAD,
    RULES_BEACONING,
    RULES_DGA,
    RULES_DNS_TUNNELING,
    RULES_DATA_UPLOAD,
    RULES_PASS_THE_HASH,
    RULES_PRIVILEGED_ANOMALY,
    RULES_SCAN_LOGIN_XFER,
    RULES_NEW_ACCOUNT,
    RULES_SCHEDULED_TASK,
    RULES_DATA_BULK_EXPORT,
    RULES_OFF_HOURS,
    // schemas
    SCHEMAS_AUTH,
    SCHEMAS_DATA,
    SCHEMAS_DNS,
    SCHEMAS_HTTP,
    SCHEMAS_MANAGEMENT,
    SCHEMAS_NETWORK,
    WINDOWS,
    // scenarios
    SCENARIOS_PORT_SCAN,
    SCENARIOS_PORT_SCAN_QUICK,
    SCENARIOS_SSH_BRUTE_FORCE,
    SCENARIOS_SSH_BRUTE_QUICK,
    SCENARIOS_ATTACK_CHAIN,
    // sinks
    TOPO_SINKS_DEFAULTS,
    TOPO_SINKS_DNS,
    TOPO_SINKS_HTTP,
    TOPO_SINKS_INSIDER,
    TOPO_SINKS_MANAGEMENT,
    TOPO_SINKS_NETWORK,
    TOPO_SINKS_SECURITY,
    TOPO_SINKS_DEFAULT,
    TOPO_SINKS_ERROR,
    TOPO_SINKS_CONN_FILE,
    // sources
    TOPO_SOURCES_INGRESS,
    // scripts
    SCRIPT_RUN,
    SCRIPT_SMOKE,
    TEST_BATCH_CONFIG,
];

/// Rules-only scope: models/{rules,schemas,scenarios} + conf + connectors + scripts.
const RULES_ONLY: &[TemplateFile] = &[
    CONF_WFUSION,
    RULES_PORT_SCAN,
    RULES_PASSWORD_SPRAYING,
    RULES_SSH_BRUTE,
    RULES_WEAK_PASSWORD_REDIS,
    RULES_FIRST_SEEN,
    RULES_LATERAL_SPREAD,
    RULES_BEACONING,
    RULES_DGA,
    RULES_DNS_TUNNELING,
    RULES_DATA_UPLOAD,
    RULES_PASS_THE_HASH,
    RULES_PRIVILEGED_ANOMALY,
    RULES_SCAN_LOGIN_XFER,
    RULES_NEW_ACCOUNT,
    RULES_SCHEDULED_TASK,
    RULES_DATA_BULK_EXPORT,
    RULES_OFF_HOURS,
    SCHEMAS_AUTH,
    SCHEMAS_DATA,
    SCHEMAS_DNS,
    SCHEMAS_HTTP,
    SCHEMAS_MANAGEMENT,
    SCHEMAS_NETWORK,
    WINDOWS,
    SCENARIOS_PORT_SCAN,
    SCENARIOS_PORT_SCAN_QUICK,
    SCENARIOS_SSH_BRUTE_FORCE,
    SCENARIOS_SSH_BRUTE_QUICK,
    SCENARIOS_ATTACK_CHAIN,
    SCRIPT_RUN,
    SCRIPT_SMOKE,
    TEST_BATCH_CONFIG,
];

/// Conf-only scope: topology/{sinks,sources} + conf + connectors + scripts.
const CONF_ONLY: &[TemplateFile] = &[
    CONF_WFUSION,
    WINDOWS,
    TOPO_SINKS_DEFAULTS,
    TOPO_SINKS_DNS,
    TOPO_SINKS_HTTP,
    TOPO_SINKS_INSIDER,
    TOPO_SINKS_MANAGEMENT,
    TOPO_SINKS_NETWORK,
    TOPO_SINKS_SECURITY,
    TOPO_SINKS_DEFAULT,
    TOPO_SINKS_ERROR,
    TOPO_SINKS_CONN_FILE,
    TOPO_SOURCES_INGRESS,
    SCRIPT_RUN,
    SCRIPT_SMOKE,
    TEST_BATCH_CONFIG,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_for_normal() {
        let files = templates_for(Scope::Normal);
        assert!(!files.is_empty(), "normal scope should have files");
        assert!(files.iter().any(|(p, _)| *p == "conf/wfusion.toml"));
        assert!(files.iter().any(|(p, _)| p.starts_with("models/")));
        assert!(files.iter().any(|(p, _)| p.starts_with("topology/")));
        assert!(files.iter().any(|(p, _)| *p == "smoke.sh"));
    }

    #[test]
    fn templates_for_rules() {
        let files = templates_for(Scope::Rules);
        assert!(!files.is_empty());
        assert!(files.iter().any(|(p, _)| *p == "conf/wfusion.toml"));
        assert!(files.iter().any(|(p, _)| p.starts_with("models/")));
        assert!(!files.iter().any(|(p, _)| p.starts_with("topology/")));
    }

    #[test]
    fn templates_for_conf() {
        let files = templates_for(Scope::Conf);
        assert!(!files.is_empty());
        assert!(files.iter().any(|(p, _)| *p == "conf/wfusion.toml"));
        assert!(files.iter().any(|(p, _)| p.starts_with("topology/")));
        assert!(
            files
                .iter()
                .any(|(p, _)| *p == "models/schemas/windows.toml")
        );
        assert!(!files.iter().any(|(p, _)| p.starts_with("models/rules/")));
        assert!(
            !files
                .iter()
                .any(|(p, _)| p.starts_with("models/scenarios/"))
        );
    }

    #[test]
    fn templates_all_have_content() {
        for &(path, content) in templates_for(Scope::Normal) {
            assert!(!content.is_empty(), "template {path} is empty");
        }
    }
}
