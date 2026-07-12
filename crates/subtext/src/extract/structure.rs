//! Vector 4 — the tagged-PDF structure tree (`/StructTreeRoot`). A StructElem's
//! `/ActualText`, `/Alt`, `/E` (expansion) and `/T` (title) routinely duplicate
//! visible page text, so a redaction that only touched the page leaves it here
//! (spec §4-C). Inverts Tumbler's `/StructTreeRoot` scrub.

use crate::extract::{scan_dict_keys, CheckOutcome, DocContext, VectorCheck};
use crate::pdf;
use crate::query::Query;
use crate::report::{Finding, Vector};
use lopdf::{Dictionary, Document, Object};

pub struct StructureTree;

/// StructElem keys that carry human-readable text.
const TEXT_KEYS: &[&[u8]] = &[b"ActualText", b"Alt", b"E", b"T"];

impl VectorCheck for StructureTree {
    fn id(&self) -> &'static str {
        "structure_tree"
    }
    fn label(&self) -> &'static str {
        "Structure tree"
    }
    fn vector(&self) -> Vector {
        Vector::StructureTree
    }
    fn method(&self) -> &'static str {
        "/StructTreeRoot ActualText/Alt/E/T"
    }

    fn run(&self, ctx: &DocContext, query: &Query) -> CheckOutcome {
        let Some(doc) = ctx.lopdf else {
            return CheckOutcome::unavailable("lopdf could not parse this document");
        };
        let Some(catalog) = pdf::catalog(doc) else {
            return CheckOutcome::unavailable("document catalog could not be read");
        };
        let mut findings = Vec::new();
        if let Some(root) = pdf::get_dict(doc, catalog, b"StructTreeRoot") {
            let mut budget = 100_000u32;
            walk(doc, root, query, &mut findings, 0, &mut budget);
        }
        CheckOutcome::ran(findings)
    }
}

/// Recursively walks StructElem nodes via `/K` (kids), matching the text keys.
fn walk(
    doc: &Document,
    node: &Dictionary,
    query: &Query,
    findings: &mut Vec<Finding>,
    depth: u32,
    budget: &mut u32,
) {
    if depth > 64 || *budget == 0 {
        return;
    }
    *budget -= 1;
    scan_dict_keys(doc, node, TEXT_KEYS, query, Vector::StructureTree, None, |k| format!("StructElem /{k}"), findings);
    // /K is a kid, an array of kids, or a reference; each kid is a StructElem
    // dict, a reference to one, or a marked-content id integer (ignored).
    match node.get(b"K").ok().and_then(|o| pdf::resolve(doc, o)) {
        Some(Object::Array(kids)) => {
            for kid in kids {
                if let Some(dict) = pdf::resolve(doc, kid).and_then(|o| o.as_dict().ok()) {
                    walk(doc, dict, query, findings, depth + 1, budget);
                }
            }
        }
        Some(Object::Dictionary(dict)) => walk(doc, dict, query, findings, depth + 1, budget),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::CheckOutcome;
    use lopdf::dictionary;

    fn run(doc: &Document, term: &str) -> Vec<Finding> {
        let ctx = DocContext { bytes: &[], lopdf: Some(doc), pdfium: None };
        let q = Query::literal([term.to_string()], false, false).unwrap();
        match StructureTree.run(&ctx, &q) {
            CheckOutcome::Ran { findings, .. } => findings,
            CheckOutcome::Skipped { reason: r, .. } => panic!("unexpected skip: {r}"),
        }
    }

    #[test]
    fn finds_secret_in_actualtext() {
        let mut doc = Document::with_version("1.5");
        let elem = doc.add_object(dictionary! {
            "Type" => "StructElem",
            "S" => "P",
            "ActualText" => Object::string_literal("Zanzibar actualtext"),
        });
        let root = doc.add_object(dictionary! {
            "Type" => "StructTreeRoot",
            "K" => vec![Object::Reference(elem)],
        });
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "StructTreeRoot" => root });
        doc.trailer.set("Root", catalog);
        let f = run(&doc, "Zanzibar");
        assert!(f.iter().any(|x| x.location == "StructElem /ActualText"), "{f:?}");
    }
}
