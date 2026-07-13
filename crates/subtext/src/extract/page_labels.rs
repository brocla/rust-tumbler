//! Vector 7 — page labels (`/PageLabels`). A label range's `/P` prefix string
//! (e.g. "Confidential-") can carry redacted text (spec §4-C, +audit).

use crate::extract::{findings_in, CheckOutcome, DocContext, VectorCheck};
use crate::pdf;
use crate::query::Query;
use crate::report::{Finding, Vector};

pub struct PageLabels;

impl VectorCheck for PageLabels {
    fn id(&self) -> &'static str {
        "page_labels"
    }
    fn label(&self) -> &'static str {
        "Page labels"
    }
    fn vector(&self) -> Vector {
        Vector::PageLabels
    }
    fn method(&self) -> &'static str {
        "/PageLabels /P prefixes"
    }

    fn run(&self, ctx: &DocContext, query: &Query) -> CheckOutcome {
        let Some(doc) = ctx.lopdf else {
            return ctx.lopdf_unavailable();
        };
        let Some(catalog) = pdf::catalog(doc) else {
            return CheckOutcome::unavailable("document catalog could not be read");
        };
        let mut findings: Vec<Finding> = Vec::new();
        if let Some(tree) = pdf::get_dict(doc, catalog, b"PageLabels") {
            pdf::walk_number_tree(doc, tree, |value| {
                if let Ok(label_dict) = value.as_dict() {
                    if let Some(prefix) = pdf::get_string(doc, label_dict, b"P") {
                        findings_in(&prefix, query, Vector::PageLabels, "PageLabel /P prefix", None, &mut findings);
                    }
                }
            });
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
    fn finds_secret_in_label_prefix() {
        let mut doc = Document::with_version("1.5");
        let label = doc.add_object(dictionary! { "S" => "D", "P" => Object::string_literal("Zanzibar-") });
        let tree = doc.add_object(dictionary! {
            "Nums" => vec![Object::Integer(0), Object::Reference(label)],
        });
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "PageLabels" => Object::Reference(tree) });
        doc.trailer.set("Root", catalog);

        let ctx = DocContext::new(&[], Some(&doc), None);
        let q = Query::literal(["Zanzibar".to_string()], false, false).unwrap();
        let f = match PageLabels.run(&ctx, &q) {
            CheckOutcome::Ran { findings, .. } => findings,
            CheckOutcome::Skipped { reason: r, .. } => panic!("skip: {r}"),
        };
        assert_eq!(f.len(), 1);
    }
}
