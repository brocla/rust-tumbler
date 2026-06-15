use crate::error::AppError;
use crate::state::{AppState, DocEntry};
use serde::Serialize;
use tauri::State;

#[derive(Serialize)]
pub struct PageDimension {
    pub width: f32,
    pub height: f32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocInfo {
    pub doc_id: String,
    pub page_count: u32,
    pub page_dimensions: Vec<PageDimension>,
}

#[tauri::command]
pub fn open_document(state: State<'_, AppState>, path: String) -> Result<DocInfo, String> {
    open_document_impl(&state, path).map_err(String::from)
}

fn open_document_impl(state: &AppState, path: String) -> Result<DocInfo, AppError> {
    let doc = state
        .pdfium
        .load_pdf_from_file(&path, None)
        .map_err(|e| AppError::pdfium("Failed to load PDF", e))?;

    let page_count = doc.pages().len() as u32;
    let mut page_dimensions = Vec::with_capacity(page_count as usize);

    for i in 0..page_count {
        let page = doc
            .pages()
            .get(i as i32)
            .map_err(|e| AppError::pdfium(format!("Failed to get page {i}"), e))?;
        page_dimensions.push(PageDimension {
            width: page.width().value,
            height: page.height().value,
        });
    }

    let doc_id = uuid::Uuid::new_v4().to_string();
    state.insert_document(
        doc_id.clone(),
        DocEntry {
            document: doc,
            file_path: path,
        },
    )?;

    Ok(DocInfo {
        doc_id,
        page_count,
        page_dimensions,
    })
}

#[tauri::command]
pub fn close_document(state: State<'_, AppState>, doc_id: String) -> Result<(), String> {
    state.remove_document(&doc_id).map_err(String::from)
}
