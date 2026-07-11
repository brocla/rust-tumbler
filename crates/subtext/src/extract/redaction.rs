//! Vector 11 — redaction annotations (`/Subtype /Redact`). A surviving
//! `/Redact` annotation is the strongest "redaction was marked but never
//! applied" tell: the content under it is very likely fully present. Its
//! `/OverlayText` and `/RC` can also echo the redacted text (spec §4-K,
//! +audit). Emits the query-independent `UnappliedRedactAnnotation` signal
//! (§3.4) for every such annotation, whether or not the query matches.

use crate::extract::{scan_dict_keys, CheckOutcome, DocContext, VectorCheck};
use crate::pdf;
use crate::query::Query;
use crate::report::{Finding, Signal, SignalKind, Vector};

pub struct RedactionAnnotations;

const TEXT_KEYS: &[&[u8]] = &[b"OverlayText", b"RC"];

impl VectorCheck for RedactionAnnotations {
    fn id(&self) -> &'static str {
        "redaction_annotations"
    }
    fn label(&self) -> &'static str {
        "Redaction annotations"
    }
    fn vector(&self) -> Vector {
        Vector::RedactionAnnotations
    }
    fn method(&self) -> &'static str {
        "/Redact annotation scan"
    }

    fn run(&self, ctx: &DocContext, query: &Query) -> CheckOutcome {
        let Some(doc) = ctx.lopdf else {
            return CheckOutcome::unavailable("lopdf could not parse this document");
        };
        let mut findings: Vec<Finding> = Vec::new();
        let mut signals: Vec<Signal> = Vec::new();

        for (page_id, page_num) in pdf::page_numbers(doc) {
            let Ok(page) = doc.get_dictionary(page_id) else { continue };
            let Some(annots) = pdf::get_array(doc, page, b"Annots") else { continue };
            for annot in annots {
                let Some(annot) = pdf::resolve(doc, annot).and_then(|o| o.as_dict().ok()) else {
                    continue;
                };
                let is_redact = annot
                    .get(b"Subtype")
                    .ok()
                    .and_then(|o| o.as_name().ok())
                    .map(|n| n == b"Redact")
                    .unwrap_or(false);
                if !is_redact {
                    continue;
                }
                signals.push(Signal {
                    kind: SignalKind::UnappliedRedactAnnotation,
                    location: format!("page {page_num}"),
                    detail: "A /Redact annotation is present but its content was never applied — the text beneath it is likely still recoverable.".to_string(),
                });
                scan_dict_keys(doc, annot, TEXT_KEYS, query, Vector::RedactionAnnotations, Some(page_num), |k| format!("/Redact /{k} (page {page_num})"), &mut findings);
            }
        }
        CheckOutcome::Ran { findings, signals }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::CheckOutcome;
    use lopdf::{dictionary, Document, Object};

    fn doc_with_redact() -> Document {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let annot = doc.add_object(dictionary! {
            "Type" => "Annot", "Subtype" => "Redact",
            "OverlayText" => Object::string_literal("Zanzibar overlay"),
        });
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id,
            "MediaBox" => vec![Object::Integer(0), Object::Integer(0), Object::Integer(200), Object::Integer(200)],
            "Annots" => vec![Object::Reference(annot)],
        });
        doc.objects.insert(pages_id, Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => Object::Integer(1),
        }));
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);
        doc
    }

    #[test]
    fn fires_signal_and_finds_overlay_text() {
        let doc = doc_with_redact();
        let ctx = DocContext { bytes: &[], lopdf: Some(&doc), pdfium: None };
        let q = Query::literal(["Zanzibar".to_string()], false, false).unwrap();
        match RedactionAnnotations.run(&ctx, &q) {
            CheckOutcome::Ran { findings, signals } => {
                assert_eq!(signals.len(), 1);
                assert!(matches!(signals[0].kind, SignalKind::UnappliedRedactAnnotation));
                assert!(findings.iter().any(|f| f.location.contains("/OverlayText")));
            }
            CheckOutcome::Skipped { reason: r, .. } => panic!("skip: {r}"),
        }
    }

    #[test]
    fn signal_fires_even_when_query_absent() {
        let doc = doc_with_redact();
        let ctx = DocContext { bytes: &[], lopdf: Some(&doc), pdfium: None };
        let q = Query::literal(["Nonexistent".to_string()], false, false).unwrap();
        match RedactionAnnotations.run(&ctx, &q) {
            CheckOutcome::Ran { findings, signals } => {
                assert!(findings.is_empty());
                assert_eq!(signals.len(), 1, "signal is query-independent");
            }
            CheckOutcome::Skipped { reason: r, .. } => panic!("skip: {r}"),
        }
    }
}
