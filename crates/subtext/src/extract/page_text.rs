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

use crate::extract::{CheckOutcome, DocContext, VectorCheck};
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
        CheckOutcome::Ran(find_in_pages(doc, query))
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
        for span in query.find_all(&full) {
            findings.push(Finding {
                vector: Vector::PageText,
                location: format!("page {page_num}"),
                matched_text: span.text.clone(),
                context: snippet(&full, span.start, span.end),
                page: Some(page_num),
                revision: None,
                container: None,
            });
        }
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

/// A trimmed one-line snippet of `haystack` around `[start, end)`, with up to
/// `PAD` chars of context on each side and ellipses when truncated. Operates on
/// char boundaries so multi-byte text is never split mid-codepoint.
fn snippet(haystack: &str, start: usize, end: usize) -> String {
    const PAD: usize = 40;
    let lo = floor_char_boundary(haystack, start.saturating_sub(PAD));
    let hi = ceil_char_boundary(haystack, (end + PAD).min(haystack.len()));
    let mut out = String::new();
    if lo > 0 {
        out.push('…');
    }
    out.push_str(haystack[lo..hi].trim());
    if hi < haystack.len() {
        out.push('…');
    }
    // Collapse any embedded newlines/tabs so the snippet stays one line.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Largest char boundary `<= i` (std's `floor_char_boundary` is still nightly).
fn floor_char_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Smallest char boundary `>= i`.
fn ceil_char_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_trims_and_ellipsizes() {
        // A match far enough into a long string to be padded on both sides.
        let prefix = "x".repeat(60);
        let suffix = "y".repeat(60);
        let text = format!("{prefix} Zanzibar {suffix}");
        let start = prefix.len() + 1;
        let s = snippet(&text, start, start + "Zanzibar".len());
        assert!(s.contains("Zanzibar"));
        assert!(s.starts_with('…'), "expected leading ellipsis, got: {s}");
        assert!(s.ends_with('…'), "expected trailing ellipsis, got: {s}");
    }

    #[test]
    fn snippet_no_ellipsis_when_whole_string_fits() {
        let text = "short Zanzibar text";
        let s = snippet(text, 6, 14);
        assert_eq!(s, "short Zanzibar text");
    }

    #[test]
    fn snippet_handles_multibyte_without_panicking() {
        let text = "café Zanzibar café";
        let s = snippet(text, 5, 13);
        assert!(s.contains("Zanzibar"));
    }
}
