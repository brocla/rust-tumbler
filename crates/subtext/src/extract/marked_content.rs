//! Vector 5 — marked-content `/ActualText` (and `/Alt`) that sits *inline in a
//! content stream* (`/Span <</ActualText (secret)>> BDC … EMC`), outside the
//! structure tree, so a StructTreeRoot walk misses it (spec §4-K, +audit).
//! pdfium's page-text extraction honours inline `/ActualText` only sometimes,
//! so this scans it directly.

use crate::extract::{findings_in, CheckOutcome, DocContext, VectorCheck};
use crate::pdf;
use crate::query::Query;
use crate::report::{Finding, Vector};
use lopdf::Object;

pub struct MarkedContent;

impl VectorCheck for MarkedContent {
    fn id(&self) -> &'static str {
        "marked_content"
    }
    fn label(&self) -> &'static str {
        "Marked content"
    }
    fn vector(&self) -> Vector {
        Vector::MarkedContent
    }
    fn method(&self) -> &'static str {
        "content-stream /ActualText, /Alt"
    }

    fn run(&self, ctx: &DocContext, query: &Query) -> CheckOutcome {
        let Some(doc) = ctx.lopdf else {
            return ctx.lopdf_unavailable();
        };
        let mut findings: Vec<Finding> = Vec::new();

        // Page content streams (labelled with their page number).
        let page_nums = pdf::page_numbers(doc);
        for (page_id, page_num) in page_nums.iter().map(|(id, n)| (*id, *n)) {
            for bytes in pdf::page_content_streams(doc, page_id) {
                scan(&bytes, query, Some(page_num), &format!("page {page_num} content"), &mut findings);
            }
        }
        // Every Form XObject stream anywhere (nested content pdfium doesn't
        // attribute to a page).
        for (id, obj) in &doc.objects {
            let Object::Stream(stream) = obj else { continue };
            if pdf::name_is(&stream.dict, b"Subtype", &[b"Form"]) {
                if let Some(bytes) = pdf::stream_bytes(doc, *id) {
                    scan(&bytes, query, None, &format!("Form XObject {} {}", id.0, id.1), &mut findings);
                }
            }
        }
        CheckOutcome::ran(findings)
    }
}

/// Scans one content-stream blob for `/ActualText`- or `/Alt`-tagged strings,
/// labelling each finding with the key it actually came from.
fn scan(bytes: &[u8], query: &Query, page: Option<u32>, where_: &str, findings: &mut Vec<Finding>) {
    for s in pdf::scan_content_strings(bytes) {
        let key: Option<&str> = match s.preceding_name.as_deref() {
            Some(b"ActualText") => Some("ActualText"),
            Some(b"Alt") => Some("Alt"),
            _ => None,
        };
        if let Some(key) = key {
            findings_in(&s.value, query, Vector::MarkedContent, &format!("/{key} in {where_}"), page, findings);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::CheckOutcome;
    use lopdf::{dictionary, Document, Stream};

    #[test]
    fn finds_secret_in_inline_actualtext() {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let content = doc.add_object(Stream::new(
            dictionary! {},
            b"/Span <</ActualText (Zanzibar hidden)>> BDC (visible) Tj EMC".to_vec(),
        ));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => Object::Reference(content),
            "MediaBox" => vec![Object::Integer(0), Object::Integer(0), Object::Integer(200), Object::Integer(200)],
        });
        doc.objects.insert(pages_id, Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => Object::Integer(1),
        }));
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);

        let ctx = DocContext::new(&[], Some(&doc), None);
        let q = Query::literal(["Zanzibar".to_string()], false, false).unwrap();
        let f = match MarkedContent.run(&ctx, &q) {
            CheckOutcome::Ran { findings, .. } => findings,
            CheckOutcome::Skipped { reason: r, .. } => panic!("skip: {r}"),
        };
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(f[0].location.contains("/ActualText"));
    }
}
