use std::time::Duration;

// ---------------------------------------------------------------------------
// Top-level
// ---------------------------------------------------------------------------

/// A complete `.wfg` scenario file.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct WfgFile {
    pub uses: Vec<UseDecl>,
    pub scenario: ScenarioDecl,
    /// Parsed new syntax section when the file uses new stream-first syntax.
    /// Legacy `.wfg` files keep this as `None`.
    pub syntax: Option<SyntaxScenario>,
}

/// `use "path.wfs"` or `use "path.wfl"`
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct UseDecl {
    pub path: String,
}

// ---------------------------------------------------------------------------
// Scenario
// ---------------------------------------------------------------------------

/// `scenario NAME seed NUMBER { ... }`
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ScenarioDecl {
    pub name: String,
    pub seed: u64,
    pub time_clause: TimeClause,
    pub total: u64,
    pub streams: Vec<StreamBlock>,
    pub injects: Vec<InjectBlock>,
    pub faults: Option<FaultsBlock>,
    pub oracle: Option<OracleBlock>,
}

/// `time "ISO8601" duration DURATION`
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct TimeClause {
    pub start: String,
    pub duration: Duration,
}

// ---------------------------------------------------------------------------
// new syntax (stream-first) extension
// ---------------------------------------------------------------------------

/// new syntax scenario data parsed from the new syntax.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct SyntaxScenario {
    /// `#[key=value, ...]` attributes attached to this scenario.
    pub attrs: Vec<ScenarioAttr>,
    /// `scenario name<k=v, ...>` inline annotations.
    pub inline_annos: Vec<ScenarioAttr>,
    pub traffic: TrafficBlock,
    pub injection: Option<SyntaxInjectionBlock>,
    pub expect: Option<ExpectBlock>,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ScenarioAttr {
    pub key: String,
    pub value: AttrValue,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum AttrValue {
    Number(f64),
    Duration(Duration),
    String(String),
    Bool(bool),
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct TrafficBlock {
    pub streams: Vec<SyntaxStreamDecl>,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct SyntaxStreamDecl {
    pub stream: String,
    pub rate: RateExpr,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum RateExpr {
    Constant(Rate),
    Wave {
        base: Rate,
        amp: Rate,
        period: Duration,
        shape: WaveShape,
    },
    Burst {
        base: Rate,
        peak: Rate,
        every: Duration,
        hold: Duration,
    },
    Timeline(Vec<TimelineSegment>),
}

impl RateExpr {
    /// Fallback EPS approximation used while datagen still relies on legacy fields.
    pub fn approx_eps(&self) -> f64 {
        match self {
            RateExpr::Constant(r) => r.events_per_second(),
            RateExpr::Wave { base, .. } => base.events_per_second(),
            RateExpr::Burst { base, .. } => base.events_per_second(),
            RateExpr::Timeline(segments) => segments
                .first()
                .map(|s| s.rate.events_per_second())
                .unwrap_or(0.0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WaveShape {
    Sine,
    Triangle,
    Square,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct TimelineSegment {
    pub start: Duration,
    pub end: Duration,
    pub rate: Rate,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct SyntaxInjectionBlock {
    pub cases: Vec<SyntaxInjectCase>,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct SyntaxInjectCase {
    pub mode: InjectCaseMode,
    pub percent: f64,
    pub stream: String,
    pub seq: SeqBlock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum InjectCaseMode {
    Hit,
    NearMiss,
    Miss,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct SeqBlock {
    pub entity: String,
    pub steps: Vec<SeqStep>,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum SeqStep {
    Use {
        /// `true` when the step is explicitly prefixed with `then`.
        then_from_prev: bool,
        predicates: Vec<FieldPredicate>,
        count: u64,
        within: Duration,
    },
    Not {
        predicates: Vec<FieldPredicate>,
        within: Duration,
    },
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct FieldPredicate {
    pub field: String,
    pub value: AttrValue,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ExpectBlock {
    pub checks: Vec<ExpectCheck>,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ExpectCheck {
    pub metric: ExpectMetric,
    pub rule: String,
    pub op: CompareOp,
    pub value: ExpectValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExpectMetric {
    Hit,
    NearMiss,
    Miss,
    Precision,
    Recall,
    Fpr,
    LatencyP95,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CompareOp {
    Gte,
    Lte,
    Gt,
    Lt,
    Eq,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ExpectValue {
    Percent(f64),
    Number(f64),
    Duration(Duration),
}

// ---------------------------------------------------------------------------
// Rate
// ---------------------------------------------------------------------------

/// Event rate, e.g. `100/s`, `50/m`, `10/h`
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Rate {
    pub count: u64,
    pub unit: RateUnit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RateUnit {
    PerSecond,
    PerMinute,
    PerHour,
}

impl Rate {
    /// Convert rate to events per second.
    pub fn events_per_second(&self) -> f64 {
        match self.unit {
            RateUnit::PerSecond => self.count as f64,
            RateUnit::PerMinute => self.count as f64 / 60.0,
            RateUnit::PerHour => self.count as f64 / 3600.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Stream
// ---------------------------------------------------------------------------

/// Stream declaration.
///
/// Supported forms:
/// - `stream ALIAS : WINDOW RATE { field_override* }` (legacy)
/// - `stream ALIAS from WINDOW rate RATE { field_override* }` (readable)
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct StreamBlock {
    pub alias: String,
    pub window: String,
    pub rate: Rate,
    pub overrides: Vec<FieldOverride>,
}

/// `FIELD_NAME = gen_expr`
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct FieldOverride {
    pub field_name: String,
    pub gen_expr: GenExpr,
}

/// Generator expression for a field override.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum GenExpr {
    StringLit(String),
    NumberLit(f64),
    BoolLit(bool),
    GenFunc { name: String, args: Vec<GenArg> },
}

/// A gen function argument, optionally named.
///
/// Supports both positional `ipv4(500)` and named `ipv4(pool: 500)` syntax.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct GenArg {
    pub name: Option<String>,
    pub value: GenExpr,
}

impl GenArg {
    pub fn positional(value: GenExpr) -> Self {
        Self { name: None, value }
    }

    pub fn named(name: impl Into<String>, value: GenExpr) -> Self {
        Self {
            name: Some(name.into()),
            value,
        }
    }
}

// ---------------------------------------------------------------------------
// Inject
// ---------------------------------------------------------------------------

/// `inject for RULE on [STREAM, ...] { inject_line* }`
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct InjectBlock {
    pub rule: String,
    pub streams: Vec<String>,
    pub lines: Vec<InjectLine>,
}

/// Inject line.
///
/// Supported forms:
/// - inline params: `MODE PERCENT% key=value key2=value2;`
/// - block params: `MODE PERCENT% { key=value; key2=value2; };`
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct InjectLine {
    pub mode: InjectMode,
    pub percent: f64,
    pub params: Vec<ParamAssign>,
    /// Step-local `use(...)` predicates from new syntax.
    ///
    /// Legacy inject syntax does not populate this field.
    pub use_steps: Vec<InjectUseStep>,
}

/// One `use(...) with(count, within)` step captured for inject generation.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct InjectUseStep {
    pub count: u64,
    pub predicates: Vec<FieldPredicate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum InjectMode {
    Hit,
    NearMiss,
    NonHit,
}

// ---------------------------------------------------------------------------
// Faults
// ---------------------------------------------------------------------------

/// `faults { fault_line* }`
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct FaultsBlock {
    pub faults: Vec<FaultLine>,
}

/// Supported fault types for temporal perturbation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FaultType {
    /// Swap adjacent events' arrival order.
    OutOfOrder,
    /// Delay event arrival position (across watermark boundary).
    Late,
    /// Clone event and insert a duplicate.
    Duplicate,
    /// Remove event from the output stream.
    Drop,
}

impl std::fmt::Display for FaultType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FaultType::OutOfOrder => write!(f, "out_of_order"),
            FaultType::Late => write!(f, "late"),
            FaultType::Duplicate => write!(f, "duplicate"),
            FaultType::Drop => write!(f, "drop"),
        }
    }
}

/// `FAULT_TYPE PERCENT%`
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct FaultLine {
    pub fault_type: FaultType,
    pub percent: f64,
}

// ---------------------------------------------------------------------------
// Oracle
// ---------------------------------------------------------------------------

/// `oracle { param_assigns }`
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct OracleBlock {
    pub params: Vec<ParamAssign>,
}

// ---------------------------------------------------------------------------
// Shared
// ---------------------------------------------------------------------------

/// `NAME = VALUE`
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ParamAssign {
    pub name: String,
    pub value: ParamValue,
}

/// Value in a parameter assignment.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ParamValue {
    Number(f64),
    Duration(Duration),
    String(String),
}
