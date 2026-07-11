//! The `VectorCheck` trait and the registry.
//!
//! Every extractor self-describes (`id`, `label`, `vector`, `method`) and is
//! registered in [`REGISTRY`], so the report's `checks` list is *generated from
//! the registry*, never hand-maintained (spec §9). Adding a vector = one file +
//! one `Vector` variant + one registry line; the checks list can never silently
//! drift from the set of implemented extractors.

use crate::query::Query;
use crate::report::{Finding, Signal, SkipKind, Vector};
use pdfium_render::prelude::PdfDocument;

pub mod annotations;
pub mod article_threads;
pub mod attachments;
pub mod destinations;
pub mod forms;
pub mod marked_content;
pub mod metadata;
pub mod optional_content;
pub mod outlines;
pub mod page_labels;
pub mod page_text;
pub mod redaction;
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
    &Pending {
        id: "revisions",
        label: "Superseded revisions",
        vector: Vector::Revisions,
        method: "per-revision reparse",
        phase: "Phase 3",
    },
    &Pending {
        id: "orphan_objects",
        label: "Orphaned objects",
        vector: Vector::OrphanObjects,
        method: "N N obj + ObjStm brute-scan",
        phase: "Phase 3",
    },
    &Pending {
        id: "raw_decompressed",
        label: "Raw decompressed scan",
        vector: Vector::RawDecompressed,
        method: "inflate-all + scan",
        phase: "Phase 3",
    },
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
