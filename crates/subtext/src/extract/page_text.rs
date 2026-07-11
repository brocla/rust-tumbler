//! Vector 1 — page text (pdfium extraction).
//!
//! The single most powerful extractor: pdfium walks each page's characters in
//! document order (including nested Form XObjects) and yields their Unicode
//! values via the font's `/ToUnicode`, so this one method already catches text
//! hidden under a black box, invisible text (render mode 3 / OCR sandwich),
//! tiny / white-on-white text, off-page text (extraction ignores position),
//! optional-content ("layer") text (extraction ignores on/off), and the
//! ToUnicode spoof (glyphs render blank but map to the secret). Split text
//! (`[(Zan)-14(zibar)] TJ`, or across a multi-stream `/Contents`) reassembles
//! into continuous reading-order text here — which is why page text must be
//! *extraction*, not a byte scan (spec §4-A, §4-L).
//!
//! Blueprint-reused from Tumbler's `text.rs::page_text_in_document_order` and
//! `search_document_impl` (spec §6): the same walk, reimplemented as a pure
//! function on a `PdfDocument` with no `AppState` coupling.

use crate::extract::{findings_in, CheckOutcome, DocContext, VectorCheck};
use crate::query::Query;
use crate::report::{Finding, Vector};
use pdfium_render::prelude::{PdfDocument, PdfPageText};

pub struct PageText;

impl VectorCheck for PageText {
    fn id(&self) -> &'static str {
        "page_text"
    }
    fn label(&self) -> &'static str {
        "Page text"
    }
    fn vector(&self) -> Vector {
        Vector::PageText
    }
    fn method(&self) -> &'static str {
        "pdfium text extraction"
    }

    fn run(&self, ctx: &DocContext, query: &Query) -> CheckOutcome {
        let Some(doc) = ctx.pdfium else {
            return CheckOutcome::Skipped(
                "pdfium could not load this document (needed for text extraction)".to_string(),
            );
        };
        CheckOutcome::ran(find_in_pages(doc, query))
    }
}

/// Runs the query against every page's extracted text, returning one finding
/// per match with a trimmed context snippet.
fn find_in_pages(doc: &PdfDocument, query: &Query) -> Vec<Finding> {
    let mut findings = Vec::new();
    let pages = doc.pages();
    for (idx, page) in pages.iter().enumerate() {
        let page_num = (idx + 1) as u32;
        let Ok(text) = page.text() else { continue };
        let full = page_text_in_document_order(&text);
        findings_in(
            &full,
            query,
            Vector::PageText,
            &format!("page {page_num}"),
            Some(page_num),
            &mut findings,
        );
    }
    findings
}

/// A page's full text by walking characters in document order and
/// concatenating their Unicode values. Ported from Tumbler's
/// `page_text_in_document_order` (text.rs): it deliberately avoids
/// `PdfPageText::all()`, whose geometric reading-order reconstruction can
/// scramble rotated / multi-column layouts (Tumbler issue #80).
fn page_text_in_document_order(text: &PdfPageText) -> String {
    text.chars()
        .iter()
        .filter_map(|ch| ch.unicode_char())
        .collect()
}

