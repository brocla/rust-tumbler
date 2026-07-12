//! Vector 15 — scripts & actions. A JavaScript action's source (`/JS`) can
//! quote redacted values; it is reachable from `/Names/JavaScript`,
//! `/OpenAction`, and `/AA` additional-actions (spec §4-G). Inverts Tumbler's
//! `/JavaScript` scrub. Every `/S /JavaScript` action in the object graph is
//! scanned, wherever referenced.

use crate::extract::{findings_in, CheckOutcome, DocContext, VectorCheck};
use crate::pdf;
use crate::query::Query;
use crate::report::Vector;
use lopdf::Object;

pub struct Scripts;

impl VectorCheck for Scripts {
    fn id(&self) -> &'static str {
        "scripts"
    }
    fn label(&self) -> &'static str {
        "Scripts & actions"
    }
    fn vector(&self) -> Vector {
        Vector::Scripts
    }
    fn method(&self) -> &'static str {
        "/JavaScript, /OpenAction, /AA /JS source"
    }

    fn run(&self, ctx: &DocContext, query: &Query) -> CheckOutcome {
        let Some(doc) = ctx.lopdf else {
            return CheckOutcome::unavailable("lopdf could not parse this document");
        };
        let mut findings = Vec::new();

        // Every JavaScript action, wherever referenced: a dict with
        // /S /JavaScript carries its source in /JS (a string or a stream).
        for (id, dict) in pdf::iter_dicts(doc) {
            if !pdf::name_is(dict, b"S", &[b"JavaScript"]) {
                continue;
            }
            match dict.get(b"JS").ok().and_then(|o| pdf::resolve(doc, o)) {
                Some(Object::String(s, _)) => {
                    let text = pdf::decode_pdf_text(s);
                    findings_in(&text, query, Vector::Scripts, &format!("JavaScript action (object {} {})", id.0, id.1), None, &mut findings);
                }
                Some(obj @ Object::Stream(_)) => {
                    if let Some(bytes) = pdf::stream_object_bytes(obj) {
                        let text = pdf::decode_stream_text(&bytes);
                        findings_in(&text, query, Vector::Scripts, &format!("JavaScript action stream (object {} {})", id.0, id.1), None, &mut findings);
                    }
                }
                _ => {}
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
    fn finds_secret_in_js_action() {
        let mut doc = Document::with_version("1.5");
        let js = doc.add_object(dictionary! {
            "S" => "JavaScript",
            "JS" => Object::string_literal("app.alert(\"Zanzibar js\");"),
        });
        let names = doc.add_object(dictionary! {
            "JavaScript" => dictionary! { "Names" => vec![Object::string_literal("docjs"), Object::Reference(js)] },
        });
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Names" => Object::Reference(names) });
        doc.trailer.set("Root", catalog);

        let ctx = DocContext { bytes: &[], lopdf: Some(&doc), pdfium: None };
        let q = Query::literal(["Zanzibar".to_string()], false, false).unwrap();
        let f = match Scripts.run(&ctx, &q) {
            CheckOutcome::Ran { findings, .. } => findings,
            CheckOutcome::Skipped { reason: r, .. } => panic!("skip: {r}"),
        };
        assert_eq!(f.len(), 1, "{f:?}");
    }
}
