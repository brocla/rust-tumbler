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
/// info dictionary via lopdf — as an in-memory buffer edit (issue #31), so
/// nothing touches the file until the user saves. Emits
/// `document-metadata-changed` with the edited doc_id so any open metadata
/// panel refreshes, and `document-dirty-changed` for the Save UX.
#[tauri::command]
pub fn set_metadata(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    doc_id: String,
    metadata: MetadataUpdate,
) -> Result<DocumentMetadata, String> {
    let result = set_metadata_impl(&state, doc_id.clone(), metadata).map_err(String::from)?;
    let _ = app.emit("document-metadata-changed", vec![doc_id.clone()]);
    let _ = app.emit(
        "document-dirty-changed",
        crate::commands::save::DirtyChangedPayload { doc_id, dirty: true },
    );
    Ok(result)
}

fn set_metadata_impl(
    state: &AppState,
    doc_id: String,
    metadata: MetadataUpdate,
) -> Result<DocumentMetadata, AppError> {
    let entry = state.get_document(&doc_id)?;
    let (buffer, producer, creation_date, mod_date) = {
        let entry = lock_mutex(&entry)?;
        let meta = entry.document.metadata();
        (
            entry.buffer.clone(),
            read_meta_tag(&meta, PdfDocumentMetadataTagType::Producer),
            read_meta_tag(&meta, PdfDocumentMetadataTagType::CreationDate),
            read_meta_tag(&meta, PdfDocumentMetadataTagType::ModificationDate),
        )
    };

    let edited = write_metadata(&buffer, &metadata)?;
    state.set_buffer_and_refresh(&doc_id, edited)?;

    // `write_metadata` only touches Title/Author/Subject/Keywords/Creator, so
    // the result can be built from the values just written plus the
    // producer/dates read above.
    Ok(DocumentMetadata {
        title: metadata.title,
        author: metadata.author,
        subject: metadata.subject,
        keywords: metadata.keywords,
        creator: metadata.creator,
        producer,
        creation_date,
        mod_date,
    })
}

/// Writes Title, Author, Subject, Keywords, and Creator into the info
/// dictionary of the PDF given as `bytes`, returning the edited bytes.
fn write_metadata(bytes: &[u8], metadata: &MetadataUpdate) -> Result<Vec<u8>, AppError> {
    let mut lopdf_doc = lopdf::Document::load_mem(bytes)
        .map_err(|e| AppError::lopdf("Failed to parse PDF for metadata update", e))?;

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

    // `save_to` rewrites every object under one fresh cross-reference table, so
    // the trailer's `/Prev` (and hybrid `/XRefStm`) pointers into the original
    // file are now stale — lopdf itself rejects them on the next `load_mem`
    // ("invalid start value in Prev field"). Incrementally-updated real-world
    // PDFs carry `/Prev`, so a second edit would otherwise fail to reparse.
    lopdf_doc.trailer.remove(b"Prev");
    lopdf_doc.trailer.remove(b"XRefStm");

    let mut out = Vec::new();
    lopdf_doc
        .save_to(&mut out)
        .map_err(|e| AppError::io("Failed to serialize PDF after metadata update", e))?;
    Ok(out)
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
    use crate::state::DocEntry;

    /// Writes new metadata into a real PDF's bytes via lopdf, then confirms
    /// pdfium can still open the edited bytes and reads back the new values.
    #[test]
    fn write_metadata_round_trip_with_pdfium() {
        let bytes = std::fs::read(crate::fixture_path()).expect("read fixture");

        let update = MetadataUpdate {
            title: "Test Title".to_string(),
            author: "Test Author".to_string(),
            subject: "Test Subject".to_string(),
            keywords: "alpha, beta".to_string(),
            creator: "Tumbler".to_string(),
        };

        let edited = write_metadata(&bytes, &update).expect("write_metadata");

        let pdfium = crate::test_pdfium();
        let doc = pdfium
            .load_pdf_from_byte_vec(edited, None)
            .expect("pdfium reload");
        let meta = doc.metadata();

        assert_eq!(read_meta_tag(&meta, PdfDocumentMetadataTagType::Title), "Test Title");
        assert_eq!(read_meta_tag(&meta, PdfDocumentMetadataTagType::Author), "Test Author");
        assert_eq!(read_meta_tag(&meta, PdfDocumentMetadataTagType::Subject), "Test Subject");
        assert_eq!(read_meta_tag(&meta, PdfDocumentMetadataTagType::Keywords), "alpha, beta");
        assert_eq!(read_meta_tag(&meta, PdfDocumentMetadataTagType::Creator), "Tumbler");
    }

    /// `set_metadata_impl` builds its returned `DocumentMetadata` from the
    /// values just written plus the producer/dates read before the write.
    /// Confirms the returned result has the new editable fields and unchanged
    /// read-only fields, the edit lands in the buffer (dirty, pdfium view
    /// refreshed) and the file on disk stays untouched until an explicit save.
    #[test]
    fn set_metadata_returns_new_values_and_reloads_document_in_place() {
        let src = crate::fixture_path();

        let tmp = std::env::temp_dir().join("tumbler_set_metadata_test.pdf");
        std::fs::copy(&src, &tmp).expect("copy fixture");
        let file_path = tmp.to_string_lossy().into_owned();

        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);

        let document = pdfium
            .load_pdf_from_file(&file_path, None)
            .expect("load pdf");
        let original_meta = document.metadata();
        let original_producer = read_meta_tag(&original_meta, PdfDocumentMetadataTagType::Producer);
        let original_creation_date =
            read_meta_tag(&original_meta, PdfDocumentMetadataTagType::CreationDate);
        let original_mod_date =
            read_meta_tag(&original_meta, PdfDocumentMetadataTagType::ModificationDate);

        state
            .insert_document(
                "doc-1".to_string(),
                DocEntry {
                    document,
                    file_path: file_path.clone(),
                    buffer: std::fs::read(&file_path).expect("read pdf"),
                    dirty: false,
                },
            )
            .expect("insert");

        let update = MetadataUpdate {
            title: "New Title".to_string(),
            author: "New Author".to_string(),
            subject: "New Subject".to_string(),
            keywords: "new, keywords".to_string(),
            creator: "Tumbler".to_string(),
        };

        let disk_before = std::fs::read(&tmp).expect("read disk");
        let result = set_metadata_impl(&state, "doc-1".to_string(), update)
            .expect("set_metadata_impl");

        assert_eq!(result.title, "New Title");
        assert_eq!(result.author, "New Author");
        assert_eq!(result.subject, "New Subject");
        assert_eq!(result.keywords, "new, keywords");
        assert_eq!(result.creator, "Tumbler");
        assert_eq!(result.producer, original_producer);
        assert_eq!(result.creation_date, original_creation_date);
        assert_eq!(result.mod_date, original_mod_date);

        // The edit lands in the buffer: the pdfium view reflects it, the doc is
        // dirty, and the file on disk is byte-identical.
        let entry = state.get_document("doc-1").expect("get doc-1");
        let entry = lock_mutex(&entry).expect("lock doc-1");
        let meta = entry.document.metadata();
        assert_eq!(read_meta_tag(&meta, PdfDocumentMetadataTagType::Title), "New Title");
        assert!(entry.dirty, "metadata edit is a buffer edit, so the doc must be dirty");
        drop(entry);
        assert_eq!(
            std::fs::read(&tmp).expect("read disk"),
            disk_before,
            "metadata edit must not touch the file until an explicit save"
        );

        std::fs::remove_file(&tmp).ok();
    }

    /// Regression: incrementally-updated real-world PDFs (e.g. the IRS form
    /// f8946) carry a `/Prev` cross-reference chain. After one metadata edit,
    /// lopdf's re-serialized output must still parse on the *next* edit — it
    /// didn't until `write_metadata` dropped the stale `/Prev`/`/XRefStm`.
    #[test]
    fn consecutive_metadata_edits_survive_reparse_on_pdf_with_prev_xref() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/forms/f8946.pdf");
        let bytes = std::fs::read(&path).expect("read f8946");

        let update = |title: &str| MetadataUpdate {
            title: title.to_string(),
            author: String::new(),
            subject: String::new(),
            keywords: String::new(),
            creator: String::new(),
        };

        let once = write_metadata(&bytes, &update("First")).expect("first write");
        // The second write reparses the first write's output — the point of
        // failure before the fix.
        let twice = write_metadata(&once, &update("Second")).expect("second write must reparse");

        let pdfium = crate::test_pdfium();
        let doc = pdfium
            .load_pdf_from_byte_vec(twice, None)
            .expect("pdfium reload");
        assert_eq!(
            read_meta_tag(&doc.metadata(), PdfDocumentMetadataTagType::Title),
            "Second"
        );
    }
}
