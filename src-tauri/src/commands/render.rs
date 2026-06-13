use crate::state::AppState;
use pdfium_render::prelude::*;
use tauri::State;

#[tauri::command]
pub fn render_page(
    state: State<'_, AppState>,
    doc_id: String,
    page: u32,
    width: u32,
    #[allow(unused)]
    height: u32,
) -> Result<tauri::ipc::Response, String> {
    let docs = state
        .documents
        .lock()
        .map_err(|e| format!("Lock error: {e}"))?;

    let entry = docs
        .get(&doc_id)
        .ok_or_else(|| format!("Document not found: {doc_id}"))?;

    let pdf_page = entry
        .document
        .pages()
        .get(page.saturating_sub(1) as i32)
        .map_err(|e| format!("Failed to get page {page}: {e}"))?;

    let config = PdfRenderConfig::new()
        .set_target_width(width as Pixels);

    let bitmap = pdf_page
        .render_with_config(&config)
        .map_err(|e| format!("Failed to render page {page}: {e}"))?;

    let rgba_bytes = bitmap.as_rgba_bytes();

    Ok(tauri::ipc::Response::new(rgba_bytes))
}
