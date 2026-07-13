//! Vector 17 — optional content ("layers", `/OCProperties`). An OCG's `/Name`
//! label (e.g. "SSN — internal only") and the `/D` default-config `/Name` are
//! catalog-reachable regardless of any page, so they survive a page flatten
//! (spec §4-H). Inverts Tumbler's `/OCProperties` scrub. Every `/Type /OCG`
//! (and `/OCMD`) name in the object graph is scanned.

use crate::extract::{findings_in, CheckOutcome, DocContext, VectorCheck};
use crate::pdf;
use crate::query::Query;
use crate::report::Vector;

pub struct OptionalContent;

impl VectorCheck for OptionalContent {
    fn id(&self) -> &'static str {
        "optional_content"
    }
    fn label(&self) -> &'static str {
        "Optional content"
    }
    fn vector(&self) -> Vector {
        Vector::OptionalContent
    }
    fn method(&self) -> &'static str {
        "OCG /Name labels + /D config"
    }

    fn run(&self, ctx: &DocContext, query: &Query) -> CheckOutcome {
        let Some(doc) = ctx.lopdf else {
            return ctx.lopdf_unavailable();
        };
        let mut findings = Vec::new();

        // Every optional-content group / membership dict carries a /Name.
        for (id, dict) in pdf::iter_dicts(doc) {
            if pdf::name_is(dict, b"Type", &[b"OCG", b"OCMD"]) {
                if let Some(name) = pdf::get_string(doc, dict, b"Name") {
                    findings_in(&name, query, Vector::OptionalContent, &format!("OCG /Name (object {} {})", id.0, id.1), None, &mut findings);
                }
            }
        }

        // The /OCProperties /D default config also carries a /Name label.
        if let Some(ocp) = pdf::catalog(doc).and_then(|c| pdf::get_dict(doc, c, b"OCProperties")) {
            if let Some(d) = pdf::get_dict(doc, ocp, b"D") {
                if let Some(name) = pdf::get_string(doc, d, b"Name") {
                    findings_in(&name, query, Vector::OptionalContent, "/OCProperties /D /Name", None, &mut findings);
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
    use lopdf::{dictionary, Document, Object};

    #[test]
    fn finds_secret_in_ocg_name() {
        let mut doc = Document::with_version("1.5");
        doc.add_object(dictionary! { "Type" => "OCG", "Name" => Object::string_literal("Zanzibar layer") });
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog" });
        doc.trailer.set("Root", catalog);

        let ctx = DocContext::new(&[], Some(&doc), None);
        let q = Query::literal(["Zanzibar".to_string()], false, false).unwrap();
        let f = match OptionalContent.run(&ctx, &q) {
            CheckOutcome::Ran { findings, .. } => findings,
            CheckOutcome::Skipped { reason: r, .. } => panic!("skip: {r}"),
        };
        assert_eq!(f.len(), 1, "{f:?}");
    }
}
