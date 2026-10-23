//! Verdicts, change classes, findings and reports (design sections 2 and 6.1).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Proven equivalent with no assumptions beyond the dialect specs.
    Equal,
    /// Proven equivalent if every listed assumption holds.
    EqualUnderAssumptions,
    /// Neither equivalence nor a witness could be established.
    Unknown,
    /// A concrete divergence was found.
    Different,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Equal => "equal",
            Verdict::EqualUnderAssumptions => "equal under assumptions",
            Verdict::Unknown => "unknown",
            Verdict::Different => "different",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeClass {
    Cosmetic,
    Structural,
    Semantic,
    Cardinality,
    Transactional,
    Concurrency,
    Binding,
    Residue,
    Limit,
}

impl ChangeClass {
    pub fn label(self) -> &'static str {
        match self {
            ChangeClass::Cosmetic => "cosmetic",
            ChangeClass::Structural => "structural",
            ChangeClass::Semantic => "semantic",
            ChangeClass::Cardinality => "cardinality",
            ChangeClass::Transactional => "transactional",
            ChangeClass::Concurrency => "concurrency",
            ChangeClass::Binding => "binding",
            ChangeClass::Residue => "residue",
            ChangeClass::Limit => "limit",
        }
    }

    pub fn default_severity(self) -> Severity {
        match self {
            ChangeClass::Transactional => Severity::Critical,
            ChangeClass::Concurrency | ChangeClass::Cardinality | ChangeClass::Binding => {
                Severity::High
            }
            ChangeClass::Semantic | ChangeClass::Residue => Severity::Medium,
            ChangeClass::Limit | ChangeClass::Structural => Severity::Low,
            ChangeClass::Cosmetic => Severity::Info,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Low => "low",
            Severity::Medium => "medium",
            Severity::High => "high",
            Severity::Critical => "critical",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    A,
    B,
}

/// A span in one artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    pub side: Side,
    pub path: String,
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
    pub byte_start: usize,
    pub byte_end: usize,
    /// The first lines of the located text, capped by the excerpt policy.
    pub excerpt: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssumptionSource {
    /// A default the tool assumes when no source states the fact.
    Default,
    /// Declared in source, a pack or project config.
    Declared,
    /// Stated by the author in a source comment (a pack author marker);
    /// open until someone resolves it.
    Author,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assumption {
    /// Stable identifier, e.g. `ASM-TSQL-CALLER-XACT-ABORT-OFF`.
    pub id: String,
    pub text: String,
    pub source: AssumptionSource,
    /// Where the assumption was stated, e.g. `A:12` for side A line 12.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cite: Option<String>,
}

impl Assumption {
    pub fn default(id: &str, text: &str) -> Self {
        Assumption {
            id: id.to_string(),
            text: text.to_string(),
            source: AssumptionSource::Default,
            cite: None,
        }
    }
}

/// One finding card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Stable across reformatting: object + class + rule + normalized anchors.
    pub fingerprint: String,
    /// Rule or engine that produced the finding, e.g. `txn.atomicity`.
    pub rule: String,
    pub class: ChangeClass,
    pub severity: Severity,
    pub verdict: Verdict,
    /// Plain-language witness or description, anchored to both sides.
    pub witness: String,
    pub a: Option<Location>,
    pub b: Option<Location>,
    pub assumptions: Vec<String>,
}

/// Inputs needed to build a fingerprint.
pub struct FingerprintInput<'a> {
    pub object: &'a str,
    pub class: ChangeClass,
    pub rule: &'a str,
    pub anchor_a: &'a str,
    pub anchor_b: &'a str,
}

pub fn fingerprint(input: &FingerprintInput<'_>) -> String {
    let mut h = Sha256::new();
    for part in [
        input.object,
        input.class.label(),
        input.rule,
        input.anchor_a,
        input.anchor_b,
    ] {
        h.update(part.as_bytes());
        h.update([0u8]);
    }
    hex(&h.finalize()[..8])
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactInfo {
    pub path: String,
    pub dialect: String,
    /// Target engine version the analysis assumes.
    pub dialect_version: String,
    /// Object identity: routine name if one is defined, else the file stem.
    pub object: String,
    /// SHA-256 of the text with line endings, BOM and trailing whitespace normalized.
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticInfo {
    pub side: Side,
    pub line: usize,
    pub col: usize,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoiseCounts {
    /// Statements that differ only cosmetically (layout, case, comments, quoting).
    pub cosmetic: usize,
    /// Structural changes that were not listed as findings.
    pub structural: usize,
}

/// Result of a diff or a check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub tool_version: String,
    pub verdict: Verdict,
    pub a: ArtifactInfo,
    pub b: Option<ArtifactInfo>,
    pub findings: Vec<Finding>,
    pub noise: NoiseCounts,
    /// Normalizations applied to reach the verdict.
    pub normalizations: Vec<String>,
    pub assumptions: Vec<Assumption>,
    /// Why the verdict could not be stronger.
    pub unknown_reasons: Vec<String>,
    pub diagnostics: Vec<DiagnosticInfo>,
    /// Findings hidden by review decisions or in-source markers; they do
    /// not count towards [`Report::open_verdict`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suppressed: Vec<Suppressed>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuppressionKind {
    /// Accepted in the review store.
    Review,
    /// A deliberate-difference marker in the source.
    InSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suppressed {
    pub finding: Finding,
    pub kind: SuppressionKind,
    /// Why: the review comment or the marker text.
    pub justification: String,
}

impl Report {
    /// The verdict over the findings that are still open. It equals
    /// `verdict` when nothing is suppressed. When something is, it is never
    /// better than "equal under assumptions": an accepted difference is
    /// still a difference, so suppression never produces "equal".
    pub fn open_verdict(&self) -> Verdict {
        if self.suppressed.is_empty() {
            return self.verdict;
        }
        if self.findings.iter().any(|f| f.verdict == Verdict::Different) {
            Verdict::Different
        } else if self.findings.iter().any(|f| f.verdict == Verdict::Unknown)
            || !self.unknown_reasons.is_empty()
        {
            Verdict::Unknown
        } else {
            Verdict::EqualUnderAssumptions
        }
    }

    pub fn highest_class(&self) -> Option<ChangeClass> {
        self.findings
            .iter()
            .max_by_key(|f| (f.severity, f.class))
            .map(|f| f.class)
    }
}
