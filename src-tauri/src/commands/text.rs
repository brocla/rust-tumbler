use crate::commands::ocr::{
    cache_get, ocr_page_into_cache, ocr_words_to_lines, ocr_words_to_text, OcrCache, OcrEngine,
    OcrLine, OcrProgress,
};
use regex::Regex;
use crate::error::AppError;
use crate::state::{lock_mutex, AppState, DocEntry};
use pdfium_render::prelude::*;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, State, WebviewWindow};

#[derive(Serialize, Clone)]
pub struct TextRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextItem {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub font_size: f32,
}

#[derive(Serialize)]
pub struct SearchResult {
    pub page: u32,
    pub rects: Vec<TextRect>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextExportResult {
    pub pages: u32,
    /// How many pages contributed text via OCR (vs. a native text layer).
    pub ocr_pages: u32,
    pub cancelled: bool,
}

/// Returns the effective left and bottom origin of the page's bounding box.
/// Most PDFs have origin (0,0), but some have non-zero origins that shift
/// text coordinates relative to the rendered output.
fn page_origin(page: &PdfPage) -> (f32, f32) {
    // Try CropBox first (used for display), fall back to MediaBox
    let bbox = page
        .boundaries()
        .crop()
        .or_else(|_| page.boundaries().media());

    match bbox {
        Ok(b) => (b.bounds.left().value, b.bounds.bottom().value),
        Err(_) => (0.0, 0.0),
    }
}

#[tauri::command]
pub fn extract_page_text(
    state: State<'_, AppState>,
    doc_id: String,
    page: u32,
) -> Result<Vec<TextItem>, String> {
    extract_page_text_impl(&state, doc_id, page).map_err(String::from)
}

fn extract_page_text_impl(
    state: &AppState,
    doc_id: String,
    page: u32,
) -> Result<Vec<TextItem>, AppError> {
    let entry = state.get_document(&doc_id)?;
    let entry = lock_mutex(&entry)?;

    let pdf_page = entry
        .document
        .pages()
        .get(page.saturating_sub(1) as i32)
        .map_err(|e| AppError::pdfium(format!("Failed to get page {page}"), e))?;

    let page_height = pdf_page.height().value;
    let (origin_x, origin_y) = page_origin(&pdf_page);

    let text = pdf_page
        .text()
        .map_err(|e| AppError::pdfium("Failed to get text", e))?;

    let mut items: Vec<TextItem> = Vec::new();
    let mut current_text = String::new();
    let mut current_x: f32 = 0.0;
    let mut current_y: f32 = 0.0;
    let mut current_width: f32 = 0.0;
    let mut current_height: f32 = 0.0;
    let mut current_font_size: f32 = 0.0;
    let mut has_current = false;

    for ch in text.chars().iter() {
        let unicode = match ch.unicode_char() {
            Some(c) => c,
            None => continue,
        };

        // Get character bounds
        let bounds = match ch.loose_bounds() {
            Ok(b) => b,
            Err(_) => continue,
        };

        let font_size = ch.scaled_font_size().value;

        // Convert PDF coordinates (origin bottom-left) to top-left origin,
        // adjusting for any non-zero page origin
        let char_x = bounds.left().value - origin_x;
        let char_y = page_height - (bounds.top().value - origin_y);
        let char_w = bounds.right().value - bounds.left().value;
        let char_h = bounds.top().value - bounds.bottom().value;

        // Group characters into text runs based on proximity and font size
        let same_line = has_current
            && (font_size - current_font_size).abs() < 0.5
            && (char_y - current_y).abs() < current_height * 0.5
            && (char_x - (current_x + current_width)).abs() < font_size * 0.5;

        if same_line {
            current_text.push(unicode);
            current_width = (char_x + char_w) - current_x;
            if char_h > current_height {
                current_height = char_h;
            }
        } else {
            // Flush previous item
            if has_current && !current_text.trim().is_empty() {
                items.push(TextItem {
                    text: current_text.clone(),
                    x: current_x,
                    y: current_y,
                    width: current_width,
                    height: current_height,
                    font_size: current_font_size,
                });
            }
            // Start new run
            current_text = String::from(unicode);
            current_x = char_x;
            current_y = char_y;
            current_width = char_w;
            current_height = char_h;
            current_font_size = font_size;
            has_current = true;
        }
    }

    // Flush last item
    if has_current && !current_text.trim().is_empty() {
        items.push(TextItem {
            text: current_text,
            x: current_x,
            y: current_y,
            width: current_width,
            height: current_height,
            font_size: current_font_size,
        });
    }

    // Fallback: a page with no native text layer (a scan) yields nothing above.
    // If OCR has been run for it, serve the recognized words — grouped into
    // lines — so the overlay has selectable spans whose copied text reads
    // correctly (words joined with spaces, one span per line).
    if items.is_empty() {
        if let Some(words) = state.get_ocr_words(&doc_id, page) {
            return Ok(ocr_words_to_lines(&words)
                .iter()
                .map(|line| ocr_line_to_text_item(line, page_height))
                .collect());
        }
    }

    Ok(items)
}

/// Converts a cached OCR line (PDF user space, origin bottom-left) into a
/// `TextItem` (origin top-left, as the text overlay expects). The font size is
/// approximated from the box height since OCR has no glyph metrics.
fn ocr_line_to_text_item(line: &OcrLine, page_height: f32) -> TextItem {
    TextItem {
        text: line.text.clone(),
        x: line.rect.x,
        y: page_height - (line.rect.y + line.rect.height),
        width: line.rect.width,
        height: line.rect.height,
        font_size: line.rect.height,
    }
}

#[tauri::command]
pub fn search_document(
    state: State<'_, AppState>,
    doc_id: String,
    query: String,
    match_case: bool,
    whole_word: bool,
    use_regex: bool,
) -> Result<Vec<SearchResult>, String> {
    search_document_impl(&state, doc_id, query, match_case, whole_word, use_regex)
        .map_err(String::from)
}

fn search_document_impl(
    state: &AppState,
    doc_id: String,
    query: String,
    match_case: bool,
    whole_word: bool,
    use_regex: bool,
) -> Result<Vec<SearchResult>, AppError> {
    if query.is_empty() {
        return Ok(Vec::new());
    }

    // Compile the regex once (before any page loop) if regex mode is active.
    // An invalid pattern returns an error immediately.
    let regex_pattern = if use_regex {
        Some(
            Regex::new(&query)
                .map_err(|e| AppError::Other(format!("Invalid regex: {e}")))?,
        )
    } else {
        None
    };

    let entry = state.get_document(&doc_id)?;
    let entry = lock_mutex(&entry)?;

    let page_count = entry.document.pages().len();
    let mut results = Vec::new();

    let options = PdfSearchOptions::new()
        .match_case(match_case)
        .match_whole_word(whole_word);

    for page_idx in 0..page_count {
        let pdf_page = match entry.document.pages().get(page_idx as i32) {
            Ok(p) => p,
            Err(_) => continue,
        };

        let page_height = pdf_page.height().value;
        let (origin_x, origin_y) = page_origin(&pdf_page);

        let text = match pdf_page.text() {
            Ok(t) => t,
            Err(_) => continue,
        };

        let mut page_rects = Vec::new();

        if let Some(ref re) = regex_pattern {
            // Regex mode: extract the full page text, find all matches,
            // then use pdfium's text.search() on each unique literal matched
            // string to obtain the highlight rectangles.
            // Deduplication prevents calling text.search() N times for the
            // same string, which would return all page-wide occurrences each
            // time and produce duplicate rects.
            let full_text = text.all();
            let unique_matches: std::collections::HashSet<&str> =
                re.find_iter(&full_text).map(|m| m.as_str()).collect();
            // Use match_case(true): matched_str is the exact string the regex
            // found, so the rect lookup must be case-exact so it does not also
            // return rects for case-variants the regex did not select.
            let lit_options = PdfSearchOptions::new().match_case(true);
            for matched_str in &unique_matches {
                if let Ok(search) = text.search(matched_str, &lit_options) {
                    for match_segments in search.iter(PdfSearchDirection::SearchForward) {
                        for i in 0..match_segments.len() {
                            if let Ok(segment) = match_segments.get(i) {
                                let bounds = segment.bounds();
                                let x = bounds.left().value - origin_x;
                                let y = page_height - (bounds.top().value - origin_y);
                                let w = bounds.right().value - bounds.left().value;
                                let h = bounds.top().value - bounds.bottom().value;
                                page_rects.push(TextRect {
                                    x,
                                    y,
                                    width: w,
                                    height: h,
                                });
                            }
                        }
                    }
                }
            }
        } else {
            // Non-regex mode: delegate match_case / whole_word to pdfium.
            let search = match text.search(&query, &options) {
                Ok(s) => s,
                Err(_) => continue,
            };

            // Each match returns PdfPageTextSegments — one or more visual
            // rectangles (e.g. a match that spans a line break yields two rects).
            // These rects come from FPDFText_GetRect, which is the canonical
            // pdfium function for computing highlight positions.
            for match_segments in search.iter(PdfSearchDirection::SearchForward) {
                for i in 0..match_segments.len() {
                    if let Ok(segment) = match_segments.get(i) {
                        let bounds = segment.bounds();
                        let x = bounds.left().value - origin_x;
                        let y = page_height - (bounds.top().value - origin_y);
                        let w = bounds.right().value - bounds.left().value;
                        let h = bounds.top().value - bounds.bottom().value;

                        page_rects.push(TextRect {
                            x,
                            y,
                            width: w,
                            height: h,
                        });
                    }
                }
            }
        }

        // Fallback: no native hits on this page. If OCR words are cached for
        // it (a scanned page made searchable), match the query against them.
        if page_rects.is_empty() {
            let page_num = (page_idx + 1) as u32;
            if let Some(words) = state.get_ocr_words(&doc_id, page_num) {
                if let Some(ref re) = regex_pattern {
                    // Regex mode: reconstruct page text with per-word byte
                    // offsets so patterns spanning multiple tokens (e.g.
                    // `Test\s+Fixture`) can match across word boundaries.
                    let mut page_text = String::new();
                    let mut word_spans: Vec<(usize, usize)> = Vec::new();
                    for word in &words {
                        let start = page_text.len();
                        page_text.push_str(&word.text);
                        word_spans.push((start, page_text.len()));
                        page_text.push(' ');
                    }
                    for mat in re.find_iter(&page_text) {
                        let (ms, me) = (mat.start(), mat.end());
                        for (i, &(ws, we)) in word_spans.iter().enumerate() {
                            if ws < me && we > ms {
                                let word = &words[i];
                                page_rects.push(TextRect {
                                    x: word.rect.x,
                                    y: page_height - (word.rect.y + word.rect.height),
                                    width: word.rect.width,
                                    height: word.rect.height,
                                });
                            }
                        }
                    }
                } else {
                    // Non-regex mode: test each word token.
                    // Compute needle once outside the loop to avoid a heap
                    // allocation per word for the lowercased query string.
                    let needle = if match_case {
                        query.clone()
                    } else {
                        query.to_lowercase()
                    };
                    for word in &words {
                        let matches = if match_case {
                            if whole_word {
                                word.text == needle
                            } else {
                                word.text.contains(&needle)
                            }
                        } else {
                            let haystack = word.text.to_lowercase();
                            if whole_word {
                                haystack == needle
                            } else {
                                haystack.contains(&needle)
                            }
                        };
                        if matches {
                            page_rects.push(TextRect {
                                x: word.rect.x,
                                y: page_height - (word.rect.y + word.rect.height),
                                width: word.rect.width,
                                height: word.rect.height,
                            });
                        }
                    }
                }
            }
        }

        if !page_rects.is_empty() {
            results.push(SearchResult {
                page: (page_idx + 1) as u32,
                rects: page_rects,
            });
        }
    }

    Ok(results)
}

/// Counts pages that would still need OCR: no native text layer **and** no
/// recognized words already in the OCR cache. Drives the frontend's "run OCR on
/// export?" confirmation — so once a page has been made searchable, it no
/// longer triggers the prompt.
#[tauri::command]
pub async fn count_pages_without_text(
    state: State<'_, AppState>,
    doc_id: String,
) -> Result<u32, String> {
    let entry = state.get_document(&doc_id).map_err(String::from)?;
    let cache = state.ocr_cache_handle();
    tauri::async_runtime::spawn_blocking(move || count_pages_without_text_impl(entry, doc_id, cache))
        .await
        .map_err(|e| e.to_string())?
        .map_err(String::from)
}

fn count_pages_without_text_impl(
    entry: Arc<Mutex<DocEntry>>,
    doc_id: String,
    cache: OcrCache,
) -> Result<u32, AppError> {
    let entry = lock_mutex(&entry)?;
    let page_count = entry.document.pages().len();
    let mut count = 0;
    for i in 0..page_count {
        let page_num = (i + 1) as u32;
        let page = entry
            .document
            .pages()
            .get(i)
            .map_err(|e| AppError::pdfium(format!("Failed to get page {page_num}"), e))?;
        let content = page.text().map(|t| t.all()).unwrap_or_default();
        // A page already OCR'd (cached) is "covered" even though its native
        // text layer is still empty.
        if content.trim().is_empty() && cache_get(&cache, &doc_id, page_num).is_none() {
            count += 1;
        }
    }
    Ok(count)
}

#[tauri::command]
pub async fn export_text(
    window: WebviewWindow,
    state: State<'_, AppState>,
    doc_id: String,
    dest_path: String,
    use_ocr: bool,
) -> Result<TextExportResult, String> {
    let entry = state.get_document(&doc_id).map_err(String::from)?;
    let engine = state.ocr_engine.clone();
    let cache = state.ocr_cache_handle();
    let cancel = Arc::new(AtomicBool::new(false));
    state.set_ocr_job(cancel.clone());

    // Forward per-page progress to the frontend on the shared `ocr-progress`
    // channel; the impl stays WebviewWindow-free so it's unit-testable with a
    // no-op closure.
    let emit = move |page, total| {
        let _ = window.emit("ocr-progress", OcrProgress { page, total });
    };

    let result = tauri::async_runtime::spawn_blocking(move || {
        export_text_impl(emit, entry, doc_id, dest_path, use_ocr, engine, cache, cancel)
    })
    .await
    .map_err(|e| e.to_string());

    state.take_ocr_job();
    result?.map_err(String::from)
}

#[allow(clippy::too_many_arguments)]
fn export_text_impl(
    emit_progress: impl Fn(u32, u32),
    entry: Arc<Mutex<DocEntry>>,
    doc_id: String,
    dest_path: String,
    use_ocr: bool,
    engine: Arc<dyn OcrEngine>,
    cache: OcrCache,
    cancel: Arc<AtomicBool>,
) -> Result<TextExportResult, AppError> {
    let page_count = lock_mutex(&entry)?.document.pages().len() as u32;

    let mut output = String::new();
    let mut ocr_pages = 0u32;
    let mut processed = 0u32;

    for i in 0..page_count {
        let page_num = i + 1;

        if cancel.load(Ordering::Relaxed) {
            return Ok(TextExportResult {
                pages: processed,
                ocr_pages,
                cancelled: true,
            });
        }
        emit_progress(page_num, page_count);

        // Read native text under a short-lived lock.
        let native = {
            let entry = lock_mutex(&entry)?;
            let page = entry
                .document
                .pages()
                .get(i as i32)
                .map_err(|e| AppError::pdfium(format!("Failed to get page {page_num}"), e))?;
            page.text().map(|t| t.all()).unwrap_or_default()
        }; // lock released here

        let page_text = if !native.trim().is_empty() {
            native
        } else {
            // No native text. Always use cached OCR if present (e.g. from
            // "Make Searchable" or a prior search); only run *new* OCR when the
            // user opted in. `ocr_page_into_cache` does the cache-or-recognize
            // step off the doc lock.
            let words = if use_ocr {
                Some(ocr_page_into_cache(&entry, &doc_id, page_num, &engine, &cache)?)
            } else {
                cache_get(&cache, &doc_id, page_num)
            };
            match words {
                Some(words) => {
                    let text = ocr_words_to_text(&words);
                    if !text.trim().is_empty() {
                        ocr_pages += 1;
                    }
                    text
                }
                None => String::new(),
            }
        };

        if page_num > 1 {
            output.push_str("\n\n");
        }
        output.push_str(&format!("--- Page {page_num} ---\n"));
        if page_text.trim().is_empty() {
            output.push_str("[no extractable text]");
        } else {
            output.push_str(&page_text);
        }
        processed = page_num;
    }

    // Write via a temp file then atomic rename so a disk-full or crash does
    // not truncate an existing file at dest_path.
    let tmp_path = format!("{dest_path}.tmp");
    std::fs::write(&tmp_path, output.as_bytes())
        .map_err(|e| AppError::io(format!("Failed to write to {tmp_path}"), e))?;
    std::fs::rename(&tmp_path, &dest_path)
        .map_err(|e| AppError::io(format!("Failed to rename {tmp_path} to {dest_path}"), e))?;

    Ok(TextExportResult {
        pages: page_count,
        ocr_pages,
        cancelled: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::ocr::OcrWord;
    use crate::state::DocEntry;

    /// Loads the checked-in fixture into `state` under `doc_id`.
    fn open_fixture(state: &AppState, doc_id: &str) {
        let src = crate::fixture_path();
        let pdfium = crate::test_pdfium();
        let document = pdfium
            .load_pdf_from_file(src.to_str().unwrap(), None)
            .expect("load pdf");
        state
            .insert_document(
                doc_id.to_string(),
                DocEntry {
                    document,
                    file_path: src.to_string_lossy().into_owned(),
                },
            )
            .expect("insert");
    }

    /// `sample.pdf` is a single 200x200 page containing the text "Test
    /// Fixture" in one run at 24pt, starting near the top-left of the page.
    /// This pins both the run-grouping logic and the coordinate conversion
    /// (PDF bottom-left origin -> top-left origin used by the UI).
    #[test]
    fn extract_page_text_returns_single_run_with_position() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        open_fixture(&state, "doc1");

        let items = extract_page_text_impl(&state, "doc1".to_string(), 1).expect("extract");

        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.text, "Test Fixture");
        assert_eq!(item.font_size, 24.0);
        assert!((item.x - 20.0).abs() < 0.5, "unexpected x: {}", item.x);
        assert!((item.y - 78.28).abs() < 0.5, "unexpected y: {}", item.y);
        assert!(item.width > 100.0, "unexpected width: {}", item.width);
        assert!(item.height > 0.0, "unexpected height: {}", item.height);
    }

    #[test]
    fn extract_page_text_for_missing_page_is_error() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        open_fixture(&state, "doc1");

        match extract_page_text_impl(&state, "doc1".to_string(), 99) {
            Err(AppError::Pdfium { .. }) => {}
            Err(other) => panic!("expected AppError::Pdfium, got {other:?}"),
            Ok(_) => panic!("expected an error for an out-of-range page"),
        }
    }

    /// Searching for a word that appears on the page returns one rect with a
    /// sensible size, using the same coordinate conversion as
    /// `extract_page_text`.
    #[test]
    fn search_document_finds_known_word_with_nonempty_rect() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        open_fixture(&state, "doc1");

        let results = search_document_impl(
            &state,
            "doc1".to_string(),
            "Test".to_string(),
            false,
            false,
            false,
        )
        .expect("search");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].page, 1);
        assert_eq!(results[0].rects.len(), 1);

        let rect = &results[0].rects[0];
        assert!(rect.width > 0.0, "unexpected width: {}", rect.width);
        assert!(rect.height > 0.0, "unexpected height: {}", rect.height);
        assert!(rect.x >= 0.0 && rect.y >= 0.0);
    }

    #[test]
    fn search_document_returns_empty_for_word_not_present() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        open_fixture(&state, "doc1");

        let results = search_document_impl(
            &state,
            "doc1".to_string(),
            "Nonexistent".to_string(),
            false,
            false,
            false,
        )
        .expect("search");

        assert!(results.is_empty());
    }

    #[test]
    fn search_document_returns_empty_for_empty_query() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        open_fixture(&state, "doc1");

        let results = search_document_impl(
            &state,
            "doc1".to_string(),
            String::new(),
            false,
            false,
            false,
        )
        .expect("search");

        assert!(results.is_empty());
    }

    fn ocr_word(text: &str) -> OcrWord {
        // Rect in PDF user space (origin bottom-left), as the cache stores it.
        OcrWord {
            text: text.to_string(),
            rect: TextRect {
                x: 10.0,
                y: 150.0,
                width: 40.0,
                height: 12.0,
            },
        }
    }

    /// When a page has no native hits for the query, `search_document` falls
    /// back to the OCR cache. Searching the text-only fixture for a word that
    /// isn't in its text layer ("Banana") returns nothing natively, but a
    /// cached OCR word for that page makes it a hit.
    #[test]
    fn search_document_falls_back_to_ocr_cache() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        open_fixture(&state, "doc1");
        state.set_ocr_words("doc1", 1, vec![ocr_word("Banana")]);

        let results = search_document_impl(
            &state,
            "doc1".to_string(),
            "banana".to_string(),
            false,
            false,
            false,
        )
        .expect("search");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].page, 1);
        assert_eq!(results[0].rects.len(), 1);
        // y is flipped from the bottom-left cache rect into top-left space.
        let rect = &results[0].rects[0];
        assert!((rect.y - (200.0 - (150.0 + 12.0))).abs() < 0.1, "y: {}", rect.y);
    }

    /// A blank page (no text layer) returns no native text, so
    /// `extract_page_text` falls back to the cached OCR words.
    #[test]
    fn extract_page_text_falls_back_to_ocr_cache_on_blank_page() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);

        let mut doc = pdfium.create_new_pdf().expect("create pdf");
        doc.pages_mut()
            .create_page_at_index(
                PdfPagePaperSize::new_custom(PdfPoints::new(200.0), PdfPoints::new(200.0)),
                0 as PdfPageIndex,
            )
            .expect("create blank page");
        state
            .insert_document(
                "blank".to_string(),
                DocEntry {
                    document: doc,
                    file_path: "blank.pdf".to_string(),
                },
            )
            .expect("insert");

        // Without OCR, a blank page extracts to nothing.
        let before = extract_page_text_impl(&state, "blank".to_string(), 1).expect("extract");
        assert!(before.is_empty(), "blank page should have no native text");

        state.set_ocr_words("blank", 1, vec![ocr_word("Scanned")]);

        let after = extract_page_text_impl(&state, "blank".to_string(), 1).expect("extract");
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].text, "Scanned");
        // Flipped from bottom-left (y=150,h=12) into top-left space.
        assert!((after[0].y - (200.0 - (150.0 + 12.0))).abs() < 0.1, "y: {}", after[0].y);
    }

    /// Minimal OCR engine for export tests: returns fixed pixel-space words.
    struct FakeOcrEngine {
        words: Vec<OcrWord>,
    }
    impl OcrEngine for FakeOcrEngine {
        fn recognize(&self, _rgba: &[u8], _w: u32, _h: u32) -> Result<Vec<OcrWord>, AppError> {
            Ok(self.words.clone())
        }
    }

    fn no_progress(_page: u32, _total: u32) {}

    /// Inserts an `n`-page blank document (no text layer) under `doc_id`.
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
                },
            )
            .expect("insert");
    }

    fn temp_txt(name: &str) -> String {
        std::env::temp_dir()
            .join(name)
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn export_text_uses_native_text_without_ocr() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        open_fixture(&state, "doc1");

        let entry = state.get_document("doc1").expect("get document");
        let dest = temp_txt("tumbler_export_native.txt");
        let cancel = Arc::new(AtomicBool::new(false));

        let result = export_text_impl(
            no_progress,
            entry,
            "doc1".to_string(),
            dest.clone(),
            false,
            state.ocr_engine.clone(),
            state.ocr_cache_handle(),
            cancel,
        )
        .expect("export");

        assert_eq!(result.pages, 1);
        assert_eq!(result.ocr_pages, 0);
        assert!(!result.cancelled);

        let content = std::fs::read_to_string(&dest).expect("read output");
        assert!(content.contains("--- Page 1 ---"), "missing page separator");
        assert!(content.contains("Test Fixture"), "missing native text");
        std::fs::remove_file(&dest).ok();
    }

    #[test]
    fn export_text_ocr_fills_blank_page_and_populates_cache() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        open_blank_doc(&state, "blank", 1);

        let entry = state.get_document("blank").expect("get document");
        let cache = state.ocr_cache_handle();
        let engine: Arc<dyn OcrEngine> = Arc::new(FakeOcrEngine {
            words: vec![ocr_word("Scanned")],
        });
        let dest = temp_txt("tumbler_export_ocr.txt");
        let cancel = Arc::new(AtomicBool::new(false));

        let result = export_text_impl(
            no_progress,
            entry,
            "blank".to_string(),
            dest.clone(),
            true,
            engine,
            cache.clone(),
            cancel,
        )
        .expect("export");

        assert_eq!(result.pages, 1);
        assert_eq!(result.ocr_pages, 1);

        let content = std::fs::read_to_string(&dest).expect("read output");
        assert!(content.contains("Scanned"), "OCR text missing: {content}");
        assert!(
            !content.contains("[no extractable text]"),
            "should not show placeholder when OCR found text"
        );
        // Export also primed the cache so search/copy now work for this page.
        assert!(cache_get(&cache, "blank", 1).is_some(), "cache not populated");
        std::fs::remove_file(&dest).ok();
    }

    #[test]
    fn export_text_without_ocr_keeps_placeholder_on_blank_page() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        open_blank_doc(&state, "blank", 1);

        let entry = state.get_document("blank").expect("get document");
        let dest = temp_txt("tumbler_export_no_ocr.txt");
        let cancel = Arc::new(AtomicBool::new(false));

        let result = export_text_impl(
            no_progress,
            entry,
            "blank".to_string(),
            dest.clone(),
            false,
            state.ocr_engine.clone(),
            state.ocr_cache_handle(),
            cancel,
        )
        .expect("export");

        assert_eq!(result.ocr_pages, 0);
        let content = std::fs::read_to_string(&dest).expect("read output");
        assert!(content.contains("[no extractable text]"), "missing placeholder");
        std::fs::remove_file(&dest).ok();
    }

    #[test]
    fn export_text_cancellation_stops_before_writing() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        open_blank_doc(&state, "blank", 3);

        let entry = state.get_document("blank").expect("get document");
        let dest = temp_txt("tumbler_export_cancel.txt");
        std::fs::remove_file(&dest).ok();
        // Pre-set the cancel token so the very first page check fires.
        let cancel = Arc::new(AtomicBool::new(true));

        let result = export_text_impl(
            no_progress,
            entry,
            "blank".to_string(),
            dest.clone(),
            false,
            state.ocr_engine.clone(),
            state.ocr_cache_handle(),
            cancel,
        )
        .expect("export");

        assert!(result.cancelled, "expected cancelled result");
        assert!(
            !std::path::Path::new(&dest).exists(),
            "cancelled export must not write a file"
        );
    }

    /// A blank page exported with `use_ocr=false` still uses cached OCR words
    /// (e.g. from a prior "Make Searchable") rather than re-OCRing or writing a
    /// placeholder. This is what lets the Export prompt be skipped afterward.
    #[test]
    fn export_text_uses_cached_ocr_even_without_use_ocr() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        open_blank_doc(&state, "blank", 1);
        // Simulate a prior Make Searchable having cached this page.
        state.set_ocr_words("blank", 1, vec![ocr_word("Scanned")]);

        let entry = state.get_document("blank").expect("get document");
        let dest = temp_txt("tumbler_export_cached.txt");
        let result = export_text_impl(
            no_progress,
            entry,
            "blank".to_string(),
            dest.clone(),
            false, // use_ocr = false: must still pick up the cache
            state.ocr_engine.clone(),
            state.ocr_cache_handle(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("export");

        assert_eq!(result.ocr_pages, 1);
        let content = std::fs::read_to_string(&dest).expect("read output");
        assert!(content.contains("Scanned"), "cached OCR text missing: {content}");
        assert!(!content.contains("[no extractable text]"));
        std::fs::remove_file(&dest).ok();
    }

    // ── Search mode flag tests (issue #6) ──────────────────────────────────
    // These tests call `search_document_impl` with the new `match_case`,
    // `whole_word`, and `use_regex` parameters that will be added by the
    // implementation.  They will not compile until those parameters exist —
    // that is intentional (TDD red phase).

    /// Default (case-insensitive) search finds "Test Fixture" via lowercase query.
    #[test]
    fn test_search_case_insensitive_default() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        open_fixture(&state, "doc1");

        let results = search_document_impl(
            &state,
            "doc1".to_string(),
            "test fixture".to_string(),
            false, // match_case
            false, // whole_word
            false, // use_regex
        )
        .expect("search");

        assert_eq!(results.len(), 1, "expected one page of results");
    }

    /// With match_case=true the lowercase query must not match "Test Fixture".
    #[test]
    fn test_search_match_case_rejects_wrong_case() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        open_fixture(&state, "doc1");

        let results = search_document_impl(
            &state,
            "doc1".to_string(),
            "test fixture".to_string(),
            true,  // match_case
            false, // whole_word
            false, // use_regex
        )
        .expect("search");

        assert!(
            results.is_empty(),
            "case-sensitive search should find no results for wrong case"
        );
    }

    /// With match_case=true the correctly-cased query must match.
    #[test]
    fn test_search_match_case_accepts_right_case() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        open_fixture(&state, "doc1");

        let results = search_document_impl(
            &state,
            "doc1".to_string(),
            "Test Fixture".to_string(),
            true,  // match_case
            false, // whole_word
            false, // use_regex
        )
        .expect("search");

        assert_eq!(results.len(), 1, "expected one page of results");
    }

    /// With whole_word=true a prefix substring of a word must not match.
    #[test]
    fn test_search_whole_word_rejects_substring() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        open_fixture(&state, "doc1");

        let results = search_document_impl(
            &state,
            "doc1".to_string(),
            "Te".to_string(),
            false, // match_case
            true,  // whole_word
            false, // use_regex
        )
        .expect("search");

        assert!(
            results.is_empty(),
            "whole-word search should not match a substring"
        );
    }

    /// A regex pattern matching the fixture text must return one result.
    #[test]
    fn test_search_regex_finds_pattern() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        open_fixture(&state, "doc1");

        let results = search_document_impl(
            &state,
            "doc1".to_string(),
            r"Test\s+Fixture".to_string(),
            false, // match_case
            false, // whole_word
            true,  // use_regex
        )
        .expect("search");

        assert_eq!(results.len(), 1, "regex should match the fixture text");
    }

    /// An invalid regex pattern must return Err rather than panic or silently
    /// returning empty results.
    #[test]
    fn test_search_invalid_regex_returns_err() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        open_fixture(&state, "doc1");

        let result = search_document_impl(
            &state,
            "doc1".to_string(),
            "[invalid".to_string(),
            false, // match_case
            false, // whole_word
            true,  // use_regex
        );

        assert!(result.is_err(), "invalid regex should return an error");
    }

    /// The regex path deduplicates matched strings before calling
    /// text.search(), so each unique literal is searched once.  The fixture
    /// contains two 'e' characters; with dedup, text.search("e") is called
    /// once and returns 2 rects — not 4 (which would result from calling it
    /// once per regex match without deduplication).
    #[test]
    fn test_search_regex_dedup_prevents_duplicate_rects() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        open_fixture(&state, "doc1");

        // "e" (case-sensitive) matches twice in "Test Fixture" (positions 1 and 11).
        let results = search_document_impl(
            &state,
            "doc1".to_string(),
            "e".to_string(),
            false, // match_case — regex itself is case-sensitive by default
            false, // whole_word
            true,  // use_regex
        )
        .expect("search");

        assert_eq!(results.len(), 1, "should find matches on page 1");
        assert_eq!(
            results[0].rects.len(),
            2,
            "exactly 2 rects expected (one per 'e'); got {} — dedup may be broken",
            results[0].rects.len()
        );
    }

    /// OCR regex fallback reconstructs page text from word tokens so patterns
    /// spanning multiple words (e.g. `Hello\s+World`) match correctly.
    #[test]
    fn test_search_ocr_regex_matches_across_word_tokens() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        open_fixture(&state, "doc1");
        // "Hello" and "World" are not in the native fixture text, so pdfium
        // returns no hits and the OCR fallback runs.
        state.set_ocr_words(
            "doc1",
            1,
            vec![ocr_word("Hello"), ocr_word("World")],
        );

        let results = search_document_impl(
            &state,
            "doc1".to_string(),
            r"Hello\s+World".to_string(),
            false, // match_case
            false, // whole_word
            true,  // use_regex
        )
        .expect("search");

        assert_eq!(results.len(), 1, "regex should find the cross-word OCR match");
        assert_eq!(
            results[0].rects.len(),
            2,
            "both word tokens (Hello, World) should be highlighted"
        );
    }

    /// OCR whole-word mode uses token equality (not split_whitespace) so a
    /// query does not accidentally match a longer token that merely starts
    /// with the same characters.
    #[test]
    fn test_search_ocr_whole_word_matches_exact_token() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        open_fixture(&state, "doc1");

        // Use words that are absent from the pdfium fixture ("Test Fixture")
        // so pdfium returns no hits and the OCR fallback runs.
        // "Bananasplit" is not in the fixture; querying "Banana" must NOT
        // match it under whole_word=true.
        state.set_ocr_words("doc1", 1, vec![ocr_word("Bananasplit")]);

        let no_match = search_document_impl(
            &state,
            "doc1".to_string(),
            "Banana".to_string(),
            false, // match_case
            true,  // whole_word
            false, // use_regex
        )
        .expect("search");
        assert!(
            no_match.is_empty(),
            "whole_word should not match a prefix of a longer token"
        );

        // Swap in the exact token — now it must match.
        state.set_ocr_words("doc1", 1, vec![ocr_word("Banana")]);

        let matched = search_document_impl(
            &state,
            "doc1".to_string(),
            "Banana".to_string(),
            false, // match_case
            true,  // whole_word
            false, // use_regex
        )
        .expect("search");
        assert_eq!(matched.len(), 1, "whole_word should match the exact token");
    }

    #[test]
    fn count_pages_without_text_counts_only_uncovered_pages() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        open_fixture(&state, "text");
        open_blank_doc(&state, "blank", 1);

        let text_doc = state.get_document("text").expect("get text doc");
        let blank_doc = state.get_document("blank").expect("get blank doc");

        // Native text → 0; blank, uncached → 1.
        assert_eq!(
            count_pages_without_text_impl(text_doc, "text".to_string(), state.ocr_cache_handle())
                .expect("count"),
            0
        );
        assert_eq!(
            count_pages_without_text_impl(
                blank_doc.clone(),
                "blank".to_string(),
                state.ocr_cache_handle()
            )
            .expect("count"),
            1
        );

        // Once the blank page is cached (Make Searchable), it's no longer counted.
        state.set_ocr_words("blank", 1, vec![ocr_word("Scanned")]);
        assert_eq!(
            count_pages_without_text_impl(blank_doc, "blank".to_string(), state.ocr_cache_handle())
                .expect("count"),
            0
        );
    }
}
