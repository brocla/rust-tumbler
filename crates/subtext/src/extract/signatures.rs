//! Vector 18 — digital signatures. A signature dictionary's `/Contents` holds
//! the PKCS#7/CMS blob as DER bytes (a certificate subject or signed attribute
//! can echo redacted text), and its `/Name` `/Reason` `/Location` `/ContactInfo`
//! are plain strings (spec §4-K, +audit). Best-effort byte scan — full CMS
//! parsing is out of scope (Tumbler's is Windows-only). Signature dicts are
//! found by their `/ByteRange`.

use crate::extract::{findings_in, CheckOutcome, DocContext, VectorCheck};
use crate::pdf;
use crate::query::Query;
use crate::report::Vector;
use lopdf::Object;

pub struct Signatures;

const SIG_TEXT_KEYS: &[&[u8]] = &[b"Name", b"Reason", b"Location", b"ContactInfo"];

impl VectorCheck for Signatures {
    fn id(&self) -> &'static str {
        "signatures"
    }
    fn label(&self) -> &'static str {
        "Signatures"
    }
    fn vector(&self) -> Vector {
        Vector::Signatures
    }
    fn method(&self) -> &'static str {
        "/Contents DER byte scan + sig strings"
    }

    fn run(&self, ctx: &DocContext, query: &Query) -> CheckOutcome {
        let Some(doc) = ctx.lopdf else {
            return CheckOutcome::Skipped("lopdf could not parse this document".to_string());
        };
        let mut findings = Vec::new();

        for (id, obj) in &doc.objects {
            let Object::Dictionary(dict) = obj else { continue };
            // A signature dictionary is identified by /ByteRange.
            if dict.get(b"ByteRange").is_err() {
                continue;
            }
            // /Contents: the CMS blob (lopdf already hex-decoded it to DER bytes).
            if let Ok(contents) = dict.get(b"Contents").and_then(|o| o.as_str()) {
                let text = String::from_utf8_lossy(contents);
                findings_in(&text, query, Vector::Signatures, &format!("signature /Contents DER (object {} {})", id.0, id.1), None, &mut findings);
            }
            for key in SIG_TEXT_KEYS {
                if let Some(text) = pdf::get_string(doc, dict, key) {
                    let key = String::from_utf8_lossy(key);
                    findings_in(&text, query, Vector::Signatures, &format!("signature /{key}"), None, &mut findings);
                }
            }
        }
        CheckOutcome::ran(findings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::CheckOutcome;
    use lopdf::{dictionary, Document};

    #[test]
    fn finds_secret_in_signature_reason() {
        let mut doc = Document::with_version("1.5");
        doc.add_object(dictionary! {
            "Type" => "Sig",
            "ByteRange" => vec![Object::Integer(0), Object::Integer(10), Object::Integer(20), Object::Integer(30)],
            "Contents" => Object::string_literal(""),
            "Reason" => Object::string_literal("Zanzibar approval"),
        });
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog" });
        doc.trailer.set("Root", catalog);

        let ctx = DocContext { bytes: &[], lopdf: Some(&doc), pdfium: None };
        let q = Query::literal(["Zanzibar".to_string()], false, false).unwrap();
        let f = match Signatures.run(&ctx, &q) {
            CheckOutcome::Ran { findings, .. } => findings,
            CheckOutcome::Skipped(r) => panic!("skip: {r}"),
        };
        assert!(f.iter().any(|x| x.location.contains("/Reason")), "{f:?}");
    }
}
