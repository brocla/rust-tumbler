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

/// Why a check was skipped — the categories are scored differently by the
/// risk rubric (§3.3, §14.2). `Unavailable` is a *per-file* blind spot
/// (encryption without a password, an unsupported filter, an unreadable
/// catalog): it caps a no-match report at warning/medium, because the term
/// could be hiding in a place this file prevented us from inspecting.
/// `NotRequested` means the tool *can* run the pass but the user opted out
/// this run (`--ocr` / `--recurse-embedded` not passed): disclosed, scored
/// low. `NotImplemented` is a *tool-phase* limitation (an extractor that has
/// not shipped in this build): disclosed honestly, but it does not imply the
/// file is suspicious, so it must not force every clean file to warning/medium.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipKind {
    Unavailable,
    NotRequested,
    NotImplemented,
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
    /// Why this check was skipped, when it was. Internal to risk scoring — not
    /// serialized (the `status`/`tone`/`detail` fields carry the skip to the
    /// JSON; this only steers the §3.3 rubric).
    #[serde(skip)]
    pub skip_kind: Option<SkipKind>,
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

impl Report {
    /// Applies the §3.3 rubric to already-populated `checks` / `findings` /
    /// `signals`, filling in `risk_tone`, `risk_score`, `title`, and
    /// `description`. Deterministic — no judgement beyond counting.
    pub fn finalize(&mut self) {
        let total = self.checks.len();
        // Partition the skips in one pass (§14.2):
        //  - `unavailable_skips`: a per-file blind spot (encryption, unsupported
        //    filter, unreadable catalog) → caps a no-match report at warning.
        //  - `declined_skips`: a pass the user opted out of this run
        //    (--ocr / --recurse-embedded not passed). Disclosed, scored low.
        //  - `unbuilt_skips`: a tool-phase limitation — an extractor not shipped
        //    in this build. Disclosed, but not evidence the file is suspicious.
        let mut unavailable_skips = 0usize;
        let mut declined_skips = 0usize;
        let mut unbuilt_skips = 0usize;
        for c in &self.checks {
            match c.skip_kind {
                Some(SkipKind::Unavailable) => unavailable_skips += 1,
                Some(SkipKind::NotRequested) => declined_skips += 1,
                Some(SkipKind::NotImplemented) => unbuilt_skips += 1,
                None => {}
            }
        }
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
        } else if unavailable_skips > 0 || signals > 0 {
            // The file itself blocked inspection of some vector, or a
            // query-independent signal fired: cannot certify.
            let mut reasons = Vec::new();
            if unavailable_skips > 0 {
                reasons.push(format!("{unavailable_skips} vector(s) could not be inspected in this file"));
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
        } else if unbuilt_skips > 0 || declined_skips > 0 {
            // Everything that ran came back clean, but coverage was not full:
            // some vectors are not built in this binary, or the user opted out
            // of an optional pass this run. Name each cause precisely (§14.2).
            let mut causes = Vec::new();
            if declined_skips > 0 {
                causes.push(format!(
                    "{declined_skips} optional pass(es) not run this time (see the skipped checks for the flag to re-run with)"
                ));
            }
            if unbuilt_skips > 0 {
                causes.push(format!("{unbuilt_skips} vector(s) not yet implemented"));
            }
            let causes = causes.join(" and ");
            (
                RiskTone::Warning,
                RiskScore::Low,
                format!("No matches in any inspected vector; {causes}."),
                format!(
                    "No matches for {terms} across every vector that ran. However, {causes}, so this is not a full-coverage clean — see the skipped checks below."
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
        // Default a skip to Unavailable (per-file blind spot); tests that need a
        // NotImplemented skip use `skip_check`.
        let kind = (status == CheckStatus::Skipped).then_some(SkipKind::Unavailable);
        make_check(vector, status, kind)
    }

    fn skip_check(vector: Vector, kind: SkipKind) -> Check {
        make_check(vector, CheckStatus::Skipped, Some(kind))
    }

    fn make_check(vector: Vector, status: CheckStatus, skip_kind: Option<SkipKind>) -> Check {
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
            skip_kind,
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
    fn unavailable_skip_is_medium_warning() {
        // A per-file blind spot (e.g. an unreadable catalog) caps at Medium.
        let r = report_with(
            vec![
                check(Vector::PageText, CheckStatus::CheckedClean),
                skip_check(Vector::Metadata, SkipKind::Unavailable),
            ],
            vec![],
            vec![],
        );
        assert_eq!(r.risk_tone, RiskTone::Warning);
        assert_eq!(r.risk_score, RiskScore::Medium);
    }

    #[test]
    fn only_not_implemented_skips_is_low_warning() {
        // Not-yet-built vectors (the phased-build state) → Low, never Medium,
        // and never forced above a genuinely clean-so-far file.
        let r = report_with(
            vec![
                check(Vector::PageText, CheckStatus::CheckedClean),
                skip_check(Vector::RenderedOcr, SkipKind::NotImplemented),
                skip_check(Vector::Revisions, SkipKind::NotImplemented),
            ],
            vec![],
            vec![],
        );
        assert_eq!(r.risk_tone, RiskTone::Warning);
        assert_eq!(r.risk_score, RiskScore::Low);
    }

    #[test]
    fn only_not_requested_skips_is_low_warning() {
        // An optional pass the user declined this run (--ocr /
        // --recurse-embedded not passed) → Low, same bucket as NotImplemented,
        // and the wording points at the skipped checks (§14.2).
        let r = report_with(
            vec![
                check(Vector::PageText, CheckStatus::CheckedClean),
                skip_check(Vector::Attachments, SkipKind::NotRequested),
            ],
            vec![],
            vec![],
        );
        assert_eq!(r.risk_tone, RiskTone::Warning);
        assert_eq!(r.risk_score, RiskScore::Low);
        assert!(r.title.contains("optional pass"), "{}", r.title);
    }

    #[test]
    fn not_requested_skip_does_not_mask_a_real_unavailable_skip() {
        let r = report_with(
            vec![
                check(Vector::PageText, CheckStatus::CheckedClean),
                skip_check(Vector::Attachments, SkipKind::NotRequested),
                skip_check(Vector::Metadata, SkipKind::Unavailable),
            ],
            vec![],
            vec![],
        );
        assert_eq!(r.risk_score, RiskScore::Medium);
    }

    #[test]
    fn not_implemented_skips_do_not_mask_a_real_unavailable_skip() {
        // Mixed: a per-file Unavailable skip must still dominate to Medium even
        // when not-implemented skips are also present.
        let r = report_with(
            vec![
                check(Vector::PageText, CheckStatus::CheckedClean),
                skip_check(Vector::Metadata, SkipKind::Unavailable),
                skip_check(Vector::Revisions, SkipKind::NotImplemented),
            ],
            vec![],
            vec![],
        );
        assert_eq!(r.risk_score, RiskScore::Medium);
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
