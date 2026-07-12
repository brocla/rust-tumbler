//! Vector 21 — raw decompressed byte scan (the backstop).
//!
//! After inflating every stream, run the query over the decompressed bytes.
//! This catches anything the structured extractors missed — but it is
//! deliberately a *backstop*, not the primary method: it scans each stream's
//! bytes in isolation, so it cannot see text split across show operators
//! (`[(Zan)-14(zibar)]TJ`) the way pdfium's `PageText` reassembly can (spec
//! §4-J, §4-L, §14.7). Backstop overlap with earlier vectors is intentional —
//! defence in depth. Image streams are skipped (they are the OCR pass's job).

use crate::extract::{findings_in, CheckOutcome, DocContext, VectorCheck};
use crate::pdf;
use crate::query::Query;
use crate::report::{Finding, Vector};
use lopdf::{Dictionary, Object};

pub struct RawDecompressed;

/// Filters whose output is image data, not text.
const IMAGE_FILTERS: &[&[u8]] = &[b"DCTDecode", b"JPXDecode", b"CCITTFaxDecode", b"JBIG2Decode"];

impl VectorCheck for RawDecompressed {
    fn id(&self) -> &'static str {
        "raw_decompressed"
    }
    fn label(&self) -> &'static str {
        "Raw decompressed scan"
    }
    fn vector(&self) -> Vector {
        Vector::RawDecompressed
    }
    fn method(&self) -> &'static str {
        "inflate every stream + scan (backstop)"
    }

    fn run(&self, ctx: &DocContext, query: &Query) -> CheckOutcome {
        let Some(doc) = ctx.lopdf else {
            return CheckOutcome::unavailable("lopdf could not parse this document");
        };
        let mut findings: Vec<Finding> = Vec::new();

        for (id, obj) in &doc.objects {
            let Object::Stream(stream) = obj else { continue };
            if is_image_stream(&stream.dict) {
                continue;
            }
            // lopdf-unpacked /ObjStm containers are gone from the map, but their
            // decompressed bytes are still reachable here if the container
            // survived; member objects themselves are already in `doc.objects`
            // and covered by the structured vectors.
            if let Some(bytes) = pdf::stream_bytes(doc, *id) {
                let text = pdf::decode_stream_text(&bytes);
                findings_in(
                    &text,
                    query,
                    Vector::RawDecompressed,
                    &format!("decompressed stream (object {} {})", id.0, id.1),
                    None,
                    &mut findings,
                );
            }
        }
        CheckOutcome::ran(findings)
    }
}

/// True when the stream carries image data (an image XObject, or an
/// image-only filter), which the raw *text* scan should skip.
fn is_image_stream(dict: &Dictionary) -> bool {
    if pdf::name_is(dict, b"Subtype", &[b"Image"]) {
        return true;
    }
    // /Filter may be a single name or an array of names.
    match dict.get(b"Filter") {
        Ok(Object::Name(n)) => IMAGE_FILTERS.contains(&n.as_slice()),
        Ok(Object::Array(a)) => a
            .iter()
            .filter_map(|o| o.as_name().ok())
            .any(|n| IMAGE_FILTERS.contains(&n)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{dictionary, Document, Stream};

    fn run(doc: &Document, term: &str) -> Vec<Finding> {
        let ctx = DocContext { bytes: &[], lopdf: Some(doc), pdfium: None };
        let q = Query::literal([term.to_string()], false, false).unwrap();
        match RawDecompressed.run(&ctx, &q) {
            CheckOutcome::Ran { findings, .. } => findings,
            CheckOutcome::Skipped { reason: r, .. } => panic!("skip: {r}"),
        }
    }

    #[test]
    fn finds_secret_in_a_content_stream() {
        let mut doc = Document::with_version("1.5");
        doc.add_object(Stream::new(
            dictionary! {},
            b"BT /F1 12 Tf (Zanzibar original) Tj ET".to_vec(),
        ));
        let f = run(&doc, "Zanzibar");
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(f[0].location.starts_with("decompressed stream"));
    }

    #[test]
    fn skips_image_streams() {
        let mut doc = Document::with_version("1.5");
        // An image whose (nonsensical) bytes happen to contain the term — must
        // not be scanned as text.
        doc.add_object(Stream::new(
            dictionary! { "Type" => "XObject", "Subtype" => "Image", "Filter" => "DCTDecode" },
            b"Zanzibar".to_vec(),
        ));
        assert!(run(&doc, "Zanzibar").is_empty());
    }
}
