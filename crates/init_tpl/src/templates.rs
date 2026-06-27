//! Template definitions — embedded via `include_str!` macros.
//!
//! Each template file from `docker/default_setting/` is embedded at compile time.
//! The macros resolve paths relative to this source file (`crates/init_tpl/src/`).

/// A single template file: (relative_path, content).
pub(crate) type TemplateFile = (&'static str, &'static str);

// ── conf ───────────────────────────────────────────────────────────────

pub(crate) const CONF_WFUSION: TemplateFile = (
    "conf/wfusion.toml",
    include_str!("../conf/wfusion.toml"),
);

// ── models / rules ─────────────────────────────────────────────────────

pub(crate) const RULES_PORT_SCAN: TemplateFile = (
    "models/rules/01-recon/port_scan.wfl",
    include_str!("../models/rules/01-recon/port_scan.wfl"),
);
pub(crate) const RULES_PASSWORD_SPRAYING: TemplateFile = (
    "models/rules/02-initial_access/password_spraying.wfl",
    include_str!(
        "../models/rules/02-initial_access/password_spraying.wfl"
    ),
);
pub(crate) const RULES_SSH_BRUTE: TemplateFile = (
    "models/rules/02-initial_access/ssh_brute_force.wfl",
    include_str!(
        "../models/rules/02-initial_access/ssh_brute_force.wfl"
    ),
);
pub(crate) const RULES_WEAK_PASSWORD_REDIS: TemplateFile = (
    "models/rules/02-initial_access/weak_password_redis.wfl",
    include_str!(
        "../models/rules/02-initial_access/weak_password_redis.wfl"
    ),
);
pub(crate) const RULES_FIRST_SEEN: TemplateFile = (
    "models/rules/03-lateral_movement/first_seen_relationship.wfl",
    include_str!(
        "../models/rules/03-lateral_movement/first_seen_relationship.wfl"
    ),
);
pub(crate) const RULES_LATERAL_SPREAD: TemplateFile = (
    "models/rules/03-lateral_movement/lateral_spread.wfl",
    include_str!(
        "../models/rules/03-lateral_movement/lateral_spread.wfl"
    ),
);
pub(crate) const RULES_BEACONING: TemplateFile = (
    "models/rules/04-c2/beaconing.wfl",
    include_str!("../models/rules/04-c2/beaconing.wfl"),
);
pub(crate) const RULES_DGA: TemplateFile = (
    "models/rules/04-c2/dga_domain.wfl",
    include_str!("../models/rules/04-c2/dga_domain.wfl"),
);
pub(crate) const RULES_DNS_TUNNELING: TemplateFile = (
    "models/rules/04-c2/dns_tunneling.wfl",
    include_str!("../models/rules/04-c2/dns_tunneling.wfl"),
);
pub(crate) const RULES_DATA_UPLOAD: TemplateFile = (
    "models/rules/05-exfiltration/data_upload.wfl",
    include_str!("../models/rules/05-exfiltration/data_upload.wfl"),
);
pub(crate) const RULES_PASS_THE_HASH: TemplateFile = (
    "models/rules/06-credential_abuse/pass_the_hash.wfl",
    include_str!(
        "../models/rules/06-credential_abuse/pass_the_hash.wfl"
    ),
);
pub(crate) const RULES_PRIVILEGED_ANOMALY: TemplateFile = (
    "models/rules/06-credential_abuse/privileged_anomaly.wfl",
    include_str!(
        "../models/rules/06-credential_abuse/privileged_anomaly.wfl"
    ),
);
pub(crate) const RULES_SCAN_LOGIN_XFER: TemplateFile = (
    "models/rules/07-chains/scan_login_xfer.wfl",
    include_str!("../models/rules/07-chains/scan_login_xfer.wfl"),
);
pub(crate) const RULES_NEW_ACCOUNT: TemplateFile = (
    "models/rules/08-persistence/new_account.wfl",
    include_str!("../models/rules/08-persistence/new_account.wfl"),
);
pub(crate) const RULES_SCHEDULED_TASK: TemplateFile = (
    "models/rules/08-persistence/scheduled_task.wfl",
    include_str!("../models/rules/08-persistence/scheduled_task.wfl"),
);
pub(crate) const RULES_DATA_BULK_EXPORT: TemplateFile = (
    "models/rules/09-insider/data_bulk_export.wfl",
    include_str!("../models/rules/09-insider/data_bulk_export.wfl"),
);
pub(crate) const RULES_OFF_HOURS: TemplateFile = (
    "models/rules/09-insider/off_hours_activity.wfl",
    include_str!("../models/rules/09-insider/off_hours_activity.wfl"),
);

// ── models / schemas ───────────────────────────────────────────────────

pub(crate) const SCHEMAS_AUTH: TemplateFile = (
    "models/schemas/auth.wfs",
    include_str!("../models/schemas/auth.wfs"),
);
pub(crate) const SCHEMAS_DATA: TemplateFile = (
    "models/schemas/data.wfs",
    include_str!("../models/schemas/data.wfs"),
);
pub(crate) const SCHEMAS_DNS: TemplateFile = (
    "models/schemas/dns.wfs",
    include_str!("../models/schemas/dns.wfs"),
);
pub(crate) const SCHEMAS_HTTP: TemplateFile = (
    "models/schemas/http.wfs",
    include_str!("../models/schemas/http.wfs"),
);
pub(crate) const SCHEMAS_MANAGEMENT: TemplateFile = (
    "models/schemas/management.wfs",
    include_str!("../models/schemas/management.wfs"),
);
pub(crate) const SCHEMAS_NETWORK: TemplateFile = (
    "models/schemas/network.wfs",
    include_str!("../models/schemas/network.wfs"),
);

// ── models / scenarios ─────────────────────────────────────────────────

pub(crate) const SCENARIOS_PORT_SCAN: TemplateFile = (
    "models/scenarios/port_scan.wfg",
    include_str!("../models/scenarios/port_scan.wfg"),
);
pub(crate) const SCENARIOS_PORT_SCAN_QUICK: TemplateFile = (
    "models/scenarios/port_scan_quick.wfg",
    include_str!("../models/scenarios/port_scan_quick.wfg"),
);
pub(crate) const SCENARIOS_SSH_BRUTE_FORCE: TemplateFile = (
    "models/scenarios/ssh_brute_force.wfg",
    include_str!("../models/scenarios/ssh_brute_force.wfg"),
);
pub(crate) const SCENARIOS_SSH_BRUTE_QUICK: TemplateFile = (
    "models/scenarios/ssh_brute_quick.wfg",
    include_str!("../models/scenarios/ssh_brute_quick.wfg"),
);
pub(crate) const SCENARIOS_ATTACK_CHAIN: TemplateFile = (
    "models/scenarios/attack_chain.wfg.bak",
    include_str!("../models/scenarios/attack_chain.wfg.bak"),
);

// ── topology / sinks ───────────────────────────────────────────────────

pub(crate) const TOPO_SINKS_DEFAULTS: TemplateFile = (
    "topology/sinks/defaults.toml",
    include_str!("../topology/sinks/defaults.toml"),
);
pub(crate) const TOPO_SINKS_DNS: TemplateFile = (
    "topology/sinks/business.d/dns.toml",
    include_str!("../topology/sinks/business.d/dns.toml"),
);
pub(crate) const TOPO_SINKS_HTTP: TemplateFile = (
    "topology/sinks/business.d/http.toml",
    include_str!("../topology/sinks/business.d/http.toml"),
);
pub(crate) const TOPO_SINKS_INSIDER: TemplateFile = (
    "topology/sinks/business.d/insider.toml",
    include_str!("../topology/sinks/business.d/insider.toml"),
);
pub(crate) const TOPO_SINKS_MANAGEMENT: TemplateFile = (
    "topology/sinks/business.d/management.toml",
    include_str!("../topology/sinks/business.d/management.toml"),
);
pub(crate) const TOPO_SINKS_NETWORK: TemplateFile = (
    "topology/sinks/business.d/network.toml",
    include_str!("../topology/sinks/business.d/network.toml"),
);
pub(crate) const TOPO_SINKS_SECURITY: TemplateFile = (
    "topology/sinks/business.d/security.toml",
    include_str!("../topology/sinks/business.d/security.toml"),
);
pub(crate) const TOPO_SINKS_DEFAULT: TemplateFile = (
    "topology/sinks/infra.d/default.toml",
    include_str!("../topology/sinks/infra.d/default.toml"),
);
pub(crate) const TOPO_SINKS_ERROR: TemplateFile = (
    "topology/sinks/infra.d/error.toml",
    include_str!("../topology/sinks/infra.d/error.toml"),
);
pub(crate) const TOPO_SINKS_CONN_FILE: TemplateFile = (
    "topology/sinks/connectors/sink.d/file.toml",
    include_str!("../topology/sinks/connectors/sink.d/file.toml"),
);

// ── topology / sources ─────────────────────────────────────────────────

pub(crate) const TOPO_SOURCES_INGRESS: TemplateFile = (
    "topology/sources/ingress.toml",
    include_str!("../topology/sources/ingress.toml"),
);

// ── scripts ────────────────────────────────────────────────────────────

pub(crate) const SCRIPT_RUN: TemplateFile = (
    "run.sh",
    include_str!("../run.sh"),
);
pub(crate) const SCRIPT_SMOKE: TemplateFile = (
    "smoke.sh",
    include_str!("../smoke.sh"),
);
