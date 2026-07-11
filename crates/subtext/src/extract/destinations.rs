//! Vector 8 — named destinations (`/Names/Dests` and the legacy `/Dests`). A
//! destination *name* (the key) can itself echo redacted text (spec §4-C,
//! +audit).

use crate::extract::{findings_in, CheckOutcome, DocContext, VectorCheck};
use crate::pdf;
use crate::query::Query;
use crate::report::{Finding, Vector};

pub struct Destinations;

impl VectorCheck for Destinations {
    fn id(&self) -> &'static str {
        "destinations"
    }
    fn label(&self) -> &'static str {
        "Named destinations"
    }
    fn vector(&self) -> Vector {
        Vector::Destinations
    }
    fn method(&self) -> &'static str {
        "/Names/Dests + /Dests"
    }

    fn run(&self, ctx: &DocContext, query: &Query) -> CheckOutcome {
        let Some(doc) = ctx.lopdf else {
            return CheckOutcome::Skipped("lopdf could not parse this document".to_string());
        };
        let Some(catalog) = pdf::catalog(doc) else {
            return CheckOutcome::ran(Vec::new());
        };
        let mut findings: Vec<Finding> = Vec::new();

        // Modern: catalog /Names /Dests is a name tree keyed by destination name.
        if let Some(names) = pdf::get_dict(doc, catalog, b"Names") {
            if let Some(dests) = pdf::get_dict(doc, names, b"Dests") {
                pdf::walk_name_tree(doc, dests, |name, _value| {
                    findings_in(&name, query, Vector::Destinations, "Named destination", None, &mut findings);
                });
            }
        }
        // Legacy: catalog /Dests is a plain dictionary keyed by destination name.
        if let Some(dests) = pdf::get_dict(doc, catalog, b"Dests") {
            for (name, _value) in dests.iter() {
                let name = String::from_utf8_lossy(name);
                findings_in(&name, query, Vector::Destinations, "Named destination (/Dests)", None, &mut findings);
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
    fn finds_secret_in_legacy_dest_name() {
        let mut doc = Document::with_version("1.5");
        let dests = doc.add_object(dictionary! {
            "Zanzibar_section" => vec![Object::Integer(1)],
        });
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Dests" => Object::Reference(dests) });
        doc.trailer.set("Root", catalog);

        let ctx = DocContext { bytes: &[], lopdf: Some(&doc), pdfium: None };
        let q = Query::literal(["Zanzibar".to_string()], false, false).unwrap();
        let f = match Destinations.run(&ctx, &q) {
            CheckOutcome::Ran { findings, .. } => findings,
            CheckOutcome::Skipped(r) => panic!("skip: {r}"),
        };
        assert_eq!(f.len(), 1);
    }
}
