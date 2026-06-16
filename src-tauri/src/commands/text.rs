use crate::error::AppError;
use crate::state::{lock_mutex, AppState, DocEntry};
use pdfium_render::prelude::*;
use serde::Serialize;
use std::sync::{Arc, Mutex};
use tauri::State;

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
    pub characters: u64,
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

    Ok(items)
}

#[tauri::command]
pub fn search_document(
    state: State<'_, AppState>,
    doc_id: String,
    query: String,
) -> Result<Vec<SearchResult>, String> {
    search_document_impl(&state, doc_id, query).map_err(String::from)
}

fn search_document_impl(
    state: &AppState,
    doc_id: String,
    query: String,
) -> Result<Vec<SearchResult>, AppError> {
    if query.is_empty() {
        return Ok(Vec::new());
    }

    let entry = state.get_document(&doc_id)?;
    let entry = lock_mutex(&entry)?;

    let page_count = entry.document.pages().len();
    let mut results = Vec::new();

    // Case-insensitive search (the default for PdfSearchOptions)
    let options = PdfSearchOptions::new();

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

        let search = match text.search(&query, &options) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let mut page_rects = Vec::new();

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

        if !page_rects.is_empty() {
            results.push(SearchResult {
                page: (page_idx + 1) as u32,
                rects: page_rects,
            });
        }
    }

    Ok(results)
}

#[tauri::command]
pub async fn export_text(
    state: State<'_, AppState>,
    doc_id: String,
    dest_path: String,
) -> Result<TextExportResult, String> {
    let entry = state.get_document(&doc_id).map_err(String::from)?;
    tauri::async_runtime::spawn_blocking(move || export_text_impl(entry, dest_path))
        .await
        .map_err(|e| e.to_string())?
        .map_err(String::from)
}

fn export_text_impl(
    entry: Arc<Mutex<DocEntry>>,
    dest_path: String,
) -> Result<TextExportResult, AppError> {
    let entry = lock_mutex(&entry)?;
    let page_count = entry.document.pages().len() as u32;
    let mut output = String::new();
    let mut total_chars: u64 = 0;

    for i in 0..page_count {
        let page_num = i + 1;
        let page = entry
            .document
            .pages()
            .get(i as i32)
            .map_err(|e| AppError::pdfium(format!("Failed to get page {page_num}"), e))?;
        let text = page
            .text()
            .map_err(|e| AppError::pdfium("Failed to get text", e))?;
        let content = text.all();

        if page_num > 1 {
            output.push_str("\n\n");
        }
        output.push_str(&format!("--- Page {page_num} ---\n"));
        if content.trim().is_empty() {
            output.push_str("[no extractable text]");
        } else {
            total_chars += content.len() as u64;
            output.push_str(&content);
        }
    }

    std::fs::write(&dest_path, output.as_bytes())
        .map_err(|e| AppError::io(format!("Failed to write to {dest_path}"), e))?;

    Ok(TextExportResult {
        pages: page_count,
        characters: total_chars,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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

        let results =
            search_document_impl(&state, "doc1".to_string(), "Test".to_string()).expect("search");

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

        let results = search_document_impl(&state, "doc1".to_string(), "Nonexistent".to_string())
            .expect("search");

        assert!(results.is_empty());
    }

    #[test]
    fn search_document_returns_empty_for_empty_query() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        open_fixture(&state, "doc1");

        let results =
            search_document_impl(&state, "doc1".to_string(), String::new()).expect("search");

        assert!(results.is_empty());
    }

    #[test]
    fn export_text_produces_nonempty_output_for_fixture() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        open_fixture(&state, "doc1");

        let entry = state.get_document("doc1").expect("get document");
        let dest = std::env::temp_dir().join("tumbler_export_text_test.txt");
        let dest_str = dest.to_string_lossy().into_owned();

        let result = export_text_impl(entry, dest_str).expect("export");

        assert_eq!(result.pages, 1);
        assert!(result.characters > 0, "expected characters > 0");

        let content = std::fs::read_to_string(&dest).expect("read output file");
        assert!(content.contains("--- Page 1 ---"), "missing page separator");
        assert!(!content.trim().is_empty(), "output should not be empty");

        std::fs::remove_file(&dest).ok();
    }
}
