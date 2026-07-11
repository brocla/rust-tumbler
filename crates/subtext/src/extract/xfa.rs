//! Vector 13 — XFA form packets (`/AcroForm /XFA`). The `datasets` packet
//! mirrors user-entered field values verbatim and the template packet holds
//! captions — both XML, invisible to every pdfium text API (spec §4-E).
//! Inverts Tumbler's `/XFA` scrub (issue #68).

use crate::extract::{findings_in, CheckOutcome, DocContext, VectorCheck};
use crate::pdf;
use crate::query::Query;
use crate::report::{Finding, Vector};
use crate::xml;
use lopdf::Object;

pub struct Xfa;

impl VectorCheck for Xfa {
    fn id(&self) -> &'static str {
        "xfa"
    }
    fn label(&self) -> &'static str {
        "XFA forms"
    }
    fn vector(&self) -> Vector {
        Vector::Xfa
    }
    fn method(&self) -> &'static str {
        "/XFA datasets + template"
    }

    fn run(&self, ctx: &DocContext, query: &Query) -> CheckOutcome {
        let Some(doc) = ctx.lopdf else {
            return CheckOutcome::Skipped("lopdf could not parse this document".to_string());
        };
        let Some(acroform) = pdf::catalog(doc).and_then(|c| pdf::get_dict(doc, c, b"AcroForm")) else {
            return CheckOutcome::ran(Vec::new());
        };
        let Some(xfa) = acroform.get(b"XFA").ok().and_then(|o| pdf::resolve(doc, o)) else {
            return CheckOutcome::ran(Vec::new());
        };
        let mut findings = Vec::new();

        match xfa {
            // Single XDP stream holding the whole XFA document.
            obj @ Object::Stream(_) => {
                scan_packet(obj, query, "XFA packet", &mut findings);
            }
            // Alternating [name, stream, name, stream, …] named packets.
            Object::Array(parts) => {
                let mut label = String::from("XFA packet");
                for part in parts {
                    match pdf::resolve(doc, part) {
                        Some(Object::String(name, _)) => {
                            label = format!("XFA {} packet", pdf::decode_pdf_text(name));
                        }
                        Some(obj @ Object::Stream(_)) => {
                            scan_packet(obj, query, &label, &mut findings);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        CheckOutcome::ran(findings)
    }
}

fn scan_packet(obj: &Object, query: &Query, label: &str, findings: &mut Vec<Finding>) {
    let Some(bytes) = pdf::stream_object_bytes(obj) else { return };
    let text = xml::visible_text(&bytes);
    findings_in(&text, query, Vector::Xfa, label, None, findings);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::CheckOutcome;
    use lopdf::{dictionary, Document, Stream};

    #[test]
    fn finds_secret_in_datasets_packet() {
        let mut doc = Document::with_version("1.5");
        let datasets = doc.add_object(Stream::new(
            lopdf::Dictionary::new(),
            b"<xfa:datasets><ssn>Zanzibar xfa value</ssn></xfa:datasets>".to_vec(),
        ));
        let acroform = doc.add_object(dictionary! {
            "XFA" => vec![Object::string_literal("datasets"), Object::Reference(datasets)],
        });
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "AcroForm" => Object::Reference(acroform) });
        doc.trailer.set("Root", catalog);

        let ctx = DocContext { bytes: &[], lopdf: Some(&doc), pdfium: None };
        let q = Query::literal(["Zanzibar".to_string()], false, false).unwrap();
        let f = match Xfa.run(&ctx, &q) {
            CheckOutcome::Ran { findings, .. } => findings,
            CheckOutcome::Skipped(r) => panic!("skip: {r}"),
        };
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(f[0].location.contains("datasets"));
    }
}
