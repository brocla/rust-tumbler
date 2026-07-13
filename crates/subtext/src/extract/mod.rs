//! The `VectorCheck` trait and the registry.
//!
//! Every extractor self-describes (`id`, `label`, `vector`, `method`) and is
//! registered in [`REGISTRY`], so the report's `checks` list is *generated from
//! the registry*, never hand-maintained (spec §9). Adding a vector = one file +
//! one `Vector` variant + one registry line; the checks list can never silently
//! drift from the set of implemented extractors.

use crate::query::Query;
use crate::report::{Check, CheckStatus, CheckTone, Finding, Signal, SkipKind, Vector};
use pdfium_render::prelude::{PdfDocument, Pdfium};

pub mod annotations;
pub mod article_threads;
pub mod attachments;
pub mod destinations;
pub mod forms;
pub mod marked_content;
pub mod metadata;
pub mod optional_content;
pub mod orphans;
pub mod outlines;
pub mod page_labels;
pub mod page_text;
pub mod raw;
pub mod redaction;
pub mod revisions;
pub mod scripts;
pub mod signatures;
pub mod structure;
pub mod uris;
pub mod xfa;

/// What one check saw. `Ran` means the check executed (no findings ⇒ clean);
/// `Skipped` means it could not run and says why — never silently dropped
/// (spec §1 honesty rule 2). `signals` carries query-independent suspicions
/// (§3.4) alongside any findings.
pub enum CheckOutcome {
    Ran {
        findings: Vec<Finding>,
        signals: Vec<Signal>,
    },
    Skipped {
        reason: String,
        kind: SkipKind,
    },
}

impl CheckOutcome {
    /// A completed run with findings only (the common case).
    pub fn ran(findings: Vec<Finding>) -> Self {
        CheckOutcome::Ran {
            findings,
            signals: Vec::new(),
        }
    }

    /// A skip because *this file* could not be inspected (per-file blind spot).
    pub fn unavailable(reason: impl Into<String>) -> Self {
        CheckOutcome::Skipped {
            reason: reason.into(),
            kind: SkipKind::Unavailable,
        }
    }

    /// A skip because the extractor has not shipped yet (tool-phase limitation).
    pub fn not_implemented(reason: impl Into<String>) -> Self {
        CheckOutcome::Skipped {
            reason: reason.into(),
            kind: SkipKind::NotImplemented,
        }
    }

    /// A skip because the user opted out of an optional pass this run
    /// (`--ocr` / `--recurse-embedded` not passed) — §14.2.
    pub fn not_requested(reason: impl Into<String>) -> Self {
        CheckOutcome::Skipped {
            reason: reason.into(),
            kind: SkipKind::NotRequested,
        }
    }
}

/// The outcome of running a set of checks against one document view: the check
/// rows (for the report's "everything inspected" list) plus the collected
/// findings and signals. `check_pdf` runs the full `REGISTRY`; `Revisions` and
/// `--recurse-embedded` run [`non_recursive_checks`] against a sub-document.
pub struct RunResult {
    pub checks: Vec<Check>,
    pub findings: Vec<Finding>,
    pub signals: Vec<Signal>,
}

/// Runs each of `checks` against `ctx`, mapping every outcome to a [`Check`]
/// row and collecting findings + signals. The single per-check code path shared
/// by the top-level scan and every sub-scan, so a prior revision or an embedded
/// PDF is inspected exactly as the host document is.
pub fn run_checks(checks: &[&dyn VectorCheck], ctx: &DocContext, query: &Query) -> RunResult {
    let mut out_checks = Vec::with_capacity(checks.len());
    let mut findings: Vec<Finding> = Vec::new();
    let mut signals: Vec<Signal> = Vec::new();

    for check in checks {
        let (tone, status, detail, skip_kind) = match check.run(ctx, query) {
            CheckOutcome::Ran {
                findings: hits,
                signals: mut sigs,
            } => {
                let n_sigs = sigs.len();
                signals.append(&mut sigs);
                if hits.is_empty() {
                    if n_sigs > 0 {
                        (
                            CheckTone::Warning,
                            CheckStatus::CheckedClean,
                            format!("No matches, but {n_sigs} suspicious signal(s) — see signals"),
                            None,
                        )
                    } else {
                        (CheckTone::Passed, CheckStatus::CheckedClean, "No matches".to_string(), None)
                    }
                } else {
                    let detail = summarize_hits(&hits);
                    findings.extend(hits);
                    (CheckTone::Leak, CheckStatus::Found, detail, None)
                }
            }
            CheckOutcome::Skipped { reason, kind } => {
                (CheckTone::Skipped, CheckStatus::Skipped, reason, Some(kind))
            }
        };
        out_checks.push(Check {
            id: check.id(),
            label: check.label(),
            vector: check.vector(),
            method: check.method(),
            tone,
            status,
            detail,
            skip_kind,
        });
    }
    RunResult { checks: out_checks, findings, signals }
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

/// The vectors that must NOT run inside a per-revision or per-embedded sub-scan:
/// they would recurse (`Revisions`, `OrphanObjects`, `RawDecompressed` operate
/// on the whole file's raw bytes) or are too costly to repeat (`RenderedOcr`).
pub fn is_recursive_vector(v: Vector) -> bool {
    matches!(
        v,
        Vector::RenderedOcr | Vector::Revisions | Vector::OrphanObjects | Vector::RawDecompressed
    )
}

/// The registry entries safe to run against a sub-document (a prior revision or
/// an embedded PDF) — everything except the recursive/expensive vectors. Built
/// at call time so the registry list is never duplicated.
pub fn non_recursive_checks() -> Vec<&'static dyn VectorCheck> {
    REGISTRY
        .iter()
        .copied()
        .filter(|c| !is_recursive_vector(c.vector()))
        .collect()
}

/// Stamps every finding with the superseded-revision index it came from. Used
/// by `Revisions` after running the shared extractors on a prior revision —
/// matching stays inside `findings_in`, only the provenance field is set here
/// (resolves review item AL1 without letting the query modes diverge).
pub(crate) fn stamp_revision(findings: &mut [Finding], rev: u32) {
    for f in findings {
        f.revision = Some(rev);
    }
}

/// Stamps every finding with the embedded-container path it came from. Used by
/// `--recurse-embedded` (§14.10). A finding already stamped by a deeper level
/// keeps its inner path as a suffix, so a doubly-nested hit reads
/// `attachment:outer.pdf › attachment:inner.pdf`.
pub(crate) fn stamp_container(findings: &mut [Finding], path: &str) {
    for f in findings {
        f.container = Some(match f.container.take() {
            Some(inner) => format!("{path} › {inner}"),
            None => path.to_string(),
        });
    }
}

/// Matches `query` against each of `dict`'s `keys` (decoded via
/// [`crate::pdf::get_string`], which resolves indirect values), emitting a
/// finding per match under `vector`. `location(key)` builds the finding's
/// location label from the matched key name. The shared "scan a fixed set of
/// text keys on a dictionary" path used by the metadata/structure/annotation/
/// forms/attachment/thread/redaction/signature extractors, so the
/// decode-and-label convention lives in exactly one place.
#[allow(clippy::too_many_arguments)]
pub(crate) fn scan_dict_keys(
    doc: &lopdf::Document,
    dict: &lopdf::Dictionary,
    keys: &[&[u8]],
    query: &Query,
    vector: Vector,
    page: Option<u32>,
    location: impl Fn(&str) -> String,
    out: &mut Vec<Finding>,
) {
    for key in keys {
        if let Some(text) = crate::pdf::get_string(doc, dict, key) {
            let key = String::from_utf8_lossy(key);
            findings_in(&text, query, vector, &location(&key), page, out);
        }
    }
}

/// Runs `query` against one decoded string and materializes a finding per
/// match under `vector` at `location` — the single matching path every
/// string-source extractor shares, so the query modes can never diverge
/// between vectors.
pub(crate) fn findings_in(
    haystack: &str,
    query: &Query,
    vector: Vector,
    location: &str,
    page: Option<u32>,
    out: &mut Vec<Finding>,
) {
    for span in query.find_all(haystack) {
        out.push(Finding {
            vector,
            location: location.to_string(),
            matched_text: span.text.clone(),
            context: snippet(haystack, span.start, span.end),
            page,
            revision: None,
            container: None,
        });
    }
}

/// A trimmed one-line snippet of `haystack` around `[start, end)`, with up to
/// `PAD` chars of context on each side and ellipses when truncated. Operates on
/// char boundaries so multi-byte text is never split mid-codepoint.
pub(crate) fn snippet(haystack: &str, start: usize, end: usize) -> String {
    const PAD: usize = 40;
    let lo = floor_char_boundary(haystack, start.saturating_sub(PAD));
    let hi = ceil_char_boundary(haystack, (end + PAD).min(haystack.len()));
    let mut out = String::new();
    if lo > 0 {
        out.push('…');
    }
    out.push_str(haystack[lo..hi].trim());
    if hi < haystack.len() {
        out.push('…');
    }
    // Collapse any embedded newlines/tabs so the snippet stays one line.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Largest char boundary `<= i` (std's `floor_char_boundary` is still nightly).
fn floor_char_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Smallest char boundary `>= i`.
fn ceil_char_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Recursion state for `--recurse-embedded` (§14.10), threaded through
/// [`DocContext`] so an embedded PDF's own `Attachments` pass can keep
/// recursing until the depth cap. `Copy` so a sub-context is built by value.
#[derive(Clone, Copy)]
pub struct Recurse<'a> {
    /// How many embedded-PDF levels deep this context already is (0 = the
    /// top-level file). Recursion stops at [`Recurse::DEPTH_CAP`].
    pub depth: u32,
    /// Hashes of embedded-PDF bytes already scanned this run — guards cyclic
    /// or duplicated (zip-bomb-style) attachments.
    pub visited: &'a std::cell::RefCell<std::collections::HashSet<u64>>,
}

impl Recurse<'_> {
    /// Maximum embedded-PDF nesting depth inspected (§14.10).
    pub const DEPTH_CAP: u32 = 3;
}

/// Everything a check needs to inspect one document. The two parser views are
/// each `Option`: a file may parse under pdfium but not lopdf (a recovered
/// corrupt xref) or vice versa, so a check that needs a view it doesn't have
/// returns `Skipped` rather than failing the whole run.
pub struct DocContext<'a, 'p> {
    /// The raw file bytes (for the raw/orphan/revision passes, Phase 3).
    pub bytes: &'a [u8],
    /// lopdf's strict object-graph view (structural vectors).
    pub lopdf: Option<&'a lopdf::Document>,
    /// pdfium's render view (page text, OCR). The document's own borrow of the
    /// process-wide `Pdfium` binding (`'p`) is kept distinct from the borrow of
    /// the document itself (`'a`), so callers can hold the context for a shorter
    /// scope than the binding lives — `PdfDocument<'p>` is invariant in `'p`, and
    /// collapsing the two lifetimes would pin `'a` to the whole process.
    pub pdfium: Option<&'a PdfDocument<'p>>,
    /// The process-wide `Pdfium` binding itself, for checks that must load a
    /// *sub*-document (an embedded PDF under `--recurse-embedded`). `None` in
    /// contexts that cannot recurse (most unit tests).
    pub pdfium_lib: Option<&'p Pdfium>,
    /// True when the file carries `/Encrypt` — even if a view decrypted it.
    /// The raw-byte passes (`Revisions`, `OrphanObjects`) skip on this: the
    /// bytes they scan are ciphertext, so "no matches" there would be a lie.
    pub encrypted: bool,
    /// Why `lopdf` is `None`, when it is (e.g. "encrypted — supply
    /// --password"). Checks report it via [`DocContext::lopdf_unavailable`].
    pub lopdf_reason: &'static str,
    /// Why `pdfium` is `None`, when it is.
    pub pdfium_reason: &'static str,
    /// `Some` when `--recurse-embedded` is active for this run.
    pub recurse: Option<Recurse<'a>>,
}

impl<'a, 'p> DocContext<'a, 'p> {
    /// A context over the given views with every Phase-3 option off — the
    /// shape almost every unit test needs.
    pub fn new(
        bytes: &'a [u8],
        lopdf: Option<&'a lopdf::Document>,
        pdfium: Option<&'a PdfDocument<'p>>,
    ) -> Self {
        DocContext {
            bytes,
            lopdf,
            pdfium,
            pdfium_lib: None,
            encrypted: false,
            lopdf_reason: "lopdf could not parse this document",
            pdfium_reason: "pdfium could not load this document",
            recurse: None,
        }
    }

    /// The skip a structural check returns when it needs the lopdf view and
    /// this context doesn't have one. Centralized so the reason can name the
    /// actual blocker (a parse failure vs. encryption without a password).
    pub fn lopdf_unavailable(&self) -> CheckOutcome {
        CheckOutcome::unavailable(self.lopdf_reason)
    }

    /// The skip a pdfium-backed check returns when the pdfium view is absent.
    pub fn pdfium_unavailable(&self) -> CheckOutcome {
        CheckOutcome::unavailable(self.pdfium_reason)
    }
}

/// One registered extractor. Object-safe so the registry is a slice of
/// `&dyn VectorCheck`.
pub trait VectorCheck: Sync {
    /// Stable slug, e.g. `"page_text"`.
    fn id(&self) -> &'static str;
    /// Human label, e.g. `"Page text"`.
    fn label(&self) -> &'static str;
    /// The `Vector` this check reports under (one variant per check).
    fn vector(&self) -> Vector;
    /// How it looked, e.g. `"pdfium text extraction"`.
    fn method(&self) -> &'static str;
    /// Run against the document; return findings or a skip reason.
    fn run(&self, ctx: &DocContext, query: &Query) -> CheckOutcome;
}

/// A placeholder for a vector whose extractor has not landed yet. It always
/// `Skipped`s with an honest reason, so the checks list stays complete (all 21
/// vectors present) while later phases fill them in. Keeps the report honest:
/// an un-implemented vector reads as "not inspected", never as clean.
pub struct Pending {
    pub id: &'static str,
    pub label: &'static str,
    pub vector: Vector,
    pub method: &'static str,
    /// Which phase lands this extractor (for the skip message).
    pub phase: &'static str,
}

impl VectorCheck for Pending {
    fn id(&self) -> &'static str {
        self.id
    }
    fn label(&self) -> &'static str {
        self.label
    }
    fn vector(&self) -> Vector {
        self.vector
    }
    fn method(&self) -> &'static str {
        self.method
    }
    fn run(&self, _ctx: &DocContext, _query: &Query) -> CheckOutcome {
        CheckOutcome::not_implemented(format!("extractor not yet implemented ({})", self.phase))
    }
}

/// The frozen registry (spec §4.1) — one entry per `Vector` variant, in report
/// order. Phase 1 implements `PageText`; the rest are `Pending` until their
/// phase. The report's checks list is built by iterating this slice.
pub static REGISTRY: &[&dyn VectorCheck] = &[
    &page_text::PageText,
    &Pending {
        id: "rendered_ocr",
        label: "Rendered-image OCR",
        vector: Vector::RenderedOcr,
        method: "OCR engine (feature \"ocr\")",
        phase: "Phase 3, opt-in --ocr",
    },
    &metadata::Metadata,
    &structure::StructureTree,
    &marked_content::MarkedContent,
    &outlines::Outlines,
    &page_labels::PageLabels,
    &destinations::Destinations,
    &article_threads::ArticleThreads,
    &annotations::Annotations,
    &redaction::RedactionAnnotations,
    &forms::Forms,
    &xfa::Xfa,
    &attachments::Attachments,
    &scripts::Scripts,
    &uris::Uris,
    &optional_content::OptionalContent,
    &signatures::Signatures,
    &revisions::Revisions,
    &orphans::OrphanObjects,
    &raw::RawDecompressed,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_one_entry_per_vector_variant() {
        // 21 vectors in the spec §4.1 registry.
        assert_eq!(REGISTRY.len(), 21);
    }

    #[test]
    fn registry_ids_and_vectors_are_unique() {
        let mut ids: Vec<&str> = REGISTRY.iter().map(|c| c.id()).collect();
        ids.sort_unstable();
        let n = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), n, "duplicate check id in registry");

        let mut vectors: Vec<u32> = REGISTRY.iter().map(|c| c.vector() as u32).collect();
        vectors.sort_unstable();
        let n = vectors.len();
        vectors.dedup();
        assert_eq!(vectors.len(), n, "duplicate Vector in registry");
    }
}
