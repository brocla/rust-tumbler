use crate::error::AppError;
use crate::state::{lock_mutex, AppState};
use pdfium_render::prelude::*;
use serde::Serialize;
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
