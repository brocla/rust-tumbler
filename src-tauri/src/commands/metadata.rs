use crate::error::AppError;
use crate::state::{lock_mutex, AppState};
use lopdf::{Dictionary, Object, StringFormat};
use pdfium_render::prelude::*;
use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};

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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataUpdate {
    pub title: String,
    pub author: String,
    pub subject: String,
    pub keywords: String,
    pub creator: String,
}

#[tauri::command]
pub fn get_metadata(
    state: State<'_, AppState>,
    doc_id: String,
) -> Result<DocumentMetadata, String> {
    get_metadata_impl(&state, doc_id).map_err(String::from)
}

fn get_metadata_impl(state: &AppState, doc_id: String) -> Result<DocumentMetadata, AppError> {
    let entry = state.get_document(&doc_id)?;
    let entry = lock_mutex(&entry)?;

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

/// Writes Title, Author, Subject, Keywords, and Creator into the document's
/// info dictionary via lopdf, then reloads every open tab pointing at this
/// file so all in-memory pdfium handles reflect the saved bytes. Emits
/// `document-metadata-changed` with the reloaded doc_ids so other tabs can
/// refresh their displayed metadata.
#[tauri::command]
pub fn set_metadata(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    doc_id: String,
    metadata: MetadataUpdate,
) -> Result<DocumentMetadata, String> {
    let (result, reloaded_ids) =
        set_metadata_impl(&state, doc_id, metadata).map_err(String::from)?;
    let _ = app.emit("document-metadata-changed", &reloaded_ids);
    Ok(result)
}

fn set_metadata_impl(
    state: &AppState,
    doc_id: String,
    metadata: MetadataUpdate,
) -> Result<(DocumentMetadata, Vec<String>), AppError> {
    let entry = state.get_document(&doc_id)?;
    let file_path = lock_mutex(&entry)?.file_path.clone();

    write_metadata(&file_path, &metadata)?;

    let reloaded_ids = state.reload_documents_with_path(&file_path)?;

    let entry = state.get_document(&doc_id)?;
    let entry = lock_mutex(&entry)?;
    let meta = entry.document.metadata();
    let result = DocumentMetadata {
        title: read_meta_tag(&meta, PdfDocumentMetadataTagType::Title),
        author: read_meta_tag(&meta, PdfDocumentMetadataTagType::Author),
        subject: read_meta_tag(&meta, PdfDocumentMetadataTagType::Subject),
        keywords: read_meta_tag(&meta, PdfDocumentMetadataTagType::Keywords),
        creator: read_meta_tag(&meta, PdfDocumentMetadataTagType::Creator),
        producer: read_meta_tag(&meta, PdfDocumentMetadataTagType::Producer),
        creation_date: read_meta_tag(&meta, PdfDocumentMetadataTagType::CreationDate),
        mod_date: read_meta_tag(&meta, PdfDocumentMetadataTagType::ModificationDate),
    };

    Ok((result, reloaded_ids))
}

/// Writes Title, Author, Subject, Keywords, and Creator into the info
/// dictionary of the PDF at `file_path`, in place, via lopdf.
fn write_metadata(file_path: &str, metadata: &MetadataUpdate) -> Result<(), AppError> {
    let mut lopdf_doc = lopdf::Document::load(file_path)
        .map_err(|e| AppError::lopdf("Failed to open PDF for metadata update", e))?;

    let info_id = lopdf_doc
        .trailer
        .get(b"Info")
        .ok()
        .and_then(|obj| obj.as_reference().ok());

    let mut info_dict = match info_id.and_then(|id| lopdf_doc.get_object(id).ok()) {
        Some(Object::Dictionary(dict)) => dict.clone(),
        _ => Dictionary::new(),
    };

    info_dict.set("Title", pdf_text_string(&metadata.title));
    info_dict.set("Author", pdf_text_string(&metadata.author));
    info_dict.set("Subject", pdf_text_string(&metadata.subject));
    info_dict.set("Keywords", pdf_text_string(&metadata.keywords));
    info_dict.set("Creator", pdf_text_string(&metadata.creator));

    match info_id {
        Some(id) => {
            lopdf_doc.objects.insert(id, Object::Dictionary(info_dict));
        }
        None => {
            let new_id = lopdf_doc.add_object(Object::Dictionary(info_dict));
            lopdf_doc.trailer.set("Info", Object::Reference(new_id));
        }
    }

    // Save to a temporary file in the same directory, then atomically replace
    // the original. This avoids corrupting/truncating the user's PDF if the
    // save is interrupted partway through.
    let tmp_path = format!("{file_path}.tmp-{}", uuid::Uuid::new_v4());

    if let Err(e) = lopdf_doc.save(&tmp_path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(AppError::io("Failed to save PDF", e));
    }

    std::fs::rename(&tmp_path, file_path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        AppError::io("Failed to replace PDF with updated copy", e)
    })?;

    Ok(())
}

/// Encodes a string as a PDF text string: PDFDocEncoding-compatible literal
/// for ASCII text, or UTF-16BE with a byte-order-mark for everything else.
fn pdf_text_string(s: &str) -> Object {
    if s.is_ascii() {
        Object::String(s.as_bytes().to_vec(), StringFormat::Literal)
    } else {
        let mut bytes = vec![0xFE, 0xFF];
        for unit in s.encode_utf16() {
            bytes.push((unit >> 8) as u8);
            bytes.push((unit & 0xFF) as u8);
        }
        Object::String(bytes, StringFormat::Literal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Writes new metadata into a real PDF via lopdf, then confirms pdfium
    /// can still open the saved file and reads back the new values.
    #[test]
    fn write_metadata_round_trip_with_pdfium() {
        let src = PathBuf::from(std::env::var("USERPROFILE").unwrap())
            .join("AppData\\Local\\Temp\\tumbler_print.pdf");
        if !src.exists() {
            eprintln!("skipping: {} not found", src.display());
            return;
        }

        let tmp = std::env::temp_dir().join("tumbler_metadata_test.pdf");
        std::fs::copy(&src, &tmp).expect("copy fixture");

        let update = MetadataUpdate {
            title: "Test Title".to_string(),
            author: "Test Author".to_string(),
            subject: "Test Subject".to_string(),
            keywords: "alpha, beta".to_string(),
            creator: "Tumbler".to_string(),
        };

        write_metadata(tmp.to_str().unwrap(), &update).expect("write_metadata");

        let pdfium = crate::test_pdfium();
        let doc = pdfium
            .load_pdf_from_file(tmp.to_str().unwrap(), None)
            .expect("pdfium reload");
        let meta = doc.metadata();

        assert_eq!(read_meta_tag(&meta, PdfDocumentMetadataTagType::Title), "Test Title");
        assert_eq!(read_meta_tag(&meta, PdfDocumentMetadataTagType::Author), "Test Author");
        assert_eq!(read_meta_tag(&meta, PdfDocumentMetadataTagType::Subject), "Test Subject");
        assert_eq!(read_meta_tag(&meta, PdfDocumentMetadataTagType::Keywords), "alpha, beta");
        assert_eq!(read_meta_tag(&meta, PdfDocumentMetadataTagType::Creator), "Tumbler");

        drop(doc);
        std::fs::remove_file(&tmp).ok();
    }
}
