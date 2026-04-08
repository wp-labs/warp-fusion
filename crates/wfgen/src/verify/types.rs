/// An actual alert to compare against oracle expectations.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ActualAlert {
    pub rule_name: String,
    pub score: f64,
    pub entity_type: String,
    pub entity_id: String,
    pub origin: String,
    pub fired_at: String,
}

/// Summary statistics of the verify comparison.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VerifySummary {
    pub oracle_total: usize,
    pub actual_total: usize,
    pub matched: usize,
    pub missing: usize,
    pub unexpected: usize,
    pub field_mismatch: usize,
}

/// Detail record for a missing or unexpected alert.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AlertDetail {
    pub rule_name: String,
    pub entity_type: String,
    pub entity_id: String,
    pub score: f64,
    pub time: String,
}

/// Detail record for a score mismatch.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MismatchDetail {
    pub rule_name: String,
    pub entity_type: String,
    pub entity_id: String,
    pub expected_score: f64,
    pub actual_score: f64,
    pub expected_time: String,
    pub actual_time: String,
}

/// Full verification report.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VerifyReport {
    pub status: String,
    pub summary: VerifySummary,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_details: Vec<AlertDetail>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unexpected_details: Vec<AlertDetail>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mismatch_details: Vec<MismatchDetail>,
}

impl VerifyReport {
    /// Render the report as a PR-friendly Markdown table.
    pub fn to_markdown(&self) -> String {
        let mut md = String::new();
        md.push_str("## wfgen Verify Report\n\n");
        md.push_str(&format!("**Status**: {}\n\n", self.status.to_uppercase()));

        // Summary table
        md.push_str("### Summary\n\n");
        md.push_str("| Metric | Count |\n");
        md.push_str("|--------|-------|\n");
        md.push_str(&format!(
            "| Oracle total | {} |\n",
            self.summary.oracle_total
        ));
        md.push_str(&format!(
            "| Actual total | {} |\n",
            self.summary.actual_total
        ));
        md.push_str(&format!("| Matched | {} |\n", self.summary.matched));
        md.push_str(&format!("| Missing | {} |\n", self.summary.missing));
        md.push_str(&format!("| Unexpected | {} |\n", self.summary.unexpected));
        md.push_str(&format!(
            "| Field mismatch | {} |\n",
            self.summary.field_mismatch
        ));

        // Missing details
        if !self.missing_details.is_empty() {
            md.push_str(&format!(
                "\n### Missing ({})\n\n",
                self.missing_details.len()
            ));
            md.push_str("| Rule | Entity | Score | Time |\n");
            md.push_str("|------|--------|-------|------|\n");
            for d in &self.missing_details {
                md.push_str(&format!(
                    "| {} | {}:{} | {:.2} | {} |\n",
                    d.rule_name, d.entity_type, d.entity_id, d.score, d.time
                ));
            }
        }

        // Unexpected details
        if !self.unexpected_details.is_empty() {
            md.push_str(&format!(
                "\n### Unexpected ({})\n\n",
                self.unexpected_details.len()
            ));
            md.push_str("| Rule | Entity | Score | Time |\n");
            md.push_str("|------|--------|-------|------|\n");
            for d in &self.unexpected_details {
                md.push_str(&format!(
                    "| {} | {}:{} | {:.2} | {} |\n",
                    d.rule_name, d.entity_type, d.entity_id, d.score, d.time
                ));
            }
        }

        // Mismatch details
        if !self.mismatch_details.is_empty() {
            md.push_str(&format!(
                "\n### Field Mismatches ({})\n\n",
                self.mismatch_details.len()
            ));
            md.push_str("| Rule | Entity | Expected | Actual | Exp. Time | Act. Time |\n");
            md.push_str("|------|--------|----------|--------|-----------|----------|\n");
            for d in &self.mismatch_details {
                md.push_str(&format!(
                    "| {} | {}:{} | {:.2} | {:.2} | {} | {} |\n",
                    d.rule_name,
                    d.entity_type,
                    d.entity_id,
                    d.expected_score,
                    d.actual_score,
                    d.expected_time,
                    d.actual_time
                ));
            }
        }

        md
    }
}
