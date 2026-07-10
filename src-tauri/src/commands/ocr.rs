use crate::error::AppError;
use crate::state::{lock_mutex, AppState, DocEntry};
use crate::commands::text::{page_text_in_document_order, TextRect};
use pdfium_render::prelude::*;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, State, WebviewWindow};

/// Recognized words per `(doc_id, page_1based)`, shared (`Arc`) so a blocking
/// task that can't borrow `&AppState` (e.g. text export) can still read/write it.
pub type OcrCache = Arc<Mutex<HashMap<(String, u32), Vec<OcrWord>>>>;

/// DPI used to rasterize a page before handing it to the OCR engine.
/// Recognition quality scales with input resolution; 300 DPI is the standard
/// "good enough for OCR" target and matches the scope doc's recommendation.
const OCR_DPI: f32 = 300.0;

/// Shown when no OCR language pack is installed. The frontend surfaces this
/// verbatim, so keep it actionable.
pub const OCR_UNAVAILABLE_MESSAGE: &str =
    "OCR is not available — install an OCR language pack in Windows Settings → \
     Time & Language → Language.";

/// A single recognized word and its bounding box.
///
/// `rect` is in PDF user space (points, origin bottom-left) — the same space a
/// persisted invisible-text layer would use. The search/extract fallbacks in
/// `text.rs` flip it to the top-left space the UI overlays expect.
#[derive(Serialize, Clone)]
pub struct OcrWord {
    pub text: String,
    pub rect: TextRect,
}

/// Seam over the real OCR backend. `WindowsOcrEngine` calls WinRT APIs that
/// require a language pack and may not be available on a CI/test machine, so
/// command logic is tested against `FakeOcrEngine` instead.
///
/// Implementations receive raw RGBA8 pixels and return words whose `rect` is in
/// **bitmap pixel space** (origin top-left). Coordinate mapping to PDF points is
/// the caller's job (`ocr_page_impl`), keeping the engine free of page geometry.
pub trait OcrEngine: Send + Sync {
    fn recognize(&self, rgba: &[u8], width: u32, height: u32) -> Result<Vec<OcrWord>, AppError>;
}

/// Maps a word's pixel-space bounding box (origin top-left, as WinRT reports it)
/// into PDF user space (origin bottom-left). Pure function — unit tested.
pub fn bitmap_rect_to_pdf_points(
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    bitmap_width: u32,
    bitmap_height: u32,
    page_width_pts: f32,
    page_height_pts: f32,
) -> TextRect {
    let x = left / bitmap_width as f32 * page_width_pts;
    let w = (right - left) / bitmap_width as f32 * page_width_pts;
    // PDF origin is bottom-left; bitmap origin is top-left, so flip Y.
    let y = (1.0 - bottom / bitmap_height as f32) * page_height_pts;
    let h = (bottom - top) / bitmap_height as f32 * page_height_pts;
    TextRect {
        x,
        y,
        width: w,
        height: h,
    }
}

/// Maps a batch of pixel-space words (as the engine returns them) into PDF
/// points. Shared by `ocr_page` and text export so they map identically.
pub fn map_words(
    raw: Vec<OcrWord>,
    bmp_w: u32,
    bmp_h: u32,
    page_w: f32,
    page_h: f32,
) -> Vec<OcrWord> {
    raw.into_iter()
        .map(|w| OcrWord {
            text: w.text,
            rect: bitmap_rect_to_pdf_points(
                w.rect.x,
                w.rect.y,
                w.rect.x + w.rect.width,
                w.rect.y + w.rect.height,
                bmp_w,
                bmp_h,
                page_w,
                page_h,
            ),
        })
        .collect()
}

/// Renders a page to RGBA at OCR DPI, returning
/// `(rgba, bitmap_w, bitmap_h, page_w_pts, page_h_pts)`. The caller holds the
/// document lock; the heavy `recognize` call should run *after* releasing it.
pub fn render_page_for_ocr(
    document: &PdfDocument,
    page: u32,
) -> Result<(Vec<u8>, u32, u32, f32, f32), AppError> {
    let pdf_page = document
        .pages()
        .get(page.saturating_sub(1) as i32)
        .map_err(|e| AppError::pdfium(format!("Failed to get page {page}"), e))?;

    let page_w = pdf_page.width().value;
    let page_h = pdf_page.height().value;
    let target_width = ((page_w / 72.0) * OCR_DPI).round().max(1.0) as u32;

    let config = PdfRenderConfig::new().set_target_width(target_width as Pixels);
    let bitmap = pdf_page
        .render_with_config(&config)
        .map_err(|e| AppError::pdfium(format!("Failed to render page {page}"), e))?;

    let rgba = bitmap.as_rgba_bytes();
    Ok((rgba, bitmap.width() as u32, bitmap.height() as u32, page_w, page_h))
}

/// Cache read against a bare `OcrCache` handle (no `AppState` needed).
pub fn cache_get(cache: &OcrCache, doc_id: &str, page: u32) -> Option<Vec<OcrWord>> {
    cache
        .lock()
        .ok()?
        .get(&(doc_id.to_string(), page))
        .cloned()
}

/// Cache write against a bare `OcrCache` handle.
pub fn cache_set(cache: &OcrCache, doc_id: &str, page: u32, words: Vec<OcrWord>) {
    if let Ok(mut c) = cache.lock() {
        c.insert((doc_id.to_string(), page), words);
    }
}

/// One reconstructed line of OCR text plus its bounding box (PDF user space,
/// origin bottom-left). Used to build line-level `TextItem`s for the
/// selectable text overlay and to serialize export text. Note the *cache*
/// stays per-word (`OcrWord`) — line grouping is a presentation concern, while
/// per-word boxes are what a future persisted `Tr 3` text layer needs.
#[derive(Clone)]
pub struct OcrLine {
    pub text: String,
    pub rect: TextRect,
}

/// Groups recognized words into visual lines. Words are in PDF user space
/// (origin bottom-left), so a larger `y` sits higher on the page. Lines are
/// ordered top→bottom, words within a line left→right; each line's text joins
/// its words with single spaces and its rect is the union of their boxes.
pub fn ocr_words_to_lines(words: &[OcrWord]) -> Vec<OcrLine> {
    if words.is_empty() {
        return Vec::new();
    }

    // Top-to-bottom (descending y), then left-to-right (ascending x), so words
    // on the same visual line end up adjacent.
    let mut sorted: Vec<&OcrWord> = words.iter().collect();
    sorted.sort_by(|a, b| {
        b.rect
            .y
            .partial_cmp(&a.rect.y)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                a.rect
                    .x
                    .partial_cmp(&b.rect.x)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });

    let mut groups: Vec<Vec<&OcrWord>> = Vec::new();
    for word in sorted {
        // Same line if its baseline is within half a line-height of the line's
        // first (topmost) word.
        let on_current = groups.last().and_then(|l| l.first()).is_some_and(|first| {
            let tol = first.rect.height.max(word.rect.height) * 0.5;
            (first.rect.y - word.rect.y).abs() <= tol
        });
        if on_current {
            groups.last_mut().unwrap().push(word);
        } else {
            groups.push(vec![word]);
        }
    }

    groups
        .into_iter()
        .map(|mut line| {
            line.sort_by(|a, b| {
                a.rect
                    .x
                    .partial_cmp(&b.rect.x)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let text = line
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let left = line.iter().map(|w| w.rect.x).fold(f32::INFINITY, f32::min);
            let right = line
                .iter()
                .map(|w| w.rect.x + w.rect.width)
                .fold(f32::NEG_INFINITY, f32::max);
            let bottom = line.iter().map(|w| w.rect.y).fold(f32::INFINITY, f32::min);
            let top = line
                .iter()
                .map(|w| w.rect.y + w.rect.height)
                .fold(f32::NEG_INFINITY, f32::max);
            OcrLine {
                text,
                rect: TextRect {
                    x: left,
                    y: bottom,
                    width: right - left,
                    height: top - bottom,
                },
            }
        })
        .collect()
}

/// Reconstructs readable text from recognized words for text export: line
/// texts joined with `\n`, top to bottom.
pub fn ocr_words_to_text(words: &[OcrWord]) -> String {
    ocr_words_to_lines(words)
        .into_iter()
        .map(|line| line.text)
        .collect::<Vec<_>>()
        .join("\n")
}

#[tauri::command]
pub fn ocr_page(
    state: State<'_, AppState>,
    doc_id: String,
    page: u32,
) -> Result<Vec<OcrWord>, String> {
    ocr_page_impl(&state, doc_id, page).map_err(String::from)
}

fn ocr_page_impl(state: &AppState, doc_id: String, page: u32) -> Result<Vec<OcrWord>, AppError> {
    let entry = state.get_document(&doc_id)?;
    ocr_page_into_cache(
        &entry,
        &doc_id,
        page,
        &state.ocr_engine,
        &state.ocr_cache_handle(),
    )
}

/// Recognizes one page into the OCR cache (or returns the already-cached
/// words). Takes the engine and cache explicitly rather than `&AppState` so it
/// can run inside a blocking task — shared by `ocr_page`, the document-level
/// "Make Searchable" action, and (eventually) "Save Searchable Copy".
///
/// The doc lock is held only to rasterize the page; the multi-second
/// `recognize` call runs after it's released.
pub fn ocr_page_into_cache(
    entry: &Arc<Mutex<DocEntry>>,
    doc_id: &str,
    page: u32,
    engine: &Arc<dyn OcrEngine>,
    cache: &OcrCache,
) -> Result<Vec<OcrWord>, AppError> {
    if let Some(cached) = cache_get(cache, doc_id, page) {
        return Ok(cached);
    }

    let (rgba, bmp_w, bmp_h, page_w, page_h) = {
        let entry = lock_mutex(entry)?;
        render_page_for_ocr(&entry.document, page)?
    };

    let raw_words = engine.recognize(&rgba, bmp_w, bmp_h)?;
    let words = map_words(raw_words, bmp_w, bmp_h, page_w, page_h);

    cache_set(cache, doc_id, page, words.clone());
    Ok(words)
}

/// Per-page progress for any document-wide OCR run (Make Searchable, and the
/// OCR pass of Export Text). Emitted on the `ocr-progress` event.
#[derive(Serialize, Clone)]
pub struct OcrProgress {
    pub page: u32,
    pub total: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrDocumentResult {
    /// Text-less pages on which OCR produced recognized words this run.
    pub pages_ocred: u32,
    pub cancelled: bool,
}

/// Document-level "Make Searchable": OCRs every page with no native text layer
/// into the cache, so search, selection/copy, and a later export all benefit.
/// Nothing is written to disk — this is the ephemeral tier.
#[tauri::command]
pub async fn ocr_document(
    window: WebviewWindow,
    state: State<'_, AppState>,
    doc_id: String,
) -> Result<OcrDocumentResult, String> {
    let entry = state.get_document(&doc_id).map_err(String::from)?;
    let engine = state.ocr_engine.clone();
    let cache = state.ocr_cache_handle();
    let cancel = Arc::new(AtomicBool::new(false));
    state.set_ocr_job(cancel.clone());

    let emit = move |page, total| {
        let _ = window.emit("ocr-progress", OcrProgress { page, total });
    };

    let result = tauri::async_runtime::spawn_blocking(move || {
        ocr_document_impl(emit, entry, doc_id, engine, cache, cancel)
    })
    .await
    .map_err(|e| e.to_string());

    state.take_ocr_job();
    result?.map_err(String::from)
}

fn ocr_document_impl(
    emit_progress: impl Fn(u32, u32),
    entry: Arc<Mutex<DocEntry>>,
    doc_id: String,
    engine: Arc<dyn OcrEngine>,
    cache: OcrCache,
    cancel: Arc<AtomicBool>,
) -> Result<OcrDocumentResult, AppError> {
    let page_count = lock_mutex(&entry)?.document.pages().len() as u32;
    let mut pages_ocred = 0u32;

    for i in 0..page_count {
        let page_num = i + 1;

        if cancel.load(Ordering::Relaxed) {
            return Ok(OcrDocumentResult {
                pages_ocred,
                cancelled: true,
            });
        }
        emit_progress(page_num, page_count);

        // Skip pages that already have a native text layer — only scans need OCR.
        let native_empty = {
            let entry = lock_mutex(&entry)?;
            let page = entry
                .document
                .pages()
                .get(i as i32)
                .map_err(|e| AppError::pdfium(format!("Failed to get page {page_num}"), e))?;
            page.text()
                .map(|t| page_text_in_document_order(&t))
                .unwrap_or_default()
                .trim()
                .is_empty()
        };
        if !native_empty {
            continue;
        }

        let words = ocr_page_into_cache(&entry, &doc_id, page_num, &engine, &cache)?;
        if !words.is_empty() {
            pages_ocred += 1;
        }
    }

    Ok(OcrDocumentResult {
        pages_ocred,
        cancelled: false,
    })
}

/// Signals an in-progress document-wide OCR run (Make Searchable or Export
/// Text's OCR pass) to stop after the current page.
#[tauri::command]
pub fn cancel_ocr(state: State<'_, AppState>) -> Result<(), String> {
    state.cancel_ocr_job();
    Ok(())
}

// ── WindowsOcrEngine (WinRT) ────────────────────────────────────────────────

use windows::Foundation::Rect;
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine as WinRtOcrEngine;
use windows::Storage::Streams::DataWriter;
use windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED};

/// Real OCR engine backed by `Windows.Media.Ocr`. No bundled binaries — the
/// engine ships with Windows 10+. Follows the same "fresh OS thread, fresh COM
/// apartment" pattern as `theme.rs`: WinRT requires an initialized apartment,
/// and reusing a Tauri worker thread that the dialog/print code already put into
/// an STA apartment would make `RoInitialize(MTA)` fail with RPC_E_CHANGED_MODE.
pub struct WindowsOcrEngine;

impl WindowsOcrEngine {
    pub fn new() -> Self {
        WindowsOcrEngine
    }
}

impl Default for WindowsOcrEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl OcrEngine for WindowsOcrEngine {
    fn recognize(&self, rgba: &[u8], width: u32, height: u32) -> Result<Vec<OcrWord>, AppError> {
        let rgba = rgba.to_vec();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            tx.send(recognize_on_fresh_thread(&rgba, width, height)).ok();
        });
        rx.recv()
            .map_err(|e| AppError::Other(format!("OCR thread channel recv failed: {e}")))?
    }
}

fn recognize_on_fresh_thread(
    rgba: &[u8],
    width: u32,
    height: u32,
) -> Result<Vec<OcrWord>, AppError> {
    unsafe { RoInitialize(RO_INIT_MULTITHREADED) }
        .map_err(|e| AppError::Other(format!("RoInitialize failed: {e}")))?;
    let result = recognize_inner(rgba, width, height);
    unsafe { RoUninitialize() };
    result
}

fn recognize_inner(rgba: &[u8], width: u32, height: u32) -> Result<Vec<OcrWord>, AppError> {
    // Language-pack check: a clear, actionable error beats a cryptic WinRT
    // failure when no recognizer is installed.
    let languages = WinRtOcrEngine::AvailableRecognizerLanguages()
        .map_err(|e| AppError::Other(format!("Failed to query OCR languages: {e}")))?;
    if languages.Size().unwrap_or(0) == 0 {
        return Err(AppError::Other(OCR_UNAVAILABLE_MESSAGE.to_string()));
    }

    let engine = WinRtOcrEngine::TryCreateFromUserProfileLanguages()
        .map_err(|e| AppError::Other(format!("Failed to create OCR engine: {e}")))?;

    // Wrap the RGBA bytes in an IBuffer, then build a SoftwareBitmap from it.
    let writer = DataWriter::new()
        .map_err(|e| AppError::Other(format!("DataWriter::new failed: {e}")))?;
    writer
        .WriteBytes(rgba)
        .map_err(|e| AppError::Other(format!("DataWriter::WriteBytes failed: {e}")))?;
    let buffer = writer
        .DetachBuffer()
        .map_err(|e| AppError::Other(format!("DataWriter::DetachBuffer failed: {e}")))?;

    let bitmap = SoftwareBitmap::CreateCopyFromBuffer(
        &buffer,
        BitmapPixelFormat::Rgba8,
        width as i32,
        height as i32,
    )
    .map_err(|e| AppError::Other(format!("SoftwareBitmap::CreateCopyFromBuffer failed: {e}")))?;

    let result = engine
        .RecognizeAsync(&bitmap)
        .map_err(|e| AppError::Other(format!("RecognizeAsync failed: {e}")))?
        .get()
        .map_err(|e| AppError::Other(format!("OCR result await failed: {e}")))?;

    let lines = result
        .Lines()
        .map_err(|e| AppError::Other(format!("Failed to read OCR lines: {e}")))?;

    let mut words = Vec::new();
    for line in lines {
        let line_words = line
            .Words()
            .map_err(|e| AppError::Other(format!("Failed to read OCR words: {e}")))?;
        for word in line_words {
            let text = word
                .Text()
                .map_err(|e| AppError::Other(format!("Failed to read OCR word text: {e}")))?
                .to_string();
            let Rect {
                X,
                Y,
                Width,
                Height,
            } = word
                .BoundingRect()
                .map_err(|e| AppError::Other(format!("Failed to read OCR word rect: {e}")))?;
            words.push(OcrWord {
                text,
                // Pixel space, origin top-left; mapped to PDF points by the caller.
                rect: TextRect {
                    x: X,
                    y: Y,
                    width: Width,
                    height: Height,
                },
            });
        }
    }

    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::DocEntry;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Records how many times `recognize` runs so the cache test can prove the
    /// engine is hit exactly once across two `ocr_page` calls.
    struct FakeOcrEngine {
        words: Vec<OcrWord>,
        call_count: AtomicUsize,
    }

    impl OcrEngine for FakeOcrEngine {
        fn recognize(&self, _rgba: &[u8], _w: u32, _h: u32) -> Result<Vec<OcrWord>, AppError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(self.words.clone())
        }
    }

    fn open_fixture(state: &AppState, doc_id: &str) {
        let pdfium = crate::test_pdfium();
        let src = crate::fixture_path();
        let entry = DocEntry::load(pdfium, &src.to_string_lossy(), None).expect("load pdf");
        state.insert_document(doc_id.to_string(), entry).expect("insert");
    }

    fn word(text: &str) -> OcrWord {
        OcrWord {
            text: text.to_string(),
            // Pixel-space rect; coordinate values are irrelevant to these tests.
            rect: TextRect {
                x: 10.0,
                y: 10.0,
                width: 40.0,
                height: 12.0,
            },
        }
    }

    /// A word at the very top of the bitmap (top=0) must land at the top of the
    /// page in PDF space. With a bottom-left origin that means a *high* y value
    /// (near `page_height`), confirming the Y axis is flipped.
    #[test]
    fn bitmap_rect_to_pdf_points_flips_y_axis() {
        // 100×200 bitmap, 72×144 pt page; word spans the top 20px.
        let rect = bitmap_rect_to_pdf_points(0.0, 0.0, 100.0, 20.0, 100, 200, 72.0, 144.0);
        // y = (1 - 20/200) * 144 = 129.6
        assert!((rect.y - 129.6).abs() < 0.1, "unexpected y: {}", rect.y);
        // Spans the full width of the page.
        assert!((rect.width - 72.0).abs() < 0.1, "unexpected width: {}", rect.width);
        // Height = 20/200 * 144 = 14.4
        assert!((rect.height - 14.4).abs() < 0.1, "unexpected height: {}", rect.height);
    }

    /// Builds a word in PDF user space (origin bottom-left): larger `y` is
    /// higher on the page.
    fn pt_word(text: &str, x: f32, y: f32) -> OcrWord {
        OcrWord {
            text: text.to_string(),
            rect: TextRect {
                x,
                y,
                width: 30.0,
                height: 10.0,
            },
        }
    }

    #[test]
    fn ocr_words_to_text_empty_is_empty_string() {
        assert_eq!(ocr_words_to_text(&[]), "");
    }

    #[test]
    fn ocr_words_to_text_orders_lines_top_down_and_words_left_right() {
        // Two lines: top line (y=100) "Hello World", bottom line (y=50) "Bye".
        // Provide them out of order to prove sorting.
        let words = vec![
            pt_word("World", 40.0, 100.0),
            pt_word("Bye", 10.0, 50.0),
            pt_word("Hello", 10.0, 100.0),
        ];
        assert_eq!(ocr_words_to_text(&words), "Hello World\nBye");
    }

    #[test]
    fn ocr_words_to_text_groups_slightly_misaligned_words_on_one_line() {
        // Same visual line: baselines differ by less than half the line height.
        let words = vec![pt_word("a", 10.0, 100.0), pt_word("b", 50.0, 103.0)];
        assert_eq!(ocr_words_to_text(&words), "a b");
    }

    #[test]
    fn ocr_words_to_lines_unions_word_boxes_per_line() {
        // One line, two words: "Hello" at x=10 (w=30) and "World" at x=50 (w=30).
        let lines = ocr_words_to_lines(&[pt_word("World", 50.0, 100.0), pt_word("Hello", 10.0, 100.0)]);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "Hello World");
        // Bounding box spans x=10..80 (left of first word to right of last).
        assert!((lines[0].rect.x - 10.0).abs() < 0.01, "x: {}", lines[0].rect.x);
        assert!((lines[0].rect.width - 70.0).abs() < 0.01, "w: {}", lines[0].rect.width);
    }

    fn open_blank_doc(state: &AppState, doc_id: &str, pages: u32) {
        let pdfium = crate::test_pdfium();
        let mut doc = pdfium.create_new_pdf().expect("create pdf");
        for i in 0..pages {
            doc.pages_mut()
                .create_page_at_index(
                    PdfPagePaperSize::new_custom(PdfPoints::new(200.0), PdfPoints::new(200.0)),
                    i as PdfPageIndex,
                )
                .expect("create blank page");
        }
        state
            .insert_document(
                doc_id.to_string(),
                DocEntry {
                    document: doc,
                    file_path: format!("{doc_id}.pdf"),
                    // No backing file; these tests never touch the buffer.
                    buffer: Vec::new(),
                    dirty: false,
                    protection: crate::state::Protection::Plaintext,
                    linearized: false,
                },
            )
            .expect("insert");
    }

    fn no_progress(_page: u32, _total: u32) {}

    /// "Make Searchable" OCRs every text-less page into the cache.
    #[test]
    fn ocr_document_ocrs_blank_pages_into_cache() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let fake: Arc<dyn OcrEngine> = Arc::new(FakeOcrEngine {
            words: vec![word("scanned")],
            call_count: AtomicUsize::new(0),
        });
        let state = AppState::new(pdfium, None).with_ocr_engine(fake);
        open_blank_doc(&state, "blank", 2);

        let entry = state.get_document("blank").expect("get");
        let cache = state.ocr_cache_handle();
        let result = ocr_document_impl(
            no_progress,
            entry,
            "blank".to_string(),
            state.ocr_engine.clone(),
            cache.clone(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("ocr document");

        assert_eq!(result.pages_ocred, 2);
        assert!(!result.cancelled);
        assert!(cache_get(&cache, "blank", 1).is_some());
        assert!(cache_get(&cache, "blank", 2).is_some());
    }

    /// A pre-set cancel token stops the run before any page is processed.
    #[test]
    fn ocr_document_honors_cancellation() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        open_blank_doc(&state, "blank", 3);

        let entry = state.get_document("blank").expect("get");
        let result = ocr_document_impl(
            no_progress,
            entry,
            "blank".to_string(),
            state.ocr_engine.clone(),
            state.ocr_cache_handle(),
            Arc::new(AtomicBool::new(true)),
        )
        .expect("ocr document");

        assert!(result.cancelled);
        assert_eq!(result.pages_ocred, 0);
    }

    /// First `ocr_page` runs the engine and caches the result; the second call
    /// must serve from cache and NOT invoke the engine again.
    #[test]
    fn ocr_page_caches_result_on_second_call() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let fake = Arc::new(FakeOcrEngine {
            words: vec![word("hello"), word("world")],
            call_count: AtomicUsize::new(0),
        });
        let state = AppState::new(pdfium, None).with_ocr_engine(fake.clone());
        open_fixture(&state, "doc1");

        let words1 = ocr_page_impl(&state, "doc1".to_string(), 1).expect("first ocr");
        let words2 = ocr_page_impl(&state, "doc1".to_string(), 1).expect("second ocr");

        assert_eq!(words1.len(), 2);
        assert_eq!(words2.len(), 2);
        assert_eq!(
            fake.call_count.load(Ordering::SeqCst),
            1,
            "engine should run once; the second call must hit the cache"
        );
    }

    /// The pixel-space rects the engine returns are mapped into PDF points
    /// before caching, so cached words are in PDF user space (bottom-left).
    #[test]
    fn ocr_page_maps_pixel_rects_into_pdf_points() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let fake = Arc::new(FakeOcrEngine {
            words: vec![word("hello")],
            call_count: AtomicUsize::new(0),
        });
        let state = AppState::new(pdfium, None).with_ocr_engine(fake);
        open_fixture(&state, "doc1");

        let words = ocr_page_impl(&state, "doc1".to_string(), 1).expect("ocr");
        let rect = &words[0].rect;
        // Fixture page is 200×200 pt, rendered at 300 DPI → ~833px wide bitmap.
        // The pixel rect (x=10) maps to a small positive x and a y near the top
        // of the page (high value under bottom-left origin).
        assert!(rect.x > 0.0 && rect.x < 200.0, "x out of range: {}", rect.x);
        assert!(rect.y > 100.0 && rect.y <= 200.0, "y not near top: {}", rect.y);
    }

    /// Real WinRT path. Requires an installed OCR language pack, so it's not run
    /// in CI — exercised manually on a dev machine.
    #[test]
    #[ignore = "requires Windows OCR language pack"]
    fn windows_ocr_engine_recognizes_text_in_rendered_page() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None); // real WindowsOcrEngine default
        open_fixture(&state, "doc1");

        let words = ocr_page_impl(&state, "doc1".to_string(), 1).expect("ocr");
        // The fixture page renders the text "Test Fixture".
        let joined = words
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            joined.to_lowercase().contains("test"),
            "expected recognized text to contain 'test', got: {joined:?}"
        );
    }
}
