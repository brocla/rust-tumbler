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
pub mod pdf;
pub mod query;
pub mod report;
pub mod xml;

pub use query::{Query, QueryMode};
pub use report::{Report, RiskScore, RiskTone};

use extract::{DocContext, Recurse, RunResult, REGISTRY};
use pdfium_render::prelude::Pdfium;
use report::QueryReport;

/// Per-run options (§14.9, §14.10). `Default` is the plain scan: no password,
/// no embedded-PDF recursion.
#[derive(Debug, Clone, Default)]
pub struct CheckOptions {
    /// Password for an encrypted input, handed to both parser views at load
    /// time (never threaded through the checks). Without one, lopdf's
    /// empty-user-password auto-unlock is still tried.
    pub password: Option<String>,
    /// Recurse into embedded PDFs (`--recurse-embedded`), depth-capped.
    pub recurse_embedded: bool,
}

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
pub fn check_pdf(
    pdfium: &Pdfium,
    bytes: &[u8],
    file_name: &str,
    query: &Query,
    options: &CheckOptions,
) -> Report {
    let password = options.password.as_deref();
    // pdfium view (page text, rendering).
    let pdfium_doc = pdfium.load_pdf_from_byte_vec(bytes.to_vec(), password).ok();
    // lopdf view (structural vectors). With a password, decrypt-during-parse
    // (a post-hoc decrypt() can't read the encrypted object streams); without
    // one, `load_mem` still auto-unlocks an empty user password.
    let lopdf_loaded = match password {
        Some(pw) => {
            lopdf::Document::load_mem_with_options(bytes, lopdf::LoadOptions::with_password(pw))
        }
        None => lopdf::Document::load_mem(bytes),
    }
    .ok();

    // lopdf's loader decrypts only when it can authenticate (a supplied or
    // empty user password): success strips /Encrypt from the trailer and
    // records `encryption_state`; otherwise it still returns a parsed document
    // whose strings/streams are CIPHERTEXT. Scanning that view would report
    // honest-looking "no matches" over unreadable bytes, so discard it and let
    // the checks skip with the encryption reason instead (§14.9).
    let undecrypted = lopdf_loaded
        .as_ref()
        .is_some_and(|d| d.trailer.get(b"Encrypt").is_ok());
    let lopdf_doc = if undecrypted { None } else { lopdf_loaded };

    let encrypted = undecrypted
        || match lopdf_doc.as_ref() {
            Some(d) => d.encryption_state.is_some(),
            // Nothing parsed at all — fall back to the raw bytes.
            None => bytes.windows(8).any(|w| w == b"/Encrypt"),
        };

    let pages = pdfium_doc
        .as_ref()
        .map(|d| d.pages().len() as u32)
        .or_else(|| lopdf_doc.as_ref().map(|d| d.get_pages().len() as u32))
        .unwrap_or(0);

    // When a view is missing on an encrypted file, the honest reason is the
    // encryption, not a generic parse failure (§14.9).
    let encrypted_reason = if password.is_some() {
        "encrypted — the supplied --password did not unlock it"
    } else {
        "encrypted — supply --password"
    };

    let visited = std::cell::RefCell::new(std::collections::HashSet::new());

    // Run every registered check in an inner scope, so the borrows the context
    // holds on the two doc handles end before those handles drop (a
    // `PdfDocument` borrows the `Pdfium` binding, so drop order is load-bearing).
    let RunResult { checks, findings, signals } = {
        let mut ctx = DocContext::new(bytes, lopdf_doc.as_ref(), pdfium_doc.as_ref());
        ctx.pdfium_lib = Some(pdfium);
        ctx.encrypted = encrypted;
        if encrypted {
            if ctx.lopdf.is_none() {
                ctx.lopdf_reason = encrypted_reason;
            }
            if ctx.pdfium.is_none() {
                ctx.pdfium_reason = encrypted_reason;
            }
        }
        if options.recurse_embedded {
            ctx.recurse = Some(Recurse { depth: 0, visited: &visited });
        }
        extract::run_checks(REGISTRY, &ctx, query)
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
        signals,
    };
    report.finalize();
    report
}
