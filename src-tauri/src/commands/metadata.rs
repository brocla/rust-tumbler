use crate::state::{AppState, DocEntry};
use lopdf::{Dictionary, Object, StringFormat};
use pdfium_render::prelude::*;
use serde::{Deserialize, Serialize};
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

/// Writes Title, Author, Subject, Keywords, and Creator into the document's
/// info dictionary via lopdf, then reloads the file into pdfium so the
/// in-memory handle reflects the saved bytes.
#[tauri::command]
pub fn set_metadata(
    state: State<'_, AppState>,
    doc_id: String,
    metadata: MetadataUpdate,
) -> Result<DocumentMetadata, String> {
    let file_path = {
        let docs = state
            .documents
            .lock()
            .map_err(|e| format!("Lock error: {e}"))?;
        docs.get(&doc_id)
            .ok_or_else(|| format!("Document not found: {doc_id}"))?
            .file_path
            .clone()
    };

    write_metadata(&file_path, &metadata)?;

    let reloaded = state
        .pdfium
        .load_pdf_from_file(&file_path, None)
        .map_err(|e| format!("Failed to reload PDF: {e}"))?;

    let meta = reloaded.metadata();
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

    state
        .documents
        .lock()
        .map_err(|e| format!("Lock error: {e}"))?
        .insert(
            doc_id,
            DocEntry {
                document: reloaded,
                file_path,
            },
        );

    Ok(result)
}

/// Writes Title, Author, Subject, Keywords, and Creator into the info
/// dictionary of the PDF at `file_path`, in place, via lopdf.
fn write_metadata(file_path: &str, metadata: &MetadataUpdate) -> Result<(), String> {
    let mut lopdf_doc = lopdf::Document::load(file_path)
        .map_err(|e| format!("Failed to open PDF for metadata update: {e}"))?;

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

    lopdf_doc
        .save(file_path)
        .map_err(|e| format!("Failed to save PDF: {e}"))?;

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

        let bindings = Pdfium::bind_to_library(crate::resolve_pdfium_path()).expect("bind pdfium");
        let pdfium = Pdfium::new(bindings);
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
