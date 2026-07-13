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
//! finding is stamped with the revision index it came from. In a linearized file
//! the first `%%EOF` ends the up-front first-page section, which is *not* a prior
//! revision, so that first boundary alone is dropped — any later `%%EOF`
//! boundaries are genuinely appended revisions and are still scanned.

use crate::extract::{
    findings_in, non_recursive_checks, run_checks, stamp_revision, sub_document_signal,
    CheckOutcome, DocContext, VectorCheck,
};
use crate::pdf;
use crate::query::Query;
use crate::report::{Finding, Signal, Vector};
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

        let mut boundaries = prior_revision_boundaries(ctx.bytes);
        // A linearized file writes a first-page section up front, terminated by
        // its own %%EOF — that boundary is NOT a prior revision. Drop only that
        // first boundary; any *later* %%EOF boundaries are genuinely appended
        // revisions and must still be scanned (§14.5). Do NOT bail on the whole
        // vector, and detect linearization from the file's first bytes, not from
        // a /Linearized key anywhere in the object map (which a crafted file
        // could plant to disable this check).
        if !boundaries.is_empty() && is_linearized(ctx.bytes) {
            boundaries.remove(0);
        }

        let mut findings: Vec<Finding> = Vec::new();
        let mut signals: Vec<Signal> = Vec::new();
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
            let label = format!("revision {rev}");

            // Structural vectors on the prior revision (Info, XMP, annots, …).
            // Propagate the recursion state and pdfium binding so an embedded
            // PDF that exists only in this prior revision is still recursed when
            // --recurse-embedded was passed (§14.10).
            let mut sub_ctx = DocContext::new(prefix, Some(&prior), None);
            sub_ctx.recurse = ctx.recurse;
            sub_ctx.pdfium_lib = ctx.pdfium_lib;
            let mut sub = run_checks(&sub_checks, &sub_ctx, query);
            stamp_revision(&mut sub.findings, rev);
            findings.append(&mut sub.findings);

            // Carry the sub-scan's signals and blind-spot disclosures up, tagged
            // with the revision, so a suspicion inside a prior revision isn't
            // dropped (the honesty contract, §14.9).
            for mut sig in sub.signals {
                sig.location = format!("{label} · {}", sig.location);
                signals.push(sig);
            }
            if let Some(sig) = sub_document_signal(&label, &sub.checks) {
                signals.push(sig);
            }

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

        CheckOutcome::Ran { findings, signals }
    }
}

/// True if `bytes` is a linearized PDF. Per ISO 32000 Annex F the linearization
/// parameter dictionary is the file's first indirect object, entirely within
/// the first 1024 bytes, so scan only that prefix for the `/Linearized` marker
/// — mirroring `FPDFAvail_IsLinearized` and Tumbler's own `buffer_is_linearized`
/// (linearize.rs). This deliberately does NOT trust a `/Linearized` key found
/// deeper in the object graph, which a crafted file could plant.
fn is_linearized(bytes: &[u8]) -> bool {
    const SCAN_LEN: usize = 1024;
    const MARKER: &[u8] = b"/Linearized";
    let scan = &bytes[..bytes.len().min(SCAN_LEN)];
    scan.windows(MARKER.len()).any(|w| w == MARKER)
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
    use crate::extract::Recurse;
    use lopdf::{dictionary, Object};

    fn zanzibar() -> Query {
        Query::literal(["Zanzibar".to_string()], false, false).unwrap()
    }

    fn run_findings(ctx: &DocContext) -> Vec<Finding> {
        match Revisions.run(ctx, &zanzibar()) {
            CheckOutcome::Ran { findings, .. } => findings,
            CheckOutcome::Skipped { reason, .. } => panic!("skip: {reason}"),
        }
    }

    #[test]
    fn is_linearized_only_trusts_the_first_1024_bytes() {
        assert!(is_linearized(b"%PDF-1.5\n1 0 obj\n<< /Linearized 1 >>\nendobj\n"));
        let mut buried = vec![b' '; 1100];
        buried.extend_from_slice(b"/Linearized 1");
        assert!(!is_linearized(&buried), "a marker past 1024 bytes must be ignored");
    }

    #[test]
    fn linearized_marker_past_1024_bytes_does_not_disable_the_vector() {
        // A `/Linearized` token planted deep in the file (past the first 1 KB)
        // must NOT be treated as a linearization marker — otherwise it is a
        // kill switch that silences the whole prior-revision scan (review #1).
        let mut rev1 = Document::with_version("1.5");
        let info = rev1.add_object(dictionary! { "Title" => Object::string_literal("Zanzibar") });
        // Push the total length past 1024 bytes so the appended marker lands
        // outside the linearization-detection window.
        rev1.add_object(Object::string_literal("x".repeat(1300)));
        let catalog = rev1.add_object(dictionary! { "Type" => "Catalog" });
        rev1.trailer.set("Root", catalog);
        rev1.trailer.set("Info", info);
        let mut bytes = Vec::new();
        rev1.save_to(&mut bytes).expect("serialize rev1");
        assert!(bytes.len() > 1024, "rev1 must exceed the detection window");
        bytes.extend_from_slice(b"\n5 0 obj\n<< /Linearized 1 >>\nendobj\n%%EOF\n");

        let ctx = DocContext::new(&bytes, None, None);
        let f = run_findings(&ctx);
        assert!(
            f.iter().any(|x| x.matched_text == "Zanzibar" && x.revision == Some(1)),
            "buried /Linearized must not disable the prior-revision scan: {f:?}"
        );
    }

    #[test]
    fn linearized_first_page_boundary_is_not_a_phantom_revision() {
        // A genuinely linearized file (marker in the first KB) with its two
        // %%EOF markers has NO prior revision — the first %%EOF ends the
        // up-front first-page section, not an earlier revision. Dropping only
        // that first boundary leaves nothing to scan, so no phantom finding.
        let mut bytes = b"%PDF-1.5\n<< /Linearized 1 /L 100 /O 3 /E 50 /N 1 /T 90 >>\n".to_vec();
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        bytes.extend_from_slice(b"first-page section with (Zanzibar) in it\n%%EOF\n");
        bytes.extend_from_slice(b"2 0 obj\n<< >>\nendobj\nmain body\n%%EOF\n");
        let ctx = DocContext::new(&bytes, None, None);
        // The dropped first-page prefix is never reparsed, so its stray
        // "(Zanzibar)" text must not surface as a revision-1 leak.
        assert!(run_findings(&ctx).is_empty());
    }

    #[test]
    fn recursion_propagates_into_a_prior_revision_embedded_pdf() {
        // A secret hidden inside an embedded PDF that exists only in a prior
        // revision must be reachable when --recurse-embedded is passed — the
        // recursion state has to flow into the per-revision sub-context (#5).
        let inner = crate::extract::attachments::tests::pdf_bytes_with_xmp_secret("Zanzibar");
        let mut host = crate::extract::attachments::tests::host_with_attachment("inner.pdf", &inner, false);
        let mut bytes = Vec::new();
        host.save_to(&mut bytes).expect("serialize rev1 host");
        bytes.extend_from_slice(b"\n9 0 obj\n<< >>\nendobj\n%%EOF\n");

        let visited = std::cell::RefCell::new(std::collections::HashSet::new());
        let mut ctx = DocContext::new(&bytes, None, None);
        ctx.recurse = Some(Recurse { depth: 0, visited: &visited });
        let f = run_findings(&ctx);
        // The secret must surface from a prior revision (revision stamped) via
        // the embedded-PDF recursion (container stamped). The exact revision
        // index isn't pinned — the embedded stream carries its own %%EOF, which
        // adds a boundary, so the host revision may not be index 1.
        assert!(
            f.iter().any(|x| {
                x.revision.is_some()
                    && x.container.as_deref().is_some_and(|c| c.contains("attachment:inner.pdf"))
                    && x.matched_text == "Zanzibar"
            }),
            "prior-revision embedded PDF must be recursed: {f:?}"
        );
    }

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
