//! Vector 6 — bookmarks (`/Outlines`). Every outline item's `/Title` can echo
//! a heading that duplicates redacted page text (spec §4-C). Inverts Tumbler's
//! `/Outlines` scrub.

use crate::extract::{findings_in, CheckOutcome, DocContext, VectorCheck};
use crate::pdf;
use crate::query::Query;
use crate::report::{Finding, Vector};
use lopdf::{Dictionary, Document};

pub struct Outlines;

impl VectorCheck for Outlines {
    fn id(&self) -> &'static str {
        "outlines"
    }
    fn label(&self) -> &'static str {
        "Bookmarks"
    }
    fn vector(&self) -> Vector {
        Vector::Outlines
    }
    fn method(&self) -> &'static str {
        "/Outlines /Title walk"
    }

    fn run(&self, ctx: &DocContext, query: &Query) -> CheckOutcome {
        let Some(doc) = ctx.lopdf else {
            return ctx.lopdf_unavailable();
        };
        let Some(catalog) = pdf::catalog(doc) else {
            return CheckOutcome::unavailable("document catalog could not be read");
        };
        let mut findings = Vec::new();
        if let Some(root) = pdf::get_dict(doc, catalog, b"Outlines") {
            // The outline is a doubly-linked tree: descend /First, follow /Next.
            let mut budget = 100_000u32;
            if let Some(first) = pdf::get_dict(doc, root, b"First") {
                walk_siblings(doc, first, query, &mut findings, 0, &mut budget);
            }
        }
        CheckOutcome::ran(findings)
    }
}

fn walk_siblings(
    doc: &Document,
    node: &Dictionary,
    query: &Query,
    findings: &mut Vec<Finding>,
    depth: u32,
    budget: &mut u32,
) {
    let mut current = Some(node);
    while let Some(item) = current {
        if depth > 64 || *budget == 0 {
            return;
        }
        *budget -= 1;
        if let Some(title) = pdf::get_string(doc, item, b"Title") {
            findings_in(&title, query, Vector::Outlines, "Bookmark /Title", None, findings);
        }
        if let Some(child) = pdf::get_dict(doc, item, b"First") {
            walk_siblings(doc, child, query, findings, depth + 1, budget);
        }
        current = pdf::get_dict(doc, item, b"Next");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::CheckOutcome;
    use lopdf::{dictionary, Object};

    #[test]
    fn finds_secret_in_bookmark_title() {
        let mut doc = Document::with_version("1.5");
        let outlines_id = doc.new_object_id();
        let item = doc.add_object(dictionary! {
            "Title" => Object::string_literal("Zanzibar bookmark"),
            "Parent" => Object::Reference(outlines_id),
        });
        doc.objects.insert(
            outlines_id,
            Object::Dictionary(dictionary! { "Type" => "Outlines", "First" => Object::Reference(item), "Last" => Object::Reference(item) }),
        );
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Outlines" => Object::Reference(outlines_id) });
        doc.trailer.set("Root", catalog);

        let ctx = DocContext::new(&[], Some(&doc), None);
        let q = Query::literal(["Zanzibar".to_string()], false, false).unwrap();
        let f = match Outlines.run(&ctx, &q) {
            CheckOutcome::Ran { findings, .. } => findings,
            CheckOutcome::Skipped { reason: r, .. } => panic!("skip: {r}"),
        };
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].location, "Bookmark /Title");
    }
}
