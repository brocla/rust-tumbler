//! Vector 12 — AcroForm fields. A field's `/V` (value), `/DV` (default), `/TU`
//! (tooltip), and `/T` (name) can hold user-entered data that survives a page-
//! only redaction, and a field with no widget is invisible to page-text
//! extraction (spec §4-E). Inverts Tumbler's AcroForm scrub. Also scans the
//! form-level `/DA` default appearance string.

use crate::extract::{findings_in, CheckOutcome, DocContext, VectorCheck};
use crate::pdf;
use crate::query::Query;
use crate::report::{Finding, Vector};
use lopdf::{Dictionary, Document, Object};

pub struct Forms;

/// Field keys carrying text.
const FIELD_KEYS: &[&[u8]] = &[b"V", b"DV", b"TU", b"T"];

impl VectorCheck for Forms {
    fn id(&self) -> &'static str {
        "forms"
    }
    fn label(&self) -> &'static str {
        "Form fields"
    }
    fn vector(&self) -> Vector {
        Vector::Forms
    }
    fn method(&self) -> &'static str {
        "AcroForm /Fields /V/DV/TU/T"
    }

    fn run(&self, ctx: &DocContext, query: &Query) -> CheckOutcome {
        let Some(doc) = ctx.lopdf else {
            return CheckOutcome::unavailable("lopdf could not parse this document");
        };
        let Some(catalog) = pdf::catalog(doc) else {
            return CheckOutcome::unavailable("document catalog could not be read");
        };
        // No AcroForm at all is a legitimate "no forms present" — clean, not a
        // blind spot.
        let Some(acroform) = pdf::get_dict(doc, catalog, b"AcroForm") else {
            return CheckOutcome::ran(Vec::new());
        };
        let mut findings = Vec::new();

        // Form-level default appearance.
        if let Some(da) = pdf::get_string(doc, acroform, b"DA") {
            findings_in(&da, query, Vector::Forms, "AcroForm /DA", None, &mut findings);
        }

        if let Some(fields) = pdf::get_array(doc, acroform, b"Fields") {
            let mut budget = 100_000u32;
            for field in fields {
                if let Some(dict) = pdf::resolve(doc, field).and_then(|o| o.as_dict().ok()) {
                    walk_field(doc, dict, query, &mut findings, 0, &mut budget);
                }
            }
        }
        CheckOutcome::ran(findings)
    }
}

fn walk_field(
    doc: &Document,
    field: &Dictionary,
    query: &Query,
    findings: &mut Vec<Finding>,
    depth: u32,
    budget: &mut u32,
) {
    if depth > 64 || *budget == 0 {
        return;
    }
    *budget -= 1;
    for key in FIELD_KEYS {
        match field.get(key).ok().and_then(|o| pdf::resolve(doc, o)) {
            Some(Object::String(..)) => {
                if let Some(text) = pdf::get_string(doc, field, key) {
                    let key = String::from_utf8_lossy(key);
                    findings_in(&text, query, Vector::Forms, &format!("form field /{key}"), None, findings);
                }
            }
            // A rich-text /V can be a stream.
            Some(obj @ Object::Stream(_)) => {
                if let Some(bytes) = pdf::stream_object_bytes(obj) {
                    let text = pdf::decode_pdf_text(&bytes);
                    let key = String::from_utf8_lossy(key);
                    findings_in(&text, query, Vector::Forms, &format!("form field /{key} (stream)"), None, findings);
                }
            }
            _ => {}
        }
    }
    if let Some(kids) = pdf::get_array(doc, field, b"Kids") {
        for kid in kids {
            if let Some(dict) = pdf::resolve(doc, kid).and_then(|o| o.as_dict().ok()) {
                walk_field(doc, dict, query, findings, depth + 1, budget);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::CheckOutcome;
    use lopdf::dictionary;

    #[test]
    fn finds_secret_in_hierarchical_field_value() {
        let mut doc = Document::with_version("1.5");
        let widget = doc.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Widget" });
        let parent = doc.add_object(dictionary! {
            "FT" => "Tx", "T" => Object::string_literal("ssn"),
            "V" => Object::string_literal("Zanzibar fieldvalue"),
            "Kids" => vec![Object::Reference(widget)],
        });
        let acroform = doc.add_object(dictionary! { "Fields" => vec![Object::Reference(parent)] });
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "AcroForm" => Object::Reference(acroform) });
        doc.trailer.set("Root", catalog);

        let ctx = DocContext { bytes: &[], lopdf: Some(&doc), pdfium: None };
        let q = Query::literal(["Zanzibar".to_string()], false, false).unwrap();
        let f = match Forms.run(&ctx, &q) {
            CheckOutcome::Ran { findings, .. } => findings,
            CheckOutcome::Skipped { reason: r, .. } => panic!("skip: {r}"),
        };
        assert!(f.iter().any(|x| x.location == "form field /V"), "{f:?}");
    }
}
