//! Vector 19 — superseded incremental-update revisions.
//!
//! A PDF can carry multiple revisions, each an appended `xref`/`trailer`
//! pointing back via `/Prev`. A later revision can "cover" an earlier one (e.g.
//! replace a page's `/Contents`) while the earlier revision survives *physically*
//! in the file, recoverable by truncating at an earlier `%%EOF`. lopdf's parse
//! keeps newest-wins, so a superseded revision's objects never enter the current
//! `Document`; they must be reached deliberately (spec §4-I, §14.5).
//!
//! Approach: for each prior-revision prefix (bytes up to a non-final `%%EOF`),
//! reparse it as a standalone document and run the non-recursive extractors on
//! it, plus a lopdf page-show-text scan (pdfium is not loaded per-revision). Each
//! finding is stamped with the revision index it came from. A `/Linearized` file
//! is a single revision whose first-page section is *not* a prior revision, so it
//! is skipped (the linearization guard).

use crate::extract::{
    findings_in, non_recursive_checks, run_checks, stamp_revision, CheckOutcome, DocContext,
    VectorCheck,
};
use crate::pdf;
use crate::query::Query;
use crate::report::{Finding, Vector};
use lopdf::Document;

pub struct Revisions;

impl VectorCheck for Revisions {
    fn id(&self) -> &'static str {
        "revisions"
    }
    fn label(&self) -> &'static str {
        "Superseded revisions"
    }
    fn vector(&self) -> Vector {
        Vector::Revisions
    }
    fn method(&self) -> &'static str {
        "per-revision reparse (truncate at %%EOF)"
    }

    fn run(&self, ctx: &DocContext, query: &Query) -> CheckOutcome {
        // An encrypted file's prior revisions are ciphertext in the raw bytes
        // (string/stream decryption keys are per-object); reporting "no
        // matches" over bytes we cannot read would be a lie (§14.9).
        if ctx.encrypted {
            return CheckOutcome::unavailable(
                "file is encrypted — the raw-byte revision scan sees only ciphertext",
            );
        }

        // A linearized file's early %%EOF is not a prior revision — skip it so we
        // don't manufacture a phantom "prior revision" from the first-page xref.
        if ctx.lopdf.map(is_linearized).unwrap_or(false) {
            return CheckOutcome::ran(Vec::new());
        }

        let boundaries = prior_revision_boundaries(ctx.bytes);
        let mut findings: Vec<Finding> = Vec::new();
        let sub_checks = non_recursive_checks();

        for (rev_index, &end) in boundaries.iter().enumerate() {
            let prefix = &ctx.bytes[..end];
            let Ok(prior) = Document::load_mem(prefix) else {
                // A prefix that doesn't parse standalone is not a recoverable
                // revision; skip it (the current revision is the authoritative
                // one). Not a per-file blind spot for the query.
                continue;
            };
            let rev = (rev_index + 1) as u32;

            // Structural vectors on the prior revision (Info, XMP, annots, …).
            let sub_ctx = DocContext::new(prefix, Some(&prior), None);
            let mut sub = run_checks(&sub_checks, &sub_ctx, query);
            stamp_revision(&mut sub.findings, rev);
            findings.append(&mut sub.findings);

            // Prior-revision page text (pdfium isn't loaded per-revision, so use
            // the lopdf show-text approximation).
            let mut page_hits = Vec::new();
            for (page_num, text) in pdf::page_show_text(&prior) {
                findings_in(
                    &text,
                    query,
                    Vector::Revisions,
                    &format!("page {page_num} (revision {rev})"),
                    Some(page_num),
                    &mut page_hits,
                );
            }
            stamp_revision(&mut page_hits, rev);
            findings.append(&mut page_hits);
        }

        CheckOutcome::ran(findings)
    }
}

/// True if the document is linearized (its first object carries `/Linearized`).
fn is_linearized(doc: &Document) -> bool {
    doc.objects.values().any(|o| match o {
        lopdf::Object::Dictionary(d) => d.get(b"Linearized").is_ok(),
        lopdf::Object::Stream(s) => s.dict.get(b"Linearized").is_ok(),
        _ => false,
    })
}

/// Byte offsets just past each `%%EOF` that starts a *prior* revision — i.e.
/// every `%%EOF` except the final one (which ends the current revision). Each
/// offset is the length of a standalone earlier-revision prefix.
fn prior_revision_boundaries(bytes: &[u8]) -> Vec<usize> {
    const EOF: &[u8] = b"%%EOF";
    let mut ends = Vec::new();
    let mut i = 0;
    while i + EOF.len() <= bytes.len() {
        if &bytes[i..i + EOF.len()] == EOF {
            ends.push(i + EOF.len());
            i += EOF.len();
        } else {
            i += 1;
        }
    }
    // Drop the final %%EOF — it terminates the current (newest) revision.
    if !ends.is_empty() {
        ends.pop();
    }
    ends
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundaries_are_all_but_the_last_eof() {
        let b = b"header %%EOF middle %%EOF tail %%EOF";
        let ends = prior_revision_boundaries(b);
        assert_eq!(ends.len(), 2, "3 %%EOF → 2 prior boundaries");
        // Each prefix ends right after a %%EOF.
        assert_eq!(&b[ends[0] - 5..ends[0]], b"%%EOF");
    }

    #[test]
    fn single_eof_has_no_prior_revision() {
        assert!(prior_revision_boundaries(b"only one %%EOF").is_empty());
    }
}
