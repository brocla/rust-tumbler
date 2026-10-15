//! ocr_status, ocr_recognize.

use super::{cancelled, edited, is_signed, reply, with, ToolResult};
use crate::automation::args::{NoArgs, OcrRecognizeArgs};
use crate::automation::Automation;
use crate::commands::{ocr, text, text_layer};
use crate::state::lock_mutex;
use pdfium_render::prelude::PdfPageIndex;
use serde_json::json;
use std::collections::HashSet;
use std::sync::atomic::Ordering;

pub(crate) fn ocr_status(auto: &mut Automation, _: NoArgs) -> ToolResult {
    // The engine has no separate availability check: the real one finds out
    // whether a language pack is installed when asked to recognize. So ask it,
    // on a small blank image.
    const SIDE: u32 = 64;
    let blank = vec![255u8; (SIDE * SIDE * 4) as usize];
    match auto.state.ocr_engine.recognize(&blank, SIDE, SIDE) {
        Ok(_) => reply(json!({ "available": true })),
        Err(e) => reply(json!({ "available": false, "message": e.to_string() })),
    }
}

/// The GUI's Make Searchable (`ocr_document`, or `ocr_page` per page) followed
/// by Add Text Layer (`add_text_layer`), composed from the same functions.
pub(crate) fn ocr_recognize(auto: &mut Automation, args: OcrRecognizeArgs) -> ToolResult {
    let key = auto.key(args.doc)?;
    let pages = match args.pages {
        Some(pages) => Some(pages.into_iter().map(|p| auto.check_page(args.doc, p)).collect::<Result<Vec<_>, _>>()?),
        None => None,
    };
    let signed = is_signed(auto, &key)?;
    let entry = auto.entry(args.doc)?;
    let engine = auto.state.ocr_engine.clone();
    let cache = auto.state.ocr_cache_handle();
    let overrides = auto.state.ocr_overrides_handle();
    let progress = auto.progress();
    let cancel = auto.cancel_token();

    // Recognition, into the session's OCR cache.
    let recognized = match &pages {
        None => {
            let report = ocr::ocr_document_impl(
                |done, total| progress(done, total),
                entry.clone(),
                key.clone(),
                engine.clone(),
                cache.clone(),
                overrides,
                cancel.clone(),
                args.force,
            )?;
            if report.cancelled {
                return Err(cancelled());
            }
            report.pages_ocred
        }
        // The per-page step ocr_document_impl runs, over just these pages.
        Some(list) => {
            let mut recognized = 0;
            for (i, &page) in list.iter().enumerate() {
                if cancel.load(Ordering::Relaxed) {
                    return Err(cancelled());
                }
                progress(i as u32 + 1, list.len() as u32);
                if !args.force && has_native_text(&entry, page)? {
                    continue;
                }
                let words = ocr::ocr_page_into_cache_forced(&entry, &key, page, &engine, &cache, args.force)?;
                if !words.is_empty() {
                    recognized += 1;
                    if args.force {
                        ocr::override_set(&overrides, &key, page);
                    }
                }
            }
            recognized
        }
    };

    // Embedding: an invisible text layer on the pages that have no text of
    // their own, authored from the words just cached.
    let (mut written, mut skipped, mut changed) = (0, 0, false);
    if args.embed {
        let only: Option<HashSet<u32>> = pages.map(|p| p.into_iter().collect());
        let (report, bytes) = text_layer::add_text_layer_impl_filtered(
            |_, _| {},
            entry,
            key.clone(),
            engine,
            cache,
            cancel,
            only.as_ref(),
            false,
        )?;
        if report.cancelled {
            return Err(cancelled());
        }
        (written, skipped) = (report.pages_written, report.pages_skipped_unsupported_geometry);
        if let Some(bytes) = bytes {
            auto.state.set_buffer_and_refresh(&key, bytes)?;
            changed = true;
        }
    }

    reply(with(
        edited(auto, args.doc, signed && changed)?,
        json!({
            "pages_recognized": recognized,
            "pages_written": written,
            "pages_skipped_unsupported_geometry": skipped,
        }),
    ))
}

fn has_native_text(
    entry: &std::sync::Arc<std::sync::Mutex<crate::state::DocEntry>>,
    page: u32,
) -> Result<bool, crate::automation::ToolError> {
    let entry = lock_mutex(entry)?;
    let text = entry
        .document
        .pages()
        .get((page - 1) as PdfPageIndex)
        .ok()
        .and_then(|p| p.text().ok().map(|t| text::page_text_in_document_order(&t)))
        .unwrap_or_default();
    Ok(!text.trim().is_empty())
}
