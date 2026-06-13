use crate::state::AppState;
use pdfium_render::prelude::*;
use serde::Serialize;
use tauri::State;

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DocumentMetadata {
    pub title: String,
    pub author: String,
    pub subject: String,
    pub keywords: String,
    pub creator: String,
    pub producer: String,
    pub creation_date: String,
    pub mod_date: String,
}

#[tauri::command]
pub fn get_metadata(
    state: State<'_, AppState>,
    doc_id: String,
) -> Result<DocumentMetadata, String> {
    let docs = state
        .documents
        .lock()
        .map_err(|e| format!("Lock error: {e}"))?;

    let entry = docs
        .get(&doc_id)
        .ok_or_else(|| format!("Document not found: {doc_id}"))?;

    let meta = entry
        .document
        .metadata();

    Ok(DocumentMetadata {
        title: read_meta_tag(&meta, PdfDocumentMetadataTagType::Title),
        author: read_meta_tag(&meta, PdfDocumentMetadataTagType::Author),
        subject: read_meta_tag(&meta, PdfDocumentMetadataTagType::Subject),
        keywords: read_meta_tag(&meta, PdfDocumentMetadataTagType::Keywords),
        creator: read_meta_tag(&meta, PdfDocumentMetadataTagType::Creator),
        producer: read_meta_tag(&meta, PdfDocumentMetadataTagType::Producer),
        creation_date: read_meta_tag(&meta, PdfDocumentMetadataTagType::CreationDate),
        mod_date: read_meta_tag(&meta, PdfDocumentMetadataTagType::ModificationDate),
    })
}

fn read_meta_tag(meta: &PdfMetadata, tag: PdfDocumentMetadataTagType) -> String {
    meta.get(tag)
        .map(|t| t.value().to_string())
        .unwrap_or_default()
}
