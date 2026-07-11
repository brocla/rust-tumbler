//! Vector 3 — document metadata: the Info dictionary and every `/Metadata`
//! XMP stream in the object graph (catalog, page, or any XObject/stream — spec
//! §4-B, §4.1). Inverts Tumbler's Info + `/Metadata` scrub (`redact.rs`).

use crate::extract::{findings_in, CheckOutcome, DocContext, VectorCheck};
use crate::pdf;
use crate::query::Query;
use crate::report::Vector;
use crate::xml;
use lopdf::Object;

pub struct Metadata;

impl VectorCheck for Metadata {
    fn id(&self) -> &'static str {
        "metadata"
    }
    fn label(&self) -> &'static str {
        "Document metadata"
    }
    fn vector(&self) -> Vector {
        Vector::Metadata
    }
    fn method(&self) -> &'static str {
        "Info dict + all /Metadata XMP"
    }

    fn run(&self, ctx: &DocContext, query: &Query) -> CheckOutcome {
        let Some(doc) = ctx.lopdf else {
            return CheckOutcome::Skipped(
                "lopdf could not parse this document (needed for metadata)".to_string(),
            );
        };
        let mut findings = Vec::new();

        // The Info dictionary — every string value, including custom keys.
        if let Some(info) = doc
            .trailer
            .get(b"Info")
            .ok()
            .and_then(|o| pdf::resolve(doc, o))
            .and_then(|o| o.as_dict().ok())
        {
            for (key, value) in info.iter() {
                if let Some(text) = pdf::string_text(value) {
                    let key = String::from_utf8_lossy(key);
                    findings_in(
                        &text,
                        query,
                        Vector::Metadata,
                        &format!("Info /{key}"),
                        None,
                        &mut findings,
                    );
                }
            }
        }

        // Every /Metadata XMP stream anywhere in the object graph.
        for (id, obj) in &doc.objects {
            let Object::Stream(stream) = obj else { continue };
            if !is_metadata_stream(&stream.dict) {
                continue;
            }
            if let Some(bytes) = pdf::stream_bytes(doc, *id) {
                let text = xml::visible_text(&bytes);
                findings_in(
                    &text,
                    query,
                    Vector::Metadata,
                    &format!("XMP metadata (object {} {})", id.0, id.1),
                    None,
                    &mut findings,
                );
            }
        }

        CheckOutcome::ran(findings)
    }
}

/// True for a stream that is an XMP metadata packet (`/Type /Metadata`, usually
/// `/Subtype /XML`).
fn is_metadata_stream(dict: &lopdf::Dictionary) -> bool {
    dict.get(b"Type")
        .ok()
        .and_then(|o| o.as_name().ok())
        .map(|n| n == b"Metadata")
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::CheckOutcome;
    use crate::report::Finding;
    use lopdf::{dictionary, Document, Stream};

    fn run(doc: &Document, term: &str) -> Vec<Finding> {
        let ctx = DocContext { bytes: &[], lopdf: Some(doc), pdfium: None };
        let q = Query::literal([term.to_string()], false, false).unwrap();
        match Metadata.run(&ctx, &q) {
            CheckOutcome::Ran { findings, .. } => findings,
            CheckOutcome::Skipped(r) => panic!("unexpected skip: {r}"),
        }
    }

    #[test]
    fn finds_secret_in_info_title() {
        let mut doc = Document::with_version("1.5");
        let info = doc.add_object(dictionary! {
            "Title" => Object::string_literal("Zanzibar report"),
            "Author" => Object::string_literal("nobody"),
        });
        doc.trailer.set("Info", info);
        let f = run(&doc, "Zanzibar");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].location, "Info /Title");
    }

    #[test]
    fn finds_secret_in_xmp_stream() {
        let mut doc = Document::with_version("1.5");
        doc.add_object(Stream::new(
            dictionary! { "Type" => "Metadata", "Subtype" => "XML" },
            b"<x><dc:title>Zanzibar xmp</dc:title></x>".to_vec(),
        ));
        let f = run(&doc, "Zanzibar");
        assert_eq!(f.len(), 1);
        assert!(f[0].location.starts_with("XMP metadata"));
    }

    #[test]
    fn no_false_positive_when_absent() {
        let mut doc = Document::with_version("1.5");
        let info = doc.add_object(dictionary! { "Title" => Object::string_literal("nothing here") });
        doc.trailer.set("Info", info);
        assert!(run(&doc, "Zanzibar").is_empty());
    }
}
