//! "Save Searchable Copy" — the persisted OCR-layer tier (issue #4).
//!
//! Where "Make Searchable" (see [`crate::commands::ocr`]) recognizes text into a
//! session-only cache, this writes those words into a **new** PDF as an
//! invisible text layer (the "OCR sandwich"): for every previously text-less
//! (scanned) page, the recognized words are grouped into lines (reusing the
//! ephemeral overlay's `ocr_words_to_lines`) and each line is appended to the
//! page's content stream as one run in **text render mode 3 (invisible)** — so
//! a reader's selection/search highlight stays smooth across the line, matching
//! "Make Searchable". The bytes are never painted —
//! they exist purely so the file is searchable, selectable, and copyable in any
//! PDF reader. Afterward Tumbler's own `search_document` / `extract_page_text`
//! need no special-casing: pdfium just sees real text operators.
//!
//! This is always "Save As": the source file is never modified. The write goes
//! to a temp file in the destination directory, then an atomic rename.
//!
//! Coordinate note: `OcrWord.rect` is already in PDF user space (points, origin
//! bottom-left) for the common case of a MediaBox at `[0 0 w h]` with no
//! `/Rotate`. Pages with a shifted origin or a rotation are *detected and
//! skipped* in this first cut rather than mis-positioned (see
//! [`geometry_is_simple`]); the happy path is authored correctly.

use crate::commands::ocr::{
    cache_get, ocr_page_into_cache, ocr_words_to_lines, OcrCache, OcrEngine, OcrProgress, OcrWord,
};
use crate::error::AppError;
use crate::state::{lock_mutex, AppState, DocEntry};
use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Dictionary, Document, Object, ObjectId, Stream, StringFormat};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, State, WebviewWindow};

/// Resource name for the invisible-text font. Prefixed to avoid colliding with
/// any font the page already defines.
const FONT_NAME: &str = "TumblerOCR";

/// Loose-bounds metrics that pdfium reports for the non-embedded standard
/// Helvetica we use, as fractions of the font size: `ASCENT` is how far the
/// text-extraction box rises above the baseline, `DESCENT` how far it drops
/// below. We size and place each line from these so its extraction box (and
/// thus the selection/search highlight, which is derived from it) coincides
/// with the OCR box — landing on the scanned text instead of a fraction of a
/// line too low. Pinned empirically by `layer_box_matches_ocr_box`.
const HELVETICA_ASCENT_RATIO: f32 = 0.905;
const HELVETICA_DESCENT_RATIO: f32 = 0.211;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveSearchableResult {
    /// Pages that received an invisible OCR text layer.
    pub pages_written: u32,
    pub cancelled: bool,
}

// ── Content-stream authoring (pure) ─────────────────────────────────────────

/// Horizontal scaling percent (`Tz`) that stretches the (invisible) glyphs to
/// roughly fill the OCR box width. Without real glyph-advance tables we
/// approximate a word's natural width as `0.5 * font_size * char_count`, which
/// is close enough — `Tz` only affects how tightly a reader's selection
/// highlight hugs the word, never search or copy correctness.
fn horizontal_scale_percent(text: &str, font_size: f32, box_width: f32) -> f32 {
    let char_count = text.chars().count().max(1) as f32;
    let natural_width = 0.5 * font_size * char_count;
    if natural_width <= 0.0 || box_width <= 0.0 {
        return 100.0;
    }
    (box_width / natural_width * 100.0).clamp(1.0, 1000.0)
}

/// Maps a Unicode scalar to its WinAnsi (cp1252) byte, or `None` if it can't be
/// represented. ASCII and Latin-1 map directly; the cp1252-only punctuation in
/// `0x80..=0x9F` (smart quotes, dashes, ellipsis, …) is mapped explicitly.
/// Characters outside WinAnsi (CJK, most non-Latin scripts) are dropped — the
/// documented limitation of the standard-font (Option A) approach.
fn win_ansi_byte(c: char) -> Option<u8> {
    let cp = c as u32;
    match cp {
        0x20..=0x7E | 0xA0..=0xFF => Some(cp as u8),
        0x20AC => Some(0x80),
        0x201A => Some(0x82),
        0x0192 => Some(0x83),
        0x201E => Some(0x84),
        0x2026 => Some(0x85),
        0x2020 => Some(0x86),
        0x2021 => Some(0x87),
        0x02C6 => Some(0x88),
        0x2030 => Some(0x89),
        0x0160 => Some(0x8A),
        0x2039 => Some(0x8B),
        0x0152 => Some(0x8C),
        0x017D => Some(0x8E),
        0x2018 => Some(0x91),
        0x2019 => Some(0x92),
        0x201C => Some(0x93),
        0x201D => Some(0x94),
        0x2022 => Some(0x95),
        0x2013 => Some(0x96),
        0x2014 => Some(0x97),
        0x02DC => Some(0x98),
        0x2122 => Some(0x99),
        0x0161 => Some(0x9A),
        0x203A => Some(0x9B),
        0x0153 => Some(0x9C),
        0x017E => Some(0x9E),
        0x0178 => Some(0x9F),
        _ => None,
    }
}

/// Encodes text for the standard WinAnsi font, dropping unrepresentable chars.
fn encode_for_font(text: &str) -> Vec<u8> {
    text.chars().filter_map(win_ansi_byte).collect()
}

/// Builds the invisible-text content stream for one page's worth of OCR words.
///
/// Words are grouped into visual **lines** with the same [`ocr_words_to_lines`]
/// pass the ephemeral "Make Searchable" overlay uses, and each line is written
/// as **one continuous run** — a single `BT … ET` block in render mode 3, with
/// one font size and one horizontal-scale (`Tz`) stretching the whole line to
/// its box width. Emitting per line (not per word) is what keeps a reader's
/// selection and search highlight smooth across the line, with uniform spacing,
/// instead of jumping between independently-scaled per-word runs. Each block is
/// self-contained so a malformed prior stream can't bleed text state into ours.
///
/// Font size and baseline are derived from the line box and Helvetica's loose
/// metrics so the run's text-extraction box coincides with the OCR box: with
/// `fs = height / (ascent + descent)` the box height matches, and placing the
/// baseline at `box_bottom + descent·fs` makes the box bottom sit on the OCR
/// box bottom (the descent hangs down to exactly the box bottom, not below it).
pub fn build_invisible_text_stream(words: &[OcrWord], font_name: &str) -> Vec<u8> {
    let mut ops: Vec<Operation> = Vec::new();
    for line in ocr_words_to_lines(words) {
        let encoded = encode_for_font(&line.text);
        if encoded.is_empty() {
            continue; // nothing representable (e.g. a pure-CJK line)
        }
        let box_height = line.rect.height.max(1.0);
        let font_size = box_height / (HELVETICA_ASCENT_RATIO + HELVETICA_DESCENT_RATIO);
        let baseline_y = line.rect.y + HELVETICA_DESCENT_RATIO * font_size;
        let h_scale = horizontal_scale_percent(&line.text, font_size, line.rect.width);

        ops.push(Operation::new("BT", vec![]));
        ops.push(Operation::new(
            "Tf",
            vec![Object::Name(font_name.as_bytes().to_vec()), Object::Real(font_size)],
        ));
        ops.push(Operation::new("Tr", vec![Object::Integer(3)])); // invisible
        ops.push(Operation::new("Tz", vec![Object::Real(h_scale)]));
        ops.push(Operation::new(
            "Td",
            vec![Object::Real(line.rect.x), Object::Real(baseline_y)],
        ));
        ops.push(Operation::new(
            "Tj",
            vec![Object::String(encoded, StringFormat::Literal)],
        ));
        ops.push(Operation::new("ET", vec![]));
    }
    Content { operations: ops }.encode().unwrap_or_default()
}

// ── Page geometry ───────────────────────────────────────────────────────────

/// Whether a page's coordinate space matches the one `OcrWord.rect` assumes:
/// MediaBox origin at (0,0) and no rotation. Rotated/offset pages are skipped
/// in this cut so their layer is never mis-placed.
fn geometry_is_simple(origin_x: f32, origin_y: f32, rotate: i64) -> bool {
    origin_x.abs() < 0.5 && origin_y.abs() < 0.5 && rotate.rem_euclid(360) == 0
}

/// Resolves a possibly-inherited page attribute, following `/Parent` up the page
/// tree and dereferencing an indirect value. Returns an owned clone.
fn inherited_value(doc: &Document, page_id: ObjectId, key: &[u8]) -> Option<Object> {
    let mut current = page_id;
    for _ in 0..64 {
        let dict = doc.get_object(current).ok()?.as_dict().ok()?;
        if let Ok(value) = dict.get(key) {
            return Some(match value {
                Object::Reference(r) => doc.get_object(*r).ok()?.clone(),
                other => other.clone(),
            });
        }
        current = dict.get(b"Parent").ok()?.as_reference().ok()?;
    }
    None
}

fn object_as_f32(obj: &Object) -> f32 {
    match obj {
        Object::Integer(i) => *i as f32,
        Object::Real(r) => *r,
        _ => 0.0,
    }
}

/// Reads a page's effective (MediaBox origin, /Rotate) for the simple-geometry
/// check. Missing values default to origin (0,0) and rotation 0.
fn page_geometry(doc: &Document, page_id: ObjectId) -> (f32, f32, i64) {
    let (origin_x, origin_y) = match inherited_value(doc, page_id, b"MediaBox") {
        Some(Object::Array(a)) if a.len() >= 2 => (object_as_f32(&a[0]), object_as_f32(&a[1])),
        _ => (0.0, 0.0),
    };
    let rotate = match inherited_value(doc, page_id, b"Rotate") {
        Some(Object::Integer(i)) => i,
        _ => 0,
    };
    (origin_x, origin_y, rotate)
}

/// Builds the page's Resources dictionary with our OCR font added, preserving
/// every existing resource (images, other fonts) whether the page owns its
/// `/Resources` or inherits them. Returned owned so it can be set after the
/// mutable page borrow begins.
fn merged_resources_with_font(
    doc: &Document,
    page_id: ObjectId,
    font_name: &str,
    font_id: ObjectId,
) -> Dictionary {
    let mut resources = match inherited_value(doc, page_id, b"Resources") {
        Some(Object::Dictionary(d)) => d,
        _ => Dictionary::new(),
    };
    let mut fonts = match resources.get(b"Font") {
        Ok(Object::Dictionary(d)) => d.clone(),
        Ok(Object::Reference(r)) => doc
            .get_object(*r)
            .ok()
            .and_then(|o| o.as_dict().ok())
            .cloned()
            .unwrap_or_default(),
        _ => Dictionary::new(),
    };
    fonts.set(font_name, Object::Reference(font_id));
    resources.set("Font", Object::Dictionary(fonts));
    resources
}

/// Appends `stream_id` to a page's `/Contents`, whatever its current shape.
/// PDF concatenates the array, so our invisible text is drawn last (over the
/// page image).
fn append_content_stream(page: &mut Dictionary, stream_id: ObjectId) {
    match page.get(b"Contents") {
        Ok(Object::Reference(existing)) => {
            let existing = *existing;
            page.set(
                "Contents",
                Object::Array(vec![Object::Reference(existing), Object::Reference(stream_id)]),
            );
        }
        Ok(Object::Array(a)) => {
            let mut a = a.clone();
            a.push(Object::Reference(stream_id));
            page.set("Contents", Object::Array(a));
        }
        _ => {
            page.set("Contents", Object::Reference(stream_id));
        }
    }
}

// ── Command ─────────────────────────────────────────────────────────────────

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

    // Same shared `ocr-progress` channel the progress overlay already listens on.
    let emit = move |page, total| {
        let _ = window.emit("ocr-progress", OcrProgress { page, total });
    };

    let result = tauri::async_runtime::spawn_blocking(move || {
        save_searchable_copy_impl(emit, entry, doc_id, dest_path, engine, cache, cancel)
    })
    .await
    .map_err(|e| e.to_string());

    state.take_ocr_job();
    result?.map_err(String::from)
}

#[allow(clippy::too_many_arguments)]
fn save_searchable_copy_impl(
    emit_progress: impl Fn(u32, u32),
    entry: Arc<Mutex<DocEntry>>,
    doc_id: String,
    dest_path: String,
    engine: Arc<dyn OcrEngine>,
    cache: OcrCache,
    cancel: Arc<AtomicBool>,
) -> Result<SaveSearchableResult, AppError> {
    let (file_path, page_count) = {
        let entry = lock_mutex(&entry)?;
        (entry.file_path.clone(), entry.document.pages().len() as u32)
    };

    // Phase A (pdfium): ensure every text-less page is OCR'd into the cache, and
    // remember which pages are text-less — those are the only ones that get a
    // layer (native-text pages are already searchable; adding text would
    // duplicate and confuse selection).
    let mut textless_pages: Vec<u32> = Vec::new();
    for i in 0..page_count {
        let page_num = i + 1;

        if cancel.load(Ordering::Relaxed) {
            return Ok(SaveSearchableResult {
                pages_written: 0,
                cancelled: true,
            });
        }
        emit_progress(page_num, page_count);

        let native_empty = {
            let entry = lock_mutex(&entry)?;
            let page = entry
                .document
                .pages()
                .get(i as i32)
                .map_err(|e| AppError::pdfium(format!("Failed to get page {page_num}"), e))?;
            page.text()
                .map(|t| t.all())
                .unwrap_or_default()
                .trim()
                .is_empty()
        };
        if native_empty {
            ocr_page_into_cache(&entry, &doc_id, page_num, &engine, &cache)?;
            textless_pages.push(page_num);
        }
    }

    // Phase B (lopdf): author the invisible text into a fresh copy read from
    // disk. pdfium's handle is never used for writing.
    let mut doc = Document::load(&file_path)
        .map_err(|e| AppError::lopdf("Failed to open PDF for searchable copy", e))?;
    let pages = doc.get_pages();

    // Add the shared font object lazily — only if at least one page needs it, so
    // a document with nothing to OCR is written back untouched.
    let mut font_id: Option<ObjectId> = None;
    let mut pages_written = 0u32;

    for page_num in textless_pages {
        let Some(words) = cache_get(&cache, &doc_id, page_num) else {
            continue;
        };
        let Some(&page_id) = pages.get(&page_num) else {
            continue;
        };

        // Skip pages whose coordinate space doesn't match what OcrWord.rect
        // assumes; better no layer than a mis-placed one.
        let (ox, oy, rotate) = page_geometry(&doc, page_id);
        if !geometry_is_simple(ox, oy, rotate) {
            continue;
        }

        let stream_bytes = build_invisible_text_stream(&words, FONT_NAME);
        if stream_bytes.is_empty() {
            continue; // no representable text for this page
        }

        let fid = *font_id.get_or_insert_with(|| {
            doc.add_object(dictionary! {
                "Type" => "Font",
                "Subtype" => "Type1",
                "BaseFont" => "Helvetica",
                "Encoding" => "WinAnsiEncoding",
            })
        });
        let resources = merged_resources_with_font(&doc, page_id, FONT_NAME, fid);
        let stream_id = doc.add_object(Object::Stream(Stream::new(Dictionary::new(), stream_bytes)));

        {
            let page = doc
                .get_object_mut(page_id)
                .map_err(|e| AppError::lopdf(format!("Failed to get page {page_num}"), e))?
                .as_dict_mut()
                .map_err(|e| AppError::lopdf(format!("Page {page_num} is not a dictionary"), e))?;
            append_content_stream(page, stream_id);
            page.set("Resources", Object::Dictionary(resources));
        }
        pages_written += 1;
    }

    // Write to a temp file in the destination dir, then atomic rename, so a
    // crash or disk-full can't leave a truncated file at dest_path.
    let tmp_path = format!("{dest_path}.tmp-{}", uuid::Uuid::new_v4());
    if let Err(e) = doc.save(&tmp_path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(AppError::io("Failed to save searchable copy", e));
    }
    std::fs::rename(&tmp_path, &dest_path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        AppError::io("Failed to move searchable copy into place", e)
    })?;

    Ok(SaveSearchableResult {
        pages_written,
        cancelled: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::text::TextRect;
    use crate::state::DocEntry;
    use std::sync::atomic::AtomicBool;

    /// OCR engine that returns fixed pixel-space words (mapped to PDF points by
    /// `ocr_page_into_cache`, exactly like the real engine).
    struct FakeOcrEngine {
        words: Vec<OcrWord>,
    }
    impl OcrEngine for FakeOcrEngine {
        fn recognize(&self, _rgba: &[u8], _w: u32, _h: u32) -> Result<Vec<OcrWord>, AppError> {
            Ok(self.words.clone())
        }
    }

    /// A word already in PDF user space (origin bottom-left), as the cache holds.
    fn pt_word(text: &str, x: f32, y: f32, w: f32, h: f32) -> OcrWord {
        OcrWord {
            text: text.to_string(),
            rect: TextRect { x, y, width: w, height: h },
        }
    }

    /// A pixel-space word as an OCR engine reports it (top-left origin).
    fn px_word(text: &str) -> OcrWord {
        OcrWord {
            text: text.to_string(),
            rect: TextRect { x: 40.0, y: 40.0, width: 120.0, height: 40.0 },
        }
    }

    fn temp_path(name: &str) -> String {
        std::env::temp_dir()
            .join(format!("{}-{}", uuid::Uuid::new_v4(), name))
            .to_string_lossy()
            .into_owned()
    }

    /// Hand-writes a minimal one-page blank PDF (no text layer) to `path`, so
    /// both pdfium and lopdf can load it — a stand-in for a scanned page.
    fn write_blank_pdf(path: &str) {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let contents_id = doc.add_object(Stream::new(Dictionary::new(), Vec::new()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => contents_id,
            "MediaBox" => vec![
                Object::Integer(0), Object::Integer(0),
                Object::Integer(200), Object::Integer(200),
            ],
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => Object::Integer(1),
            }),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", catalog_id);
        doc.save(path).expect("write blank pdf");
    }

    // ── Pure builder / helpers ──────────────────────────────────────────────

    #[test]
    fn builds_invisible_text_ops() {
        let bytes = build_invisible_text_stream(&[pt_word("Hello", 10.0, 100.0, 60.0, 12.0)], FONT_NAME);
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("3 Tr"), "missing invisible render mode: {s}");
        assert!(s.contains("BT") && s.contains("ET"), "missing BT/ET: {s}");
        assert!(s.contains("Hello"), "missing word text: {s}");
        assert!(s.contains("/TumblerOCR"), "missing font ref: {s}");
    }

    #[test]
    fn empty_words_produce_empty_stream() {
        assert!(build_invisible_text_stream(&[], FONT_NAME).is_empty());
    }

    /// Words sharing a baseline become a single continuous line run (one BT…ET,
    /// text joined with spaces), not one run per word — this is what preserves
    /// the smooth, uniform highlighting of "Make Searchable".
    #[test]
    fn words_on_one_line_form_a_single_run() {
        let words = vec![
            pt_word("Hello", 10.0, 100.0, 30.0, 12.0),
            pt_word("World", 50.0, 100.0, 30.0, 12.0),
        ];
        let bytes = build_invisible_text_stream(&words, FONT_NAME);
        let content = Content::decode(&bytes).expect("decode content");
        let runs = content
            .operations
            .iter()
            .filter(|op| op.operator == "BT")
            .count();
        assert_eq!(runs, 1, "two words on one line should be one run, got {runs}");
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("Hello World"), "line text should be joined: {s}");
    }

    #[test]
    fn unrepresentable_only_word_is_skipped() {
        // A pure-CJK token has no WinAnsi bytes, so it contributes nothing.
        let bytes = build_invisible_text_stream(&[pt_word("日本語", 0.0, 0.0, 30.0, 10.0)], FONT_NAME);
        assert!(bytes.is_empty(), "CJK-only word should be dropped under WinAnsi");
    }

    #[test]
    fn geometry_simple_only_for_unrotated_origin_zero() {
        assert!(geometry_is_simple(0.0, 0.0, 0));
        assert!(geometry_is_simple(0.0, 0.0, 360));
        assert!(!geometry_is_simple(10.0, 0.0, 0), "offset origin is not simple");
        assert!(!geometry_is_simple(0.0, 0.0, 90), "rotation is not simple");
        assert!(!geometry_is_simple(0.0, 0.0, 270));
    }

    // ── The decisive round-trip: searchable in any reader ───────────────────

    /// A blank (scanned-style) page + cached OCR words → save copy → reopen the
    /// output with pdfium → pdfium's **native** text API returns the words. This
    /// proves the layer is real text operators, not a Tumbler-only overlay.
    #[test]
    fn saved_copy_is_natively_searchable() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();

        let src = temp_path("src.pdf");
        write_blank_pdf(&src);
        let dest = temp_path("out.pdf");

        let engine: Arc<dyn OcrEngine> = Arc::new(FakeOcrEngine { words: vec![px_word("Scanned")] });
        let state = AppState::new(pdfium, None).with_ocr_engine(engine.clone());

        let document = pdfium.load_pdf_from_file(&src, None).expect("load blank");
        state
            .insert_document(
                "doc1".to_string(),
                DocEntry { document, file_path: src.clone() },
            )
            .expect("insert");

        let entry = state.get_document("doc1").expect("get");
        let result = save_searchable_copy_impl(
            |_, _| {},
            entry,
            "doc1".to_string(),
            dest.clone(),
            engine,
            state.ocr_cache_handle(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("save searchable");

        assert_eq!(result.pages_written, 1);
        assert!(!result.cancelled);

        // Reopen the saved copy and read text through pdfium's native API.
        let reopened = pdfium.load_pdf_from_file(&dest, None).expect("reopen copy");
        let text = reopened
            .pages()
            .get(0)
            .expect("page 0")
            .text()
            .expect("text")
            .all();
        assert!(
            text.contains("Scanned"),
            "native pdfium text should contain the OCR word, got: {text:?}"
        );

        drop(reopened);
        std::fs::remove_file(&src).ok();
        std::fs::remove_file(&dest).ok();
    }

    /// The saved layer's text-extraction (loose) box — which drives the
    /// selection/search highlight — must coincide with the OCR box, so the
    /// highlight sits on the scanned text rather than a fraction of a line low.
    /// Seeded rect: bottom y=100, height=20 → OCR box spans y 100..120.
    #[test]
    fn layer_box_matches_ocr_box() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();

        let src = temp_path("src.pdf");
        write_blank_pdf(&src);
        let dest = temp_path("out.pdf");

        let word = OcrWord {
            text: "Scanned".to_string(),
            rect: TextRect { x: 30.0, y: 100.0, width: 120.0, height: 20.0 },
        };
        let engine: Arc<dyn OcrEngine> = Arc::new(FakeOcrEngine { words: vec![word.clone()] });
        let state = AppState::new(pdfium, None).with_ocr_engine(engine.clone());
        // Seed the cache directly so the rect is exactly the one above (no
        // pixel→point mapping in the way).
        state.set_ocr_words("doc1", 1, vec![word]);
        let document = pdfium.load_pdf_from_file(&src, None).expect("load blank");
        state
            .insert_document("doc1".to_string(), DocEntry { document, file_path: src.clone() })
            .expect("insert");

        let entry = state.get_document("doc1").expect("get");
        save_searchable_copy_impl(
            |_, _| {},
            entry,
            "doc1".to_string(),
            dest.clone(),
            engine,
            state.ocr_cache_handle(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("save");

        // Union the loose bounds of every character in the run.
        let reopened = pdfium.load_pdf_from_file(&dest, None).expect("reopen");
        let page = reopened.pages().get(0).expect("page");
        let text = page.text().expect("text");
        let mut bottom = f32::INFINITY;
        let mut top = f32::NEG_INFINITY;
        let mut fonts = std::collections::HashSet::new();
        for ch in text.chars().iter() {
            fonts.insert(ch.font_name());
            if let Ok(b) = ch.loose_bounds() {
                bottom = bottom.min(b.bottom().value);
                top = top.max(b.top().value);
            }
        }

        // Font is exactly the standard Helvetica we declared (metrics assumed).
        assert!(fonts.contains("Helvetica"), "unexpected font(s): {fonts:?}");
        // Highlight box aligns with the OCR box within a fraction of a point.
        assert!(
            (bottom - 100.0).abs() < 1.5,
            "layer bottom {bottom} should align to OCR box bottom 100"
        );
        assert!(
            (top - 120.0).abs() < 1.5,
            "layer top {top} should align to OCR box top 120"
        );

        drop(reopened);
        std::fs::remove_file(&src).ok();
        std::fs::remove_file(&dest).ok();
    }

    /// A page with a native text layer must not receive a duplicate OCR layer.
    /// The fixture's single page has real text ("Test Fixture"), so no page is
    /// text-less and nothing is written.
    #[test]
    fn pages_with_native_text_are_not_modified() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);

        let src = crate::fixture_path();
        let document = pdfium
            .load_pdf_from_file(src.to_str().unwrap(), None)
            .expect("load fixture");
        state
            .insert_document(
                "doc1".to_string(),
                DocEntry {
                    document,
                    file_path: src.to_string_lossy().into_owned(),
                },
            )
            .expect("insert");

        let dest = temp_path("native.pdf");
        let entry = state.get_document("doc1").expect("get");
        let result = save_searchable_copy_impl(
            |_, _| {},
            entry,
            "doc1".to_string(),
            dest.clone(),
            state.ocr_engine.clone(),
            state.ocr_cache_handle(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("save searchable");

        assert_eq!(result.pages_written, 0, "native-text page must get no layer");
        std::fs::remove_file(&dest).ok();
    }

    /// "Save As" only: the source file's bytes are untouched even though the
    /// destination gains a text layer.
    #[test]
    fn source_file_is_unchanged() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();

        let src = temp_path("src.pdf");
        write_blank_pdf(&src);
        let before = std::fs::read(&src).expect("read src");
        let dest = temp_path("out.pdf");

        let engine: Arc<dyn OcrEngine> = Arc::new(FakeOcrEngine { words: vec![px_word("Scanned")] });
        let state = AppState::new(pdfium, None).with_ocr_engine(engine.clone());
        let document = pdfium.load_pdf_from_file(&src, None).expect("load blank");
        state
            .insert_document(
                "doc1".to_string(),
                DocEntry { document, file_path: src.clone() },
            )
            .expect("insert");

        let entry = state.get_document("doc1").expect("get");
        let result = save_searchable_copy_impl(
            |_, _| {},
            entry,
            "doc1".to_string(),
            dest.clone(),
            engine,
            state.ocr_cache_handle(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("save searchable");
        assert_eq!(result.pages_written, 1);

        let after = std::fs::read(&src).expect("read src again");
        assert_eq!(before, after, "source file must not be modified");

        std::fs::remove_file(&src).ok();
        std::fs::remove_file(&dest).ok();
    }

    /// A pre-set cancel token stops before any file is written.
    #[test]
    fn cancellation_writes_no_file() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();

        let src = temp_path("src.pdf");
        write_blank_pdf(&src);
        let dest = temp_path("out.pdf");
        std::fs::remove_file(&dest).ok();

        let state = AppState::new(pdfium, None);
        let document = pdfium.load_pdf_from_file(&src, None).expect("load blank");
        state
            .insert_document(
                "doc1".to_string(),
                DocEntry { document, file_path: src.clone() },
            )
            .expect("insert");

        let entry = state.get_document("doc1").expect("get");
        let result = save_searchable_copy_impl(
            |_, _| {},
            entry,
            "doc1".to_string(),
            dest.clone(),
            state.ocr_engine.clone(),
            state.ocr_cache_handle(),
            Arc::new(AtomicBool::new(true)),
        )
        .expect("save searchable");

        assert!(result.cancelled);
        assert_eq!(result.pages_written, 0);
        assert!(!std::path::Path::new(&dest).exists(), "cancelled run must not write");

        std::fs::remove_file(&src).ok();
    }
}
