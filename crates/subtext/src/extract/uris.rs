//! Vector 16 — URIs & web capture. A URI action (`/S /URI`, `/URI`) can carry
//! the secret in a query string (`?ssn=…`), and web-capture source URLs live in
//! `/SpiderInfo` / the `/URLS` name tree (spec §4-K, +audit). Every `/URI`
//! string in the object graph is scanned, wherever referenced.

use crate::extract::{findings_in, CheckOutcome, DocContext, VectorCheck};
use crate::pdf;
use crate::query::Query;
use crate::report::Vector;

pub struct Uris;

impl VectorCheck for Uris {
    fn id(&self) -> &'static str {
        "uris"
    }
    fn label(&self) -> &'static str {
        "URIs & web capture"
    }
    fn vector(&self) -> Vector {
        Vector::Uris
    }
    fn method(&self) -> &'static str {
        "/URI actions + web-capture URLs"
    }

    fn run(&self, ctx: &DocContext, query: &Query) -> CheckOutcome {
        let Some(doc) = ctx.lopdf else {
            return CheckOutcome::unavailable("lopdf could not parse this document");
        };
        let mut findings = Vec::new();

        // Every /URI string anywhere: URI actions, and the /URLS web-capture
        // name tree (whose values are also dicts with /URI). Iterating all
        // dicts for a /URI entry catches every reference site.
        for (id, dict) in pdf::iter_dicts(doc) {
            if let Some(uri) = pdf::get_string(doc, dict, b"URI") {
                findings_in(&uri, query, Vector::Uris, &format!("/URI (object {} {})", id.0, id.1), None, &mut findings);
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
    fn finds_secret_in_uri_query_string() {
        let mut doc = Document::with_version("1.5");
        doc.add_object(dictionary! {
            "S" => "URI",
            "URI" => Object::string_literal("https://x.example/?q=Zanzibar"),
        });
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog" });
        doc.trailer.set("Root", catalog);

        let ctx = DocContext { bytes: &[], lopdf: Some(&doc), pdfium: None };
        let q = Query::literal(["Zanzibar".to_string()], false, false).unwrap();
        let f = match Uris.run(&ctx, &q) {
            CheckOutcome::Ran { findings, .. } => findings,
            CheckOutcome::Skipped { reason: r, .. } => panic!("skip: {r}"),
        };
        assert_eq!(f.len(), 1, "{f:?}");
    }
}
