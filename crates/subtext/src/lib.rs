//! Subtext — A Redaction Checker.
//!
//! A **read-only**, tool-agnostic tool that answers one question about a PDF:
//! *"Does the word (or words, or pattern) I redacted still appear anywhere in
//! this file?"* — and, just as importantly, *"which of the many places a PDF
//! can hide text did you actually check?"*
//!
//! Design contract: `doc/redaction-checker-design.md` (status: Ratified).
//! Core principle: **completeness is the product** — the tool never certifies
//! "clean"; it reports "no matches found in the N vectors listed below" and
//! lists them, and every vector it could not inspect is reported as Skipped
//! with a reason, never silently dropped.
//!
//! The entry point is [`check_pdf`]: a pure function over the file bytes and a
//! [`Query`] that runs every registered [`extract::VectorCheck`] and returns a
//! [`Report`]. The CLI (`bin/subtext.rs`) is a thin wrapper around it.

pub mod extract;
pub mod query;
pub mod report;

pub use query::{Query, QueryMode};
pub use report::{Report, RiskScore, RiskTone};

use extract::{CheckOutcome, DocContext, REGISTRY};
use pdfium_render::prelude::Pdfium;
use report::{Check, CheckStatus, CheckTone, Finding, QueryReport};

/// Runs the full vector inventory against one PDF and returns its report.
///
/// Pure with respect to the process: it binds nothing and writes nothing. The
/// caller supplies the process-wide `Pdfium` binding (pdfium can be bound only
/// once per process; the CLI binds it once). `bytes` is the whole file;
/// `file_name` is used only for the report header.
///
/// Neither parser view is required to succeed: a file may load under pdfium but
/// not lopdf (a recovered corrupt xref) or vice versa. Each check reports
/// `Skipped` when it lacks the view it needs, so a partial parse yields a
/// partial-but-honest report rather than an error.
pub fn check_pdf(pdfium: &Pdfium, bytes: &[u8], file_name: &str, query: &Query) -> Report {
    // pdfium view (page text, rendering). Password handling arrives in Phase 3.
    let pdfium_doc = pdfium.load_pdf_from_byte_vec(bytes.to_vec(), None).ok();
    // lopdf view (structural vectors, Phase 2+).
    let lopdf_doc = lopdf::Document::load_mem(bytes).ok();

    let pages = pdfium_doc
        .as_ref()
        .map(|d| d.pages().len() as u32)
        .or_else(|| lopdf_doc.as_ref().map(|d| d.get_pages().len() as u32))
        .unwrap_or(0);

    // Run every registered check in an inner scope, so the borrows the context
    // holds on the two doc handles end before those handles drop (a
    // `PdfDocument` borrows the `Pdfium` binding, so drop order is load-bearing).
    let (checks, findings) = {
        let ctx = DocContext {
            bytes,
            lopdf: lopdf_doc.as_ref(),
            pdfium: pdfium_doc.as_ref(),
        };

        let mut checks = Vec::with_capacity(REGISTRY.len());
        let mut findings: Vec<Finding> = Vec::new();

        for check in REGISTRY {
            let (tone, status, detail) = match check.run(&ctx, query) {
                CheckOutcome::Ran(hits) if hits.is_empty() => {
                    (CheckTone::Passed, CheckStatus::CheckedClean, "No matches".to_string())
                }
                CheckOutcome::Ran(mut hits) => {
                    let detail = summarize_hits(&hits);
                    findings.append(&mut hits);
                    (CheckTone::Leak, CheckStatus::Found, detail)
                }
                CheckOutcome::Skipped(reason) => {
                    (CheckTone::Skipped, CheckStatus::Skipped, reason)
                }
            };
            checks.push(Check {
                id: check.id(),
                label: check.label(),
                vector: check.vector(),
                method: check.method(),
                tone,
                status,
                detail,
            });
        }
        (checks, findings)
    };

    let mut report = Report {
        file_name: file_name.to_string(),
        file_size: bytes.len() as u64,
        generated_at: chrono::Utc::now().to_rfc3339(),
        pages,
        query: QueryReport::from_query(query),
        // Filled by `finalize` below.
        risk_tone: RiskTone::Clean,
        risk_score: RiskScore::None,
        title: String::new(),
        description: String::new(),
        checks,
        findings,
        // Signals arrive with their producing checks (Phase 2+).
        signals: Vec::new(),
    };
    report.finalize();
    report
}

/// A one-line summary of a check's hits for its `detail` field, e.g.
/// "2 matches on pages 4, 7" or "1 match".
fn summarize_hits(hits: &[Finding]) -> String {
    let n = hits.len();
    let noun = if n == 1 { "match" } else { "matches" };
    let mut pages: Vec<u32> = hits.iter().filter_map(|h| h.page).collect();
    pages.sort_unstable();
    pages.dedup();
    match pages.as_slice() {
        [] => format!("{n} {noun}"),
        [p] => format!("{n} {noun} on page {p}"),
        many => {
            let list = many.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ");
            format!("{n} {noun} on pages {list}")
        }
    }
}
