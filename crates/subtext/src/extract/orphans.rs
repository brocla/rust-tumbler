//! Vector 20 — orphaned / unreferenced objects.
//!
//! Objects physically present in the byte stream but not part of the current
//! document lopdf loaded (left by a botched edit, or belonging to a superseded
//! revision). We brute-scan the raw bytes for every `N N obj` header and scan
//! the body of any whose id is *not* in the current object map (spec §4-I,
//! §14.6). Overlap with `Revisions` (prior-revision objects are also "not
//! current") is intentional backstop redundancy; findings are deduped by object
//! id so the same object is not reported twice within this check.
//!
//! Limitation: a body that is a compressed stream is scanned as raw (lossy)
//! bytes, so a compressed orphan stream may not reveal its text — the
//! `RawDecompressed` pass inflates *reachable* streams; inflating orphaned
//! streams is a follow-up.

use crate::extract::{findings_in, CheckOutcome, DocContext, VectorCheck};
use crate::pdf;
use crate::query::Query;
use crate::report::{Finding, Vector};
use regex::bytes::Regex;
use std::collections::HashSet;

pub struct OrphanObjects;

impl VectorCheck for OrphanObjects {
    fn id(&self) -> &'static str {
        "orphan_objects"
    }
    fn label(&self) -> &'static str {
        "Orphaned objects"
    }
    fn vector(&self) -> Vector {
        Vector::OrphanObjects
    }
    fn method(&self) -> &'static str {
        "N N obj brute-scan vs current object map"
    }

    fn run(&self, ctx: &DocContext, query: &Query) -> CheckOutcome {
        // An orphan's strings/streams in an encrypted file are ciphertext in
        // the raw bytes; scanning them would false-clean (§14.9).
        if ctx.encrypted {
            return CheckOutcome::unavailable(
                "file is encrypted — the raw-byte orphan scan sees only ciphertext",
            );
        }
        let Some(doc) = ctx.lopdf else {
            return ctx.lopdf_unavailable();
        };
        let bytes = ctx.bytes;
        let re = Regex::new(r"(?m)(\d+)[ \t\r\n]+(\d+)[ \t\r\n]+obj").expect("valid regex");
        let mut findings: Vec<Finding> = Vec::new();
        let mut seen: HashSet<(u32, u16)> = HashSet::new();

        for cap in re.captures_iter(bytes) {
            let whole = cap.get(0).expect("group 0");
            let (Some(num), Some(gen)) = (parse_u32(&cap[1]), parse_u16(&cap[2])) else {
                continue;
            };
            let oid = (num, gen);
            // Present in the current (newest-wins reachable) document → not an
            // orphan.
            if doc.objects.contains_key(&oid) {
                continue;
            }
            if !seen.insert(oid) {
                continue; // already scanned this id (an earlier occurrence)
            }
            // Body runs from just past "obj" to the next "endobj".
            let start = whole.end();
            let end = find_sub(&bytes[start..], b"endobj")
                .map(|p| start + p)
                .unwrap_or(bytes.len());
            let text = pdf::decode_stream_text(&bytes[start..end]);
            findings_in(
                &text,
                query,
                Vector::OrphanObjects,
                &format!("orphan object {num} {gen}"),
                None,
                &mut findings,
            );
        }
        CheckOutcome::ran(findings)
    }
}

fn parse_u32(b: &[u8]) -> Option<u32> {
    std::str::from_utf8(b).ok()?.parse().ok()
}
fn parse_u16(b: &[u8]) -> Option<u16> {
    std::str::from_utf8(b).ok()?.parse().ok()
}

/// First index of `needle` in `haystack`.
fn find_sub(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{dictionary, Document, Object};

    #[test]
    fn finds_secret_in_an_unreferenced_object() {
        // A minimal doc whose current object map has one object, plus raw bytes
        // carrying a second `N N obj` the map doesn't know about.
        let mut doc = Document::with_version("1.5");
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog" });
        doc.trailer.set("Root", catalog);
        // Object 99 0 is not in doc.objects — it's an orphan in the raw bytes.
        let raw = b"%PDF-1.5\n99 0 obj\n(Zanzibar orphan)\nendobj\n%%EOF";

        let ctx = DocContext::new(raw, Some(&doc), None);
        let q = Query::literal(["Zanzibar".to_string()], false, false).unwrap();
        let f = match OrphanObjects.run(&ctx, &q) {
            CheckOutcome::Ran { findings, .. } => findings,
            CheckOutcome::Skipped { reason: r, .. } => panic!("skip: {r}"),
        };
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].location, "orphan object 99 0");
    }

    #[test]
    fn ignores_objects_present_in_current_document() {
        // The object IS in the map → not an orphan, even though its header is in
        // the raw bytes.
        let mut doc = Document::with_version("1.5");
        let id = doc.add_object(Object::string_literal("Zanzibar current"));
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog" });
        doc.trailer.set("Root", catalog);
        let raw = format!("{} {} obj (Zanzibar current) endobj", id.0, id.1);

        let ctx = DocContext::new(raw.as_bytes(), Some(&doc), None);
        let q = Query::literal(["Zanzibar".to_string()], false, false).unwrap();
        let f = match OrphanObjects.run(&ctx, &q) {
            CheckOutcome::Ran { findings, .. } => findings,
            CheckOutcome::Skipped { reason: r, .. } => panic!("skip: {r}"),
        };
        assert!(f.is_empty(), "current object must not be flagged orphan: {f:?}");
    }
}
