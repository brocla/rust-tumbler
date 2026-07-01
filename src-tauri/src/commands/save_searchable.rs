//! "Save Searchable Copy" — writes a new PDF with an invisible OCR text layer
//! (PDF render mode 3 / "Tr 3") on every previously text-less (scanned) page.
//!
//! Font: Helvetica + WinAnsiEncoding (Option A). Non-WinAnsi characters (CJK,
//! many non-Latin scripts) are silently dropped from the text layer. The layer
//! still provides correct search and copy for Latin/Western-European content.
//! Upgrade to a glyphless CID font (Option B) for full-language support.
//!
//! The source file is never modified — this is "Save As" only.

use crate::commands::ocr::{cache_get, ocr_document_impl, OcrCache, OcrEngine, OcrProgress, OcrWord};
use crate::error::AppError;
use crate::state::{lock_mutex, AppState, DocEntry};
use lopdf::{
    content::{Content, Operation},
    Dictionary, Object, ObjectId, Stream,
};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use tauri::{Emitter, State, WebviewWindow};

/// Name used for the invisible-text font in every modified page's /Resources.
const FONT_NAME: &[u8] = b"TumblerOCR";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveSearchableResult {
    /// Pages that received an OCR text layer.
    pub pages_written: u32,
    pub cancelled: bool,
}

#[tauri::command]
pub async fn save_searchable_copy(
    window: WebviewWindow,
    state: State<'_, AppState>,
    doc_id: String,
    dest_path: String,
) -> Result<SaveSearchableResult, String> {
    let entry = state.get_document(&doc_id).map_err(String::from)?;
    let engine = state.ocr_engine.clone();
    let cache = state.ocr_cache_handle();
    let cancel = Arc::new(AtomicBool::new(false));
    state.set_ocr_job(cancel.clone());

    let file_path = {
        let e = lock_mutex(&entry).map_err(String::from)?;
        e.file_path.clone()
    };

    let emit = move |page, total| {
        let _ = window.emit("ocr-progress", OcrProgress { page, total });
    };

    let result = tauri::async_runtime::spawn_blocking(move || {
        save_searchable_copy_impl(emit, entry, doc_id, file_path, dest_path, engine, cache, cancel)
    })
    .await
    .map_err(|e| e.to_string());

    state.take_ocr_job();
    result?.map_err(String::from)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn save_searchable_copy_impl(
    emit_progress: impl Fn(u32, u32),
    entry: Arc<Mutex<DocEntry>>,
    doc_id: String,
    file_path: String,
    dest_path: String,
    engine: Arc<dyn OcrEngine>,
    cache: OcrCache,
    cancel: Arc<AtomicBool>,
) -> Result<SaveSearchableResult, AppError> {
    // Identify text-less pages using a short-lived pdfium lock so we know
    // exactly which pages the OCR layer should cover.
    let (page_count, textless_pages) = {
        let e = lock_mutex(&entry)?;
        let count = e.document.pages().len() as u32;
        let textless: HashSet<u32> = (1..=count)
            .filter(|&page_num| {
                match e.document.pages().get((page_num - 1) as i32) {
                    Ok(page) => {
                        let native = page.text().map(|t| t.all()).unwrap_or_default();
                        native.trim().is_empty()
                    }
                    Err(_) => true,
                }
            })
            .collect();
        (count, textless)
    };

    // OCR pass: populate the cache for every text-less page.
    // Reuses ocr_document_impl so progress events and cancellation are shared
    // with "Make Searchable".
    let ocr_result = ocr_document_impl(
        emit_progress,
        entry,
        doc_id.clone(),
        engine,
        cache.clone(),
        cancel,
    )?;

    if ocr_result.cancelled {
        return Ok(SaveSearchableResult {
            pages_written: 0,
            cancelled: true,
        });
    }

    // lopdf pass: open the source and write invisible text layers.
    let mut doc = lopdf::Document::load(&file_path)
        .map_err(|e| AppError::lopdf("Failed to open PDF for searchable copy", e))?;

    // One Helvetica Type1 font object shared by all modified pages.
    let font_id = {
        let mut d = Dictionary::new();
        d.set("Type", Object::Name(b"Font".to_vec()));
        d.set("Subtype", Object::Name(b"Type1".to_vec()));
        d.set("BaseFont", Object::Name(b"Helvetica".to_vec()));
        d.set("Encoding", Object::Name(b"WinAnsiEncoding".to_vec()));
        doc.add_object(Object::Dictionary(d))
    };

    let page_ids: BTreeMap<u32, ObjectId> = doc.get_pages();
    let mut pages_written = 0u32;

    for page_num in 1..=page_count {
        if !textless_pages.contains(&page_num) {
            continue;
        }
        let words = match cache_get(&cache, &doc_id, page_num) {
            Some(w) if !w.is_empty() => w,
            _ => continue,
        };
        let Some(&page_id) = page_ids.get(&page_num) else {
            continue;
        };

        let stream_bytes = build_invisible_text_stream(&words, FONT_NAME)?;
        append_content_stream(&mut doc, page_id, stream_bytes)?;
        ensure_font_in_page_resources(&mut doc, page_id, FONT_NAME, font_id)?;
        pages_written += 1;
    }

    // Write via temp + atomic rename so a crash cannot corrupt the destination.
    let tmp_path = format!("{dest_path}.tmp-{}", uuid::Uuid::new_v4());
    if let Err(e) = doc.save(&tmp_path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(AppError::io("Failed to save searchable copy", e));
    }
    std::fs::rename(&tmp_path, &dest_path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        AppError::io("Failed to move searchable copy to destination", e)
    })?;

    Ok(SaveSearchableResult {
        pages_written,
        cancelled: false,
    })
}

/// Builds a PDF content stream with one invisible (Tr 3) text object per word.
/// Public for unit tests.
pub fn build_invisible_text_stream(
    words: &[OcrWord],
    font_name: &[u8],
) -> Result<Vec<u8>, AppError> {
    let mut ops: Vec<Operation> = Vec::new();

    for w in words {
        let encoded = encode_win_ansi(&w.text);
        if encoded.is_empty() {
            continue;
        }

        let font_size = w.rect.height.max(1.0);
        let char_count = encoded.len().max(1);
        // Approximate natural text width: Helvetica averages ~0.5× font-size
        // per glyph at any size. Tz (horizontal scale %) stretches/shrinks the
        // run to fit the OCR box, keeping the text roughly under the image word.
        let natural_width = 0.5 * font_size * char_count as f32;
        let h_scale = ((w.rect.width / natural_width) * 100.0).clamp(10.0, 2000.0);

        ops.push(Operation::new("BT", vec![]));
        ops.push(Operation::new(
            "Tf",
            vec![
                Object::Name(font_name.to_vec()),
                Object::Real(font_size),
            ],
        ));
        ops.push(Operation::new("Tr", vec![Object::Integer(3)]));
        ops.push(Operation::new("Tz", vec![Object::Real(h_scale)]));
        ops.push(Operation::new(
            "Td",
            vec![Object::Real(w.rect.x), Object::Real(w.rect.y)],
        ));
        ops.push(Operation::new("Tj", vec![Object::string_literal(encoded)]));
        ops.push(Operation::new("ET", vec![]));
    }

    Content { operations: ops }
        .encode()
        .map_err(|e| AppError::Other(format!("Failed to encode content stream: {e}")))
}

/// Appends a new content stream to a page, handling `/Contents` as a single
/// Reference, an Array, or absent.
fn append_content_stream(
    doc: &mut lopdf::Document,
    page_id: ObjectId,
    stream_bytes: Vec<u8>,
) -> Result<(), AppError> {
    let stream_id =
        doc.add_object(Object::Stream(Stream::new(Dictionary::new(), stream_bytes)));

    let existing = doc
        .objects
        .get(&page_id)
        .and_then(|o| o.as_dict().ok())
        .and_then(|d| d.get(b"Contents").ok())
        .cloned();

    let new_contents = match existing {
        Some(Object::Reference(r)) => {
            Object::Array(vec![Object::Reference(r), Object::Reference(stream_id)])
        }
        Some(Object::Array(mut a)) => {
            a.push(Object::Reference(stream_id));
            Object::Array(a)
        }
        _ => Object::Reference(stream_id),
    };

    if let Some(Object::Dictionary(d)) = doc.objects.get_mut(&page_id) {
        d.set(b"Contents", new_contents);
    }
    Ok(())
}

/// Ensures `/Resources /Font /TumblerOCR` points at `font_id` for the page.
/// Preserves any existing font entries. Writes an inline Resources dict back
/// onto the page dict (this always wins for that page, even when Resources is
/// otherwise inherited from the parent Pages node).
fn ensure_font_in_page_resources(
    doc: &mut lopdf::Document,
    page_id: ObjectId,
    font_name: &[u8],
    font_id: ObjectId,
) -> Result<(), AppError> {
    let page_dict = doc
        .objects
        .get(&page_id)
        .and_then(|o| o.as_dict().ok())
        .cloned()
        .ok_or_else(|| AppError::Other("Page object not found in lopdf document".to_string()))?;

    let mut resources = match page_dict.get(b"Resources").ok() {
        Some(Object::Reference(id)) => doc
            .objects
            .get(id)
            .and_then(|o| o.as_dict().ok())
            .cloned()
            .unwrap_or_default(),
        Some(Object::Dictionary(d)) => d.clone(),
        _ => Dictionary::new(),
    };

    let mut fonts = match resources.get(b"Font").ok() {
        Some(Object::Reference(id)) => doc
            .objects
            .get(id)
            .and_then(|o| o.as_dict().ok())
            .cloned()
            .unwrap_or_default(),
        Some(Object::Dictionary(d)) => d.clone(),
        _ => Dictionary::new(),
    };

    fonts.set(font_name, Object::Reference(font_id));
    resources.set(b"Font", Object::Dictionary(fonts));

    if let Some(Object::Dictionary(d)) = doc.objects.get_mut(&page_id) {
        d.set(b"Resources", Object::Dictionary(resources));
    }
    Ok(())
}

/// Maps a Unicode codepoint to its WinAnsi (cp1252) byte. Characters outside
/// WinAnsi are silently dropped — they cannot be represented in a Helvetica
/// Type1 font with WinAnsiEncoding.
fn char_to_win_ansi(c: char) -> Option<u8> {
    let cp = c as u32;
    match cp {
        0x20..=0x7E => Some(cp as u8), // ASCII printable
        0xA0..=0xFF => Some(cp as u8), // Latin-1 supplement (identical to cp1252)
        // Windows-1252 extensions (0x80–0x9F):
        0x20AC => Some(0x80), // €
        0x201A => Some(0x82), // ‚
        0x0192 => Some(0x83), // ƒ
        0x201E => Some(0x84), // „
        0x2026 => Some(0x85), // …
        0x2020 => Some(0x86), // †
        0x2021 => Some(0x87), // ‡
        0x02C6 => Some(0x88), // ˆ
        0x2030 => Some(0x89), // ‰
        0x0160 => Some(0x8A), // Š
        0x2039 => Some(0x8B), // ‹
        0x0152 => Some(0x8C), // Œ
        0x017D => Some(0x8E), // Ž
        0x2018 => Some(0x91), // '
        0x2019 => Some(0x92), // '
        0x201C => Some(0x93), // "
        0x201D => Some(0x94), // "
        0x2022 => Some(0x95), // •
        0x2013 => Some(0x96), // –
        0x2014 => Some(0x97), // —
        0x02DC => Some(0x98), // ˜
        0x2122 => Some(0x99), // ™
        0x0161 => Some(0x9A), // š
        0x203A => Some(0x9B), // ›
        0x0153 => Some(0x9C), // œ
        0x017E => Some(0x9E), // ž
        0x0178 => Some(0x9F), // Ÿ
        _ => None,
    }
}

fn encode_win_ansi(text: &str) -> Vec<u8> {
    text.chars().filter_map(char_to_win_ansi).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::ocr::OcrWord;
    use crate::commands::text::TextRect;
    use crate::state::DocEntry;
    use pdfium_render::prelude::*;

    fn word_pt(text: &str, x: f32, y: f32) -> OcrWord {
        OcrWord {
            text: text.to_string(),
            rect: TextRect {
                x,
                y,
                width: 50.0,
                height: 12.0,
            },
        }
    }

    // ── Pure content-stream builder ─────────────────────────────────────────

    #[test]
    fn builds_invisible_text_ops() {
        let words = vec![word_pt("Hello", 10.0, 100.0)];
        let bytes = build_invisible_text_stream(&words, FONT_NAME).expect("encode");
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("3 Tr"), "missing invisible render mode: {s}");
        assert!(s.contains("BT") && s.contains("ET"), "missing BT/ET: {s}");
        assert!(s.contains("Hello"), "missing word text: {s}");
    }

    #[test]
    fn empty_word_list_produces_empty_stream() {
        let bytes = build_invisible_text_stream(&[], FONT_NAME).expect("encode");
        // Should be a valid (possibly empty) stream with no BT blocks.
        let s = String::from_utf8_lossy(&bytes);
        assert!(!s.contains("BT"), "unexpected BT in empty stream: {s}");
    }

    #[test]
    fn non_win_ansi_chars_are_dropped() {
        // CJK characters have no WinAnsi representation.
        assert_eq!(encode_win_ansi("日本語"), Vec::<u8>::new());
        // ASCII is preserved.
        assert_eq!(encode_win_ansi("ABC"), b"ABC");
    }

    #[test]
    fn non_win_ansi_word_is_skipped_in_stream() {
        let words = vec![word_pt("日本語", 0.0, 0.0)];
        let bytes = build_invisible_text_stream(&words, FONT_NAME).expect("encode");
        let s = String::from_utf8_lossy(&bytes);
        assert!(!s.contains("BT"), "CJK word should produce no BT block: {s}");
    }

    // ── Round-trip: pdfium reads back the OCR words as native text ──────────

    fn open_fixture_copy(state: &AppState, doc_id: &str) -> String {
        let src = crate::fixture_path();
        let tmp = std::env::temp_dir().join(format!("tumbler_ssc_{doc_id}.pdf"));
        std::fs::copy(&src, &tmp).expect("copy fixture");
        let pdfium = crate::test_pdfium();
        let document = pdfium
            .load_pdf_from_file(tmp.to_str().unwrap(), None)
            .expect("load pdf");
        state
            .insert_document(
                doc_id.to_string(),
                DocEntry {
                    document,
                    file_path: tmp.to_string_lossy().into_owned(),
                },
            )
            .expect("insert");
        tmp.to_string_lossy().into_owned()
    }

    fn no_cancel() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    fn no_progress(_p: u32, _t: u32) {}

    struct FakeOcrEngine {
        words: Vec<OcrWord>,
    }
    impl OcrEngine for FakeOcrEngine {
        fn recognize(&self, _rgba: &[u8], _w: u32, _h: u32) -> Result<Vec<OcrWord>, AppError> {
            Ok(self.words.clone())
        }
    }

    fn fake_engine(words: Vec<OcrWord>) -> Arc<dyn OcrEngine> {
        Arc::new(FakeOcrEngine { words })
    }

    /// The key round-trip test: blank page → OCR words seeded → save copy →
    /// reload with pdfium → pdfium's native text API returns the OCR words.
    #[test]
    fn saved_copy_is_natively_searchable() {
        let _g = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let engine = fake_engine(vec![word_pt("Scanned", 10.0, 150.0)]);
        let state = AppState::new(pdfium, None).with_ocr_engine(engine);

        // Write a blank one-page PDF to disk via pdfium so lopdf can reload it.
        let blank_pdf = std::env::temp_dir().join("tumbler_blank_for_ssc.pdf");
        {
            let mut doc = pdfium.create_new_pdf().expect("create pdf");
            doc.pages_mut()
                .create_page_at_index(
                    PdfPagePaperSize::new_custom(PdfPoints::new(200.0), PdfPoints::new(200.0)),
                    0 as PdfPageIndex,
                )
                .expect("create blank page");
            doc.save_to_file(blank_pdf.to_str().unwrap())
                .expect("save blank pdf");
        }

        // Load the on-disk file into AppState with the correct path.
        let document = pdfium
            .load_pdf_from_file(blank_pdf.to_str().unwrap(), None)
            .expect("load blank pdf");
        state
            .insert_document(
                "blank".to_string(),
                DocEntry {
                    document,
                    file_path: blank_pdf.to_string_lossy().into_owned(),
                },
            )
            .expect("insert blank");

        // Seed the OCR cache manually (simulates a prior "Make Searchable").
        state.set_ocr_words("blank", 1, vec![word_pt("Scanned", 10.0, 150.0)]);

        let dest = std::env::temp_dir().join("tumbler_ssc_searchable.pdf");
        let entry = state.get_document("blank").expect("get entry");

        save_searchable_copy_impl(
            no_progress,
            entry,
            "blank".to_string(),
            blank_pdf.to_string_lossy().into_owned(),
            dest.to_string_lossy().into_owned(),
            state.ocr_engine.clone(),
            state.ocr_cache_handle(),
            no_cancel(),
        )
        .expect("save searchable copy");

        // Reopen with pdfium and check native text.
        let reloaded = pdfium
            .load_pdf_from_file(dest.to_str().unwrap(), None)
            .expect("reload searchable copy");
        let page = reloaded.pages().get(0).expect("page 0");
        let text = page.text().expect("text").all();
        assert!(
            text.contains("Scanned"),
            "expected pdfium native text to contain 'Scanned', got: {text:?}"
        );

        std::fs::remove_file(&blank_pdf).ok();
        std::fs::remove_file(&dest).ok();
    }

    /// Pages that already have a native text layer must not get a duplicate OCR
    /// layer (the fixture's "Test Fixture" page must be untouched).
    #[test]
    fn pages_with_native_text_are_not_modified() {
        let _g = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        let src = open_fixture_copy(&state, "text");

        let dest = std::env::temp_dir().join("tumbler_ssc_native_check.pdf");
        let entry = state.get_document("text").expect("get entry");

        let result = save_searchable_copy_impl(
            no_progress,
            entry,
            "text".to_string(),
            src.clone(),
            dest.to_string_lossy().into_owned(),
            state.ocr_engine.clone(),
            state.ocr_cache_handle(),
            no_cancel(),
        )
        .expect("save searchable copy");

        assert_eq!(result.pages_written, 0, "native-text page must not get a layer");

        std::fs::remove_file(&dest).ok();
        std::fs::remove_file(&src).ok();
    }

    /// The source file must be byte-for-byte identical before and after.
    #[test]
    fn source_file_is_unchanged() {
        let _g = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        let src = open_fixture_copy(&state, "src_unchanged");

        let before = std::fs::read(&src).expect("read before");
        let dest = std::env::temp_dir().join("tumbler_ssc_unchanged_dest.pdf");

        let entry = state.get_document("src_unchanged").expect("get entry");
        save_searchable_copy_impl(
            no_progress,
            entry,
            "src_unchanged".to_string(),
            src.clone(),
            dest.to_string_lossy().into_owned(),
            state.ocr_engine.clone(),
            state.ocr_cache_handle(),
            no_cancel(),
        )
        .expect("save searchable copy");

        let after = std::fs::read(&src).expect("read after");
        assert_eq!(before, after, "source file was modified");

        std::fs::remove_file(&dest).ok();
        std::fs::remove_file(&src).ok();
    }

    /// Cancellation before the OCR pass returns cancelled=true and writes nothing.
    #[test]
    fn cancellation_returns_early() {
        let _g = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        let src = open_fixture_copy(&state, "cancel_src");

        let dest = std::env::temp_dir().join("tumbler_ssc_cancel_dest.pdf");
        std::fs::remove_file(&dest).ok();

        let entry = state.get_document("cancel_src").expect("get entry");
        let cancel = Arc::new(AtomicBool::new(true)); // pre-cancelled

        let result = save_searchable_copy_impl(
            no_progress,
            entry,
            "cancel_src".to_string(),
            src.clone(),
            dest.to_string_lossy().into_owned(),
            state.ocr_engine.clone(),
            state.ocr_cache_handle(),
            cancel,
        )
        .expect("impl should not error on cancel");

        assert!(result.cancelled, "expected cancelled=true");
        assert!(
            !std::path::Path::new(&dest).exists(),
            "cancelled run must not write dest file"
        );

        std::fs::remove_file(&src).ok();
    }
}
