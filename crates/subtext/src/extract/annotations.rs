//! Vector 10 — annotations (`/Annots`). An annotation's `/Contents`, `/T`,
//! `/Subj`, and `/RC` (rich text) are viewer-drawn text outside the page
//! content, and its `/AP` appearance stream can paint the secret too (spec
//! §4-D). Inverts Tumbler's page-`/Annots` scrub.

use crate::extract::{findings_in, CheckOutcome, DocContext, VectorCheck};
use crate::pdf;
use crate::query::Query;
use crate::report::{Finding, Vector};
use lopdf::{Dictionary, Document, Object};

pub struct Annotations;

/// Annotation dictionary keys that hold plain text.
const TEXT_KEYS: &[&[u8]] = &[b"Contents", b"T", b"Subj", b"RC"];

impl VectorCheck for Annotations {
    fn id(&self) -> &'static str {
        "annotations"
    }
    fn label(&self) -> &'static str {
        "Annotations"
    }
    fn vector(&self) -> Vector {
        Vector::Annotations
    }
    fn method(&self) -> &'static str {
        "/Annots text + /AP appearance streams"
    }

    fn run(&self, ctx: &DocContext, query: &Query) -> CheckOutcome {
        let Some(doc) = ctx.lopdf else {
            return CheckOutcome::Skipped("lopdf could not parse this document".to_string());
        };
        let mut findings = Vec::new();
        let page_nums = pdf::page_numbers(doc);
        for (page_id, page_num) in page_nums.iter().map(|(id, n)| (*id, *n)) {
            let Ok(page) = doc.get_dictionary(page_id) else { continue };
            let Some(annots) = pdf::get_array(doc, page, b"Annots") else { continue };
            for annot in annots {
                let Some(annot) = pdf::resolve(doc, annot).and_then(|o| o.as_dict().ok()) else {
                    continue;
                };
                scan_annotation(doc, annot, query, page_num, &mut findings);
            }
        }
        CheckOutcome::ran(findings)
    }
}

fn scan_annotation(
    doc: &Document,
    annot: &Dictionary,
    query: &Query,
    page_num: u32,
    findings: &mut Vec<Finding>,
) {
    for key in TEXT_KEYS {
        if let Some(text) = pdf::get_string(doc, annot, key) {
            let key = String::from_utf8_lossy(key);
            findings_in(&text, query, Vector::Annotations, &format!("annotation /{key} (page {page_num})"), Some(page_num), findings);
        }
    }
    // /AP appearance stream(s): /AP /N may be a stream, or a sub-dictionary of
    // appearance states each pointing to a stream. Decode and scan the text.
    if let Some(ap) = pdf::get_dict(doc, annot, b"AP") {
        for state in [b"N".as_slice(), b"D", b"R"] {
            match ap.get(state).ok().and_then(|o| pdf::resolve(doc, o)) {
                Some(obj @ Object::Stream(_)) => scan_appearance(obj, query, page_num, findings),
                Some(Object::Dictionary(states)) => {
                    for (_name, v) in states.iter() {
                        if let Some(obj) = pdf::resolve(doc, v) {
                            scan_appearance(obj, query, page_num, findings);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// Decodes one resolved appearance-stream XObject and scans its content-stream
/// string operands for the query.
fn scan_appearance(obj: &Object, query: &Query, page_num: u32, findings: &mut Vec<Finding>) {
    let Some(bytes) = pdf::stream_object_bytes(obj) else { return };
    for s in pdf::scan_content_strings(&bytes) {
        findings_in(&s.value, query, Vector::Annotations, &format!("annotation /AP appearance (page {page_num})"), Some(page_num), findings);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::CheckOutcome;
    use lopdf::{dictionary, Stream};

    fn one_page_doc_with_annot(annot: Dictionary) -> Document {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let annot_id = doc.add_object(annot);
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![Object::Integer(0), Object::Integer(0), Object::Integer(200), Object::Integer(200)],
            "Annots" => vec![Object::Reference(annot_id)],
        });
        doc.objects.insert(pages_id, Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => Object::Integer(1),
        }));
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);
        doc
    }

    fn run(doc: &Document, term: &str) -> Vec<Finding> {
        let ctx = DocContext { bytes: &[], lopdf: Some(doc), pdfium: None };
        let q = Query::literal([term.to_string()], false, false).unwrap();
        match Annotations.run(&ctx, &q) {
            CheckOutcome::Ran { findings, .. } => findings,
            CheckOutcome::Skipped(r) => panic!("skip: {r}"),
        }
    }

    #[test]
    fn finds_secret_in_contents() {
        let doc = one_page_doc_with_annot(dictionary! {
            "Type" => "Annot", "Subtype" => "Text",
            "Contents" => Object::string_literal("Zanzibar comment"),
        });
        let f = run(&doc, "Zanzibar");
        assert!(f.iter().any(|x| x.location.contains("/Contents")), "{f:?}");
    }

    #[test]
    fn finds_secret_in_appearance_stream() {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let ap = doc.add_object(Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Form" },
            b"BT /F1 18 Tf (Zanzibar) Tj ET".to_vec()));
        let annot_id = doc.add_object(dictionary! {
            "Type" => "Annot", "Subtype" => "FreeText",
            "AP" => dictionary! { "N" => Object::Reference(ap) },
        });
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id,
            "MediaBox" => vec![Object::Integer(0), Object::Integer(0), Object::Integer(200), Object::Integer(200)],
            "Annots" => vec![Object::Reference(annot_id)],
        });
        doc.objects.insert(pages_id, Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => Object::Integer(1),
        }));
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);

        let f = run(&doc, "Zanzibar");
        assert!(f.iter().any(|x| x.location.contains("/AP appearance")), "{f:?}");
    }
}
