use crate::state::AppState;
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
    let doc = state
        .pdfium
        .load_pdf_from_file(&path, None)
        .map_err(|e| format!("Failed to load PDF: {e}"))?;

    let page_count = doc.pages().len() as u32;
    let mut page_dimensions = Vec::with_capacity(page_count as usize);

    for i in 0..page_count {
        let page = doc
            .pages()
            .get(i as i32)
            .map_err(|e| format!("Failed to get page {i}: {e}"))?;
        page_dimensions.push(PageDimension {
            width: page.width().value,
            height: page.height().value,
        });
    }

    let doc_id = uuid::Uuid::new_v4().to_string();
    let entry = crate::state::DocEntry {
        document: doc,
        file_path: path,
    };

    state
        .documents
        .lock()
        .map_err(|e| format!("Lock error: {e}"))?
        .insert(doc_id.clone(), entry);

    Ok(DocInfo {
        doc_id,
        page_count,
        page_dimensions,
    })
}

#[tauri::command]
pub fn close_document(state: State<'_, AppState>, doc_id: String) -> Result<(), String> {
    state
        .documents
        .lock()
        .map_err(|e| format!("Lock error: {e}"))?
        .remove(&doc_id);
    Ok(())
}
