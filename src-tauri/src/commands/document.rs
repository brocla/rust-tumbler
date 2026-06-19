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
    state.clear_ocr_cache_for_doc(&doc_id);
    state.remove_document(&doc_id).map_err(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `sample.pdf` is a single 200x200 page (see `commands::text` tests for
    /// its text content). Opening it should report that size and register
    /// the document in `state` under the returned `doc_id`.
    #[test]
    fn open_document_loads_fixture_with_page_dimensions() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        let src = crate::fixture_path();

        let info = open_document_impl(&state, src.to_string_lossy().into_owned()).expect("open");

        assert_eq!(info.page_count, 1);
        assert_eq!(info.page_dimensions.len(), 1);
        assert_eq!(info.page_dimensions[0].width, 200.0);
        assert_eq!(info.page_dimensions[0].height, 200.0);
        assert!(!info.doc_id.is_empty());

        assert!(state.get_document(&info.doc_id).is_ok());
    }

    #[test]
    fn open_document_for_missing_file_is_error() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);

        let missing = std::env::temp_dir().join("tumbler_does_not_exist.pdf");
        match open_document_impl(&state, missing.to_string_lossy().into_owned()) {
            Err(AppError::Pdfium { .. }) => {}
            Err(other) => panic!("expected AppError::Pdfium, got {other:?}"),
            Ok(_) => panic!("expected an error for a missing file"),
        }
    }
}
