//! text_extract, text_find, doc_export_text.

use super::{cancelled, reply, snake_json, ToolResult};
use crate::automation::args::{DocExportTextArgs, TextExtractArgs, TextFindArgs};
use crate::automation::{paths, Automation};
use crate::commands::{ocr, text};
use crate::state::lock_mutex;
use pdfium_render::prelude::PdfPageIndex;
use serde_json::json;

pub(crate) fn text_extract(auto: &mut Automation, args: TextExtractArgs) -> ToolResult {
    let key = auto.key(args.doc)?;
    let pages = match args.pages {
        Some(pages) => pages.into_iter().map(|p| auto.check_page(args.doc, p)).collect::<Result<Vec<_>, _>>()?,
        None => (1..=auto.page_count(args.doc)?).collect(),
    };

    let entry = auto.entry(args.doc)?;
    let entry = lock_mutex(&entry)?;
    let mut out = Vec::with_capacity(pages.len());
    for page in pages {
        let native = entry
            .document
            .pages()
            .get((page - 1) as PdfPageIndex)
            .ok()
            .and_then(|p| p.text().ok().map(|t| text::page_text_in_document_order(&t)))
            .unwrap_or_default();
        let ocr_words = auto.state.get_ocr_words(&key, page);
        // The same precedence search and the GUI's text layer use: a
        // force-re-OCR'd page (issue #97) serves its OCR words over the native
        // layer; otherwise native text wins, and OCR fills pages without it.
        let (text, source) = match ocr_words {
            Some(words) if auto.state.ocr_overrides_native_text(&key, page) => (ocr::ocr_words_to_text(&words), "ocr"),
            _ if !native.trim().is_empty() => (native, "text"),
            Some(words) => (ocr::ocr_words_to_text(&words), "ocr"),
            None => (String::new(), "none"),
        };
        out.push(json!({ "page": page, "text": text, "source": source }));
    }
    reply(json!({ "pages": out }))
}

pub(crate) fn text_find(auto: &mut Automation, args: TextFindArgs) -> ToolResult {
    let key = auto.key(args.doc)?;
    let results =
        text::search_document_impl(&auto.state, key, args.query.clone(), args.match_case, args.whole_word, args.regex)?;
    let total: usize = results.iter().map(|r| r.matches.len()).sum();
    reply(json!({ "query": args.query, "total": total, "results": snake_json(&results)? }))
}

pub(crate) fn doc_export_text(auto: &mut Automation, args: DocExportTextArgs) -> ToolResult {
    let key = auto.key(args.doc)?;
    let target = paths::output_path(auto.root(), &args.path)?.to_string_lossy().into_owned();
    let progress = auto.progress();
    let report = text::export_text_impl(
        |done, total| progress(done, total),
        auto.entry(args.doc)?,
        key,
        target.clone(),
        args.use_ocr,
        auto.state.ocr_engine.clone(),
        auto.state.ocr_cache_handle(),
        auto.cancel_token(),
    )?;
    if report.cancelled {
        return Err(cancelled());
    }
    reply(json!({ "doc": args.doc, "path": target, "pages": report.pages, "ocr_pages": report.ocr_pages }))
}
