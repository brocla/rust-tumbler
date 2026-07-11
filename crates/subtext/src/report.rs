//! The report — the locked §3 schema.
//!
//! A [`Report`] carries a `checks` array (every vector the tool knows about,
//! each marked passed / warning / leak / skipped — "all the ways it was
//! checked") and a `findings` array ("where the words were found"), plus
//! query-independent `signals`. Risk is computed deterministically from the
//! check outcomes by the §3.3 rubric — no fuzzy judgement.

use crate::query::{Query, QueryMode};
use serde::Serialize;

/// The whole report for one file. Serialized camelCase to match the §3.2 JSON.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub file_name: String,
    pub file_size: u64,
    /// RFC 3339, `chrono::Utc::now().to_rfc3339()`.
    pub generated_at: String,
    pub pages: u32,
    pub query: QueryReport,
    /// Report-level tone.
    pub risk_tone: RiskTone,
    pub risk_score: RiskScore,
    /// One-line verdict (never the bare word "clean" — see §1 / §3.3).
    pub title: String,
    /// Human summary, including the "N vectors inspected" sentence.
    pub description: String,
    /// One entry per registered `VectorCheck`, ALWAYS.
    pub checks: Vec<Check>,
    /// Where the term was found (empty ⇒ no matches).
    pub findings: Vec<Finding>,
    /// Query-independent suspicions (§3.4).
    pub signals: Vec<Signal>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryReport {
    pub terms: Vec<String>,
    pub mode: QueryMode,
    pub case_sensitive: bool,
    pub whole_word: bool,
}

impl QueryReport {
    pub fn from_query(q: &Query) -> Self {
        Self {
            terms: q.terms().to_vec(),
            mode: q.mode(),
            case_sensitive: q.case_sensitive(),
            whole_word: q.whole_word(),
        }
    }
}

/// One row of the "everything we checked" list.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    pub id: &'static str,
    pub label: &'static str,
    pub vector: Vector,
    pub method: &'static str,
    pub tone: CheckTone,
    pub status: CheckStatus,
    /// "1 match on page 4" / "No matches" / a skip reason.
    pub detail: String,
}

/// One place the term was found.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    pub vector: Vector,
    /// "page 4", "Info /Title", "revision 1 · orphan 12 0 R".
    pub location: String,
    pub matched_text: String,
    /// A trimmed snippet around the match.
    pub context: String,
    /// 1-based, when the hit is page-anchored.
    pub page: Option<u32>,
    /// Which superseded revision, when applicable.
    pub revision: Option<u32>,
    /// Embedded-PDF path under `--recurse-embedded` (§3.4).
    pub container: Option<String>,
}

/// A query-independent suspicion.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Signal {
    pub kind: SignalKind,
    pub location: String,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RiskTone {
    Clean,
    Warning,
    Leak,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RiskScore {
    None,
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CheckTone {
    Passed,
    Warning,
    Leak,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CheckStatus {
    Found,
    CheckedClean,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SignalKind {
    UnappliedRedactAnnotation,
    RenderExtractMismatch,
}

/// One `Vector` variant per registered `VectorCheck` (§3.1). The `checks`
/// array is generated from the registry, so this enum can never silently
/// drift from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Vector {
    PageText,
    RenderedOcr,
    Metadata,
    StructureTree,
    MarkedContent,
    Outlines,
    PageLabels,
    Destinations,
    ArticleThreads,
    Annotations,
    RedactionAnnotations,
    Forms,
    Xfa,
    Attachments,
    Scripts,
    Uris,
    OptionalContent,
    Signatures,
    Revisions,
    OrphanObjects,
    RawDecompressed,
}

impl Vector {
    /// The OCR family — checks whose skip is *not* a structural blind spot,
    /// because the content was still fully reachable by extraction (§3.3).
    fn is_ocr_family(self) -> bool {
        matches!(self, Vector::RenderedOcr)
    }
}

impl Report {
    /// Applies the §3.3 rubric to already-populated `checks` / `findings` /
    /// `signals`, filling in `risk_tone`, `risk_score`, `title`, and
    /// `description`. Deterministic — no judgement beyond counting.
    pub fn finalize(&mut self) {
        let total = self.checks.len();
        let skipped = self
            .checks
            .iter()
            .filter(|c| c.status == CheckStatus::Skipped);
        let structural_skips = skipped.clone().filter(|c| !c.vector.is_ocr_family()).count();
        let ocr_skips = skipped.filter(|c| c.vector.is_ocr_family()).count();
        let signals = self.signals.len();
        let terms = terms_phrase(&self.query.terms);

        let (tone, score, title, description) = if !self.findings.is_empty() {
            let vectors_hit = distinct_finding_vectors(&self.findings);
            (
                RiskTone::Leak,
                RiskScore::High,
                "Redacted text is still recoverable.".to_string(),
                format!(
                    "Found {terms} across {vectors_hit} of {total} inspected vectors. The redacted term is recoverable."
                ),
            )
        } else if structural_skips > 0 || signals > 0 {
            let mut reasons = Vec::new();
            if structural_skips > 0 {
                reasons.push(format!("{structural_skips} vector(s) could not be fully inspected"));
            }
            if signals > 0 {
                reasons.push(format!("{signals} suspicious signal(s) fired"));
            }
            let reasons = reasons.join(" and ");
            (
                RiskTone::Warning,
                RiskScore::Medium,
                format!("No matches found, but {reasons}."),
                format!(
                    "No matches for {terms} in the vectors that ran, but {reasons} — the file cannot be certified. See the skipped checks and signals below."
                ),
            )
        } else if ocr_skips > 0 {
            (
                RiskTone::Warning,
                RiskScore::Low,
                "No matches in any inspected text vector; the image/OCR pass was not run.".to_string(),
                format!(
                    "No matches for {terms} across all inspected text vectors. The rendered-image OCR pass was not run, so text baked into images was not examined (re-run with --ocr)."
                ),
            )
        } else {
            (
                RiskTone::Clean,
                RiskScore::None,
                format!("No matches found across all {total} inspected vectors."),
                format!(
                    "No matches for {terms} across all {total} inspected vectors. This states what was checked, not that the file is safe — see the full checks list."
                ),
            )
        };

        self.risk_tone = tone;
        self.risk_score = score;
        self.title = title;
        self.description = description;
    }
}

/// Count of distinct vectors that produced a finding.
fn distinct_finding_vectors(findings: &[Finding]) -> usize {
    let mut v: Vec<u32> = findings.iter().map(|f| f.vector as u32).collect();
    v.sort_unstable();
    v.dedup();
    v.len()
}

fn terms_phrase(terms: &[String]) -> String {
    match terms {
        [one] => format!("\"{one}\""),
        many => {
            let quoted: Vec<String> = many.iter().map(|t| format!("\"{t}\"")).collect();
            quoted.join(", ")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(vector: Vector, status: CheckStatus) -> Check {
        let tone = match status {
            CheckStatus::Found => CheckTone::Leak,
            CheckStatus::CheckedClean => CheckTone::Passed,
            CheckStatus::Skipped => CheckTone::Skipped,
        };
        Check {
            id: "x",
            label: "X",
            vector,
            method: "test",
            tone,
            status,
            detail: String::new(),
        }
    }

    fn report_with(checks: Vec<Check>, findings: Vec<Finding>, signals: Vec<Signal>) -> Report {
        let mut r = Report {
            file_name: "f.pdf".to_string(),
            file_size: 0,
            generated_at: "now".to_string(),
            pages: 0,
            query: QueryReport {
                terms: vec!["Zanzibar".to_string()],
                mode: QueryMode::Literal,
                case_sensitive: false,
                whole_word: false,
            },
            risk_tone: RiskTone::Clean,
            risk_score: RiskScore::None,
            title: String::new(),
            description: String::new(),
            checks,
            findings,
            signals,
        };
        r.finalize();
        r
    }

    fn finding(vector: Vector) -> Finding {
        Finding {
            vector,
            location: "page 1".to_string(),
            matched_text: "Zanzibar".to_string(),
            context: "…Zanzibar…".to_string(),
            page: Some(1),
            revision: None,
            container: None,
        }
    }

    #[test]
    fn any_finding_forces_high_leak() {
        let r = report_with(
            vec![
                check(Vector::PageText, CheckStatus::Found),
                check(Vector::Metadata, CheckStatus::CheckedClean),
            ],
            vec![finding(Vector::PageText)],
            vec![],
        );
        assert_eq!(r.risk_tone, RiskTone::Leak);
        assert_eq!(r.risk_score, RiskScore::High);
    }

    #[test]
    fn all_clean_no_skips_is_none_clean() {
        let r = report_with(
            vec![
                check(Vector::PageText, CheckStatus::CheckedClean),
                check(Vector::Metadata, CheckStatus::CheckedClean),
            ],
            vec![],
            vec![],
        );
        assert_eq!(r.risk_tone, RiskTone::Clean);
        assert_eq!(r.risk_score, RiskScore::None);
        // Honesty rule: never the bare word "clean" as certification.
        assert!(r.title.contains("inspected vectors"));
    }

    #[test]
    fn structural_skip_is_medium_warning() {
        let r = report_with(
            vec![
                check(Vector::PageText, CheckStatus::CheckedClean),
                check(Vector::Metadata, CheckStatus::Skipped),
            ],
            vec![],
            vec![],
        );
        assert_eq!(r.risk_tone, RiskTone::Warning);
        assert_eq!(r.risk_score, RiskScore::Medium);
    }

    #[test]
    fn only_ocr_skip_is_low_warning() {
        let r = report_with(
            vec![
                check(Vector::PageText, CheckStatus::CheckedClean),
                check(Vector::RenderedOcr, CheckStatus::Skipped),
            ],
            vec![],
            vec![],
        );
        assert_eq!(r.risk_tone, RiskTone::Warning);
        assert_eq!(r.risk_score, RiskScore::Low);
    }

    #[test]
    fn signal_alone_lifts_clean_to_medium_warning() {
        let r = report_with(
            vec![check(Vector::PageText, CheckStatus::CheckedClean)],
            vec![],
            vec![Signal {
                kind: SignalKind::UnappliedRedactAnnotation,
                location: "page 7".to_string(),
                detail: "…".to_string(),
            }],
        );
        assert_eq!(r.risk_tone, RiskTone::Warning);
        assert_eq!(r.risk_score, RiskScore::Medium);
    }
}
