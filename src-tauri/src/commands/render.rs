use crate::error::AppError;
use crate::state::{lock_mutex, AppState};
use pdfium_render::prelude::*;
use tauri::State;

#[tauri::command]
pub fn render_page(
    state: State<'_, AppState>,
    doc_id: String,
    page: u32,
    width: u32,
) -> Result<tauri::ipc::Response, String> {
    render_page_impl(&state, doc_id, page, width).map_err(String::from)
}

fn render_page_impl(
    state: &AppState,
    doc_id: String,
    page: u32,
    width: u32,
) -> Result<tauri::ipc::Response, AppError> {
    let entry = state.get_document(&doc_id)?;
    let entry = lock_mutex(&entry)?;

    let pdf_page = entry
        .document
        .pages()
        .get(page.saturating_sub(1) as i32)
        .map_err(|e| AppError::pdfium(format!("Failed to get page {page}"), e))?;

    let config = PdfRenderConfig::new()
        .set_target_width(width as Pixels);

    let bitmap = pdf_page
        .render_with_config(&config)
        .map_err(|e| AppError::pdfium(format!("Failed to render page {page}"), e))?;

    let rgba_bytes = bitmap.as_rgba_bytes();

    Ok(tauri::ipc::Response::new(rgba_bytes))
}
