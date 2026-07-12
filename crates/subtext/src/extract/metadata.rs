//! Vector 3 — document metadata: the Info dictionary and every `/Metadata`
//! XMP stream in the object graph (catalog, page, or any XObject/stream — spec
//! §4-B, §4.1). Inverts Tumbler's Info + `/Metadata` scrub (`redact.rs`).

use crate::extract::{findings_in, CheckOutcome, DocContext, VectorCheck};
use crate::pdf;
use crate::query::Query;
use crate::report::Vector;
use crate::xml;

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
            return CheckOutcome::unavailable("lopdf could not parse this document (needed for metadata)");
        };
        let mut findings = Vec::new();

        // The Info dictionary — every string value, including custom keys.
        // Values may themselves be indirect references, so resolve each before
        // decoding (a bare `/Title 12 0 R` would otherwise be silently skipped).
        if let Some(info) = doc
            .trailer
            .get(b"Info")
            .ok()
            .and_then(|o| pdf::resolve(doc, o))
            .and_then(|o| o.as_dict().ok())
        {
            for (key, value) in info.iter() {
                if let Some(text) = pdf::resolve(doc, value).and_then(pdf::string_text) {
                    let key = String::from_utf8_lossy(key);
                    findings_in(&text, query, Vector::Metadata, &format!("Info /{key}"), None, &mut findings);
                }
            }
        }

        // XMP `/Metadata` streams. Collect every candidate id once: (a) any
        // stream tagged `/Type /Metadata` anywhere in the graph, PLUS (b) the
        // catalog's and each page's `/Metadata` target *regardless of /Type* —
        // a hider can omit the optional `/Type` marker, so the structural
        // reference must be followed directly (spec §4-B), not just the tag.
        let mut xmp_ids: Vec<lopdf::ObjectId> = pdf::iter_dicts(doc)
            .filter(|(_, dict)| is_metadata_stream(dict))
            .map(|(id, _)| id)
            .collect();
        if let Some(catalog) = pdf::catalog(doc) {
            push_metadata_ref(catalog, &mut xmp_ids);
        }
        for page_id in doc.get_pages().into_values() {
            if let Ok(page) = doc.get_dictionary(page_id) {
                push_metadata_ref(page, &mut xmp_ids);
            }
        }
        xmp_ids.sort_unstable();
        xmp_ids.dedup();

        for id in xmp_ids {
            // Only scan objects that are actually streams (a /Metadata entry
            // could point elsewhere in a malformed file).
            if doc.get_object(id).and_then(|o| o.as_stream()).is_err() {
                continue;
            }
            if let Some(bytes) = pdf::stream_bytes(doc, id) {
                let text = xml::visible_text(&bytes);
                findings_in(&text, query, Vector::Metadata, &format!("XMP metadata (object {} {})", id.0, id.1), None, &mut findings);
            }
        }

        CheckOutcome::ran(findings)
    }
}

/// Pushes `dict`'s `/Metadata` entry's object id (when it is an indirect
/// reference) onto `ids`.
fn push_metadata_ref(dict: &lopdf::Dictionary, ids: &mut Vec<lopdf::ObjectId>) {
    if let Ok(id) = dict.get(b"Metadata").and_then(|o| o.as_reference()) {
        ids.push(id);
    }
}

/// True for a stream that is an XMP metadata packet (`/Type /Metadata`, usually
/// `/Subtype /XML`). Note this is the tag-based path only; the extractor also
/// follows catalog/page `/Metadata` references directly for streams that omit
/// the optional `/Type`.
fn is_metadata_stream(dict: &lopdf::Dictionary) -> bool {
    pdf::name_is(dict, b"Type", &[b"Metadata"])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::CheckOutcome;
    use crate::report::Finding;
    use lopdf::{dictionary, Document, Object, Stream};

    fn run(doc: &Document, term: &str) -> Vec<Finding> {
        let ctx = DocContext { bytes: &[], lopdf: Some(doc), pdfium: None };
        let q = Query::literal([term.to_string()], false, false).unwrap();
        match Metadata.run(&ctx, &q) {
            CheckOutcome::Ran { findings, .. } => findings,
            CheckOutcome::Skipped { reason: r, .. } => panic!("unexpected skip: {r}"),
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

    #[test]
    fn finds_secret_in_indirect_info_value() {
        // /Title stored as an indirect reference (legal) must still be scanned.
        let mut doc = Document::with_version("1.5");
        let title = doc.add_object(Object::string_literal("Zanzibar report"));
        let info = doc.add_object(dictionary! { "Title" => Object::Reference(title) });
        doc.trailer.set("Info", info);
        let f = run(&doc, "Zanzibar");
        assert_eq!(f.len(), 1, "indirect Info value missed: {f:?}");
        assert_eq!(f[0].location, "Info /Title");
    }

    #[test]
    fn finds_xmp_referenced_from_catalog_without_type() {
        // An XMP stream that omits the optional /Type /Metadata but is reached
        // via catalog /Metadata must still be scanned (a hider can drop /Type).
        let mut doc = Document::with_version("1.5");
        let xmp = doc.add_object(Stream::new(
            dictionary! { "Subtype" => "XML" }, // note: no /Type
            b"<x><dc:title>Zanzibar xmp</dc:title></x>".to_vec(),
        ));
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Metadata" => Object::Reference(xmp) });
        doc.trailer.set("Root", catalog);
        let f = run(&doc, "Zanzibar");
        assert_eq!(f.len(), 1, "untyped catalog XMP missed: {f:?}");
    }
}
