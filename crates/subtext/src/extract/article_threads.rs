//! Vector 9 — article threads (`/Threads`). Each thread's `/I` information dict
//! carries Info-style strings (`/Title`, `/Author`, `/Subject`, `/Keywords`)
//! that can echo redacted text (spec §4-C, +audit).

use crate::extract::{scan_dict_keys, CheckOutcome, DocContext, VectorCheck};
use crate::pdf;
use crate::query::Query;
use crate::report::{Finding, Vector};

pub struct ArticleThreads;

const INFO_KEYS: &[&[u8]] = &[b"Title", b"Author", b"Subject", b"Keywords"];

impl VectorCheck for ArticleThreads {
    fn id(&self) -> &'static str {
        "article_threads"
    }
    fn label(&self) -> &'static str {
        "Article threads"
    }
    fn vector(&self) -> Vector {
        Vector::ArticleThreads
    }
    fn method(&self) -> &'static str {
        "/Threads bead /I info"
    }

    fn run(&self, ctx: &DocContext, query: &Query) -> CheckOutcome {
        let Some(doc) = ctx.lopdf else {
            return CheckOutcome::unavailable("lopdf could not parse this document");
        };
        let Some(catalog) = pdf::catalog(doc) else {
            return CheckOutcome::unavailable("document catalog could not be read");
        };
        let mut findings: Vec<Finding> = Vec::new();
        if let Some(threads) = pdf::get_array(doc, catalog, b"Threads") {
            for thread in threads {
                let Some(thread) = pdf::resolve(doc, thread).and_then(|o| o.as_dict().ok()) else {
                    continue;
                };
                if let Some(info) = pdf::get_dict(doc, thread, b"I") {
                    scan_dict_keys(doc, info, INFO_KEYS, query, Vector::ArticleThreads, None, |k| format!("Thread /I /{k}"), &mut findings);
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
    fn finds_secret_in_thread_info() {
        let mut doc = Document::with_version("1.5");
        let info = dictionary! { "Title" => Object::string_literal("Zanzibar thread") };
        let thread = doc.add_object(dictionary! { "Type" => "Thread", "I" => info });
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Threads" => vec![Object::Reference(thread)] });
        doc.trailer.set("Root", catalog);

        let ctx = DocContext { bytes: &[], lopdf: Some(&doc), pdfium: None };
        let q = Query::literal(["Zanzibar".to_string()], false, false).unwrap();
        let f = match ArticleThreads.run(&ctx, &q) {
            CheckOutcome::Ran { findings, .. } => findings,
            CheckOutcome::Skipped { reason: r, .. } => panic!("skip: {r}"),
        };
        assert_eq!(f.len(), 1);
    }
}
