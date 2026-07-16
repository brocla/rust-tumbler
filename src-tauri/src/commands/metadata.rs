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
        // Read /ModDate via lopdf — pdfium-render can't (wrong Info key).
        mod_date: read_info_text(&entry.buffer, b"ModDate"),
    })
}

/// Which visible metadata-panel fields contain a redaction query — so the
/// redaction UI can enable Apply and tell the user where the keyword hit even
/// when it appears nowhere in the page text (issue #87).
#[tauri::command]
pub fn find_redaction_metadata_matches(
    state: State<'_, AppState>,
    doc_id: String,
    queries: Vec<String>,
) -> Result<Vec<String>, String> {
    find_redaction_metadata_matches_impl(&state, doc_id, queries).map_err(String::from)
}

fn find_redaction_metadata_matches_impl(
    state: &AppState,
    doc_id: String,
    queries: Vec<String>,
) -> Result<Vec<String>, AppError> {
    let entry = state.get_document(&doc_id)?;
    let entry = lock_mutex(&entry)?;
    Ok(metadata_field_matches(&entry.buffer, &queries))
}

fn read_meta_tag(meta: &PdfMetadata, tag: PdfDocumentMetadataTagType) -> String {
    meta.get(tag)
        .map(|t| t.value().to_string())
        .unwrap_or_default()
}

/// Reads a PDF text string from the Info dict of `bytes` by `key`, decoding a
/// UTF-16BE (BOM-prefixed) or a Latin-1/ASCII literal. Empty when absent or on
/// any parse failure.
///
/// Used for `/ModDate`: pdfium-render 0.9 queries the modification date under
/// the wrong Info key ("ModificationDate" instead of "ModDate"), so it can
/// never surface a real document's modification date (issue #74). lopdf reads
/// it correctly from the buffer.
fn read_info_text(bytes: &[u8], key: &[u8]) -> String {
    let Ok(doc) = lopdf::Document::load_mem(bytes) else {
        return String::new();
    };
    let info = doc
        .trailer
        .get(b"Info")
        .ok()
        .and_then(|o| o.as_reference().ok())
        .and_then(|id| doc.get_object(id).ok());
    let Some(Object::Dictionary(dict)) = info else {
        return String::new();
    };
    match dict.get(key).ok().and_then(|o| o.as_str().ok()) {
        Some(raw) => decode_pdf_text_string(raw),
        None => String::new(),
    }
}

/// The editable Info fields shown in the metadata panel. Producer and the dates
/// are read-only and are never cleared by a keyword redaction. Kept as one list
/// so the redaction *finder* (`metadata_field_matches`, here) and the redaction
/// *scrub* (`scrub_info_fields` in `redact.rs`) agree on exactly which fields a
/// keyword redaction clears.
pub const REDACTABLE_INFO_FIELDS: [&str; 5] =
    ["Title", "Author", "Subject", "Keywords", "Creator"];

/// Returns the metadata-panel field names whose value contains any of `queries`
/// (case-insensitive substring). Only the editable Info fields are considered.
/// Reads from `bytes` via lopdf so it sees the current buffer, consistent with
/// [`read_info_text`]. Used by the redaction UI to tell the user where a keyword
/// hit and to drive the surgical Info clearing (issue #87).
pub fn metadata_field_matches(bytes: &[u8], queries: &[String]) -> Vec<String> {
    let needles: Vec<String> = queries
        .iter()
        .map(|q| q.trim().to_lowercase())
        .filter(|q| !q.is_empty())
        .collect();
    if needles.is_empty() {
        return Vec::new();
    }
    REDACTABLE_INFO_FIELDS
        .iter()
        .filter(|field| {
            let value = read_info_text(bytes, field.as_bytes()).to_lowercase();
            needles.iter().any(|n| value.contains(n))
        })
        .map(|field| field.to_string())
        .collect()
}

/// Decodes the raw bytes of a PDF string object into a Rust `String`: UTF-16BE
/// when byte-order-marked, otherwise byte-for-byte as Latin-1 (the ASCII path
/// `pdf_text_string` writes for dates falls out of this unchanged).
pub(crate) fn decode_pdf_text_string(raw: &[u8]) -> String {
    if raw.len() >= 2 && raw[0] == 0xFE && raw[1] == 0xFF {
        let units: Vec<u16> = raw[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        raw.iter().map(|&b| b as char).collect()
    }
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
        crate::commands::save::dirty_changed_payload(&state, doc_id, true),
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
            // /ModDate via lopdf — pdfium-render can't read it (wrong Info key).
            read_info_text(&entry.buffer, b"ModDate"),
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

/// The current local time as a PDF date string, e.g. `D:20260710143005-04'00'`
/// (PDF 32000-1 §7.9.4). chrono's `%z` gives `-0400`; PDF wants the apostrophes.
fn pdf_date_now() -> String {
    let now = chrono::Local::now();
    let stamp = now.format("D:%Y%m%d%H%M%S%z").to_string();
    // Turn the trailing `±HHMM` into `±HH'mm'`. Any well-formed `%z` output ends
    // in five chars (sign + four digits); leave anything unexpected untouched.
    let bytes = stamp.as_bytes();
    if bytes.len() >= 5 {
        let (head, off) = stamp.split_at(stamp.len() - 5);
        if matches!(off.as_bytes()[0], b'+' | b'-') && off[1..].bytes().all(|b| b.is_ascii_digit()) {
            return format!("{head}{}{}'{}'", &off[..1], &off[1..3], &off[3..5]);
        }
    }
    stamp
}

/// Stamps the Info-dict fields Save should own (issue #74) into the PDF given
/// as `bytes`, returning the edited bytes:
/// - `/ModDate` → now (the file is being written to disk).
/// - `/Producer` → `Tumbler <version>` (the last application to write the file).
///
/// Deliberately never touches `/CreationDate` — Tumbler views and edits PDFs
/// but does not author them, so a document's creation date is not Tumbler's to
/// set, even when it's absent.
///
/// Tauri-free so Save can call it while holding the document lock, and so it's
/// unit-testable without `AppState`.
pub(crate) fn stamp_save_metadata(bytes: &[u8]) -> Result<Vec<u8>, AppError> {
    let mut lopdf_doc = lopdf::Document::load_mem(bytes)
        .map_err(|e| AppError::lopdf("Failed to parse PDF for metadata stamp", e))?;

    let info_id = lopdf_doc
        .trailer
        .get(b"Info")
        .ok()
        .and_then(|obj| obj.as_reference().ok());

    let mut info_dict = match info_id.and_then(|id| lopdf_doc.get_object(id).ok()) {
        Some(Object::Dictionary(dict)) => dict.clone(),
        _ => Dictionary::new(),
    };

    info_dict.set("ModDate", pdf_text_string(&pdf_date_now()));
    info_dict.set(
        "Producer",
        pdf_text_string(&format!("Tumbler {}", env!("CARGO_PKG_VERSION"))),
    );

    match info_id {
        Some(id) => {
            lopdf_doc.objects.insert(id, Object::Dictionary(info_dict));
        }
        None => {
            let new_id = lopdf_doc.add_object(Object::Dictionary(info_dict));
            lopdf_doc.trailer.set("Info", Object::Reference(new_id));
        }
    }

    // Same stale-xref caveat as `write_metadata`: `save_to` writes one fresh
    // cross-reference table, so the trailer's `/Prev`/`/XRefStm` pointers into
    // the original file must go or the next `load_mem` rejects them.
    lopdf_doc.trailer.remove(b"Prev");
    lopdf_doc.trailer.remove(b"XRefStm");

    let mut out = Vec::new();
    lopdf_doc
        .save_to(&mut out)
        .map_err(|e| AppError::io("Failed to serialize PDF after metadata stamp", e))?;
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

    /// `metadata_field_matches` reports which editable Info fields contain the
    /// query (case-insensitive) and ignores the read-only Producer/date fields
    /// (issue #87).
    #[test]
    fn metadata_field_matches_finds_editable_fields_case_insensitively() {
        let bytes = {
            let mut doc = lopdf::Document::load_mem(&std::fs::read(crate::fixture_path()).unwrap())
                .expect("parse fixture");
            let mut info = Dictionary::new();
            info.set("Author", Object::string_literal("Jon Worthington"));
            info.set("Title", Object::string_literal("Meeting Schedule"));
            info.set("Producer", Object::string_literal("jon's tool"));
            let info_id = doc.add_object(info);
            doc.trailer.set("Info", info_id);
            let mut out = Vec::new();
            doc.save_to(&mut out).expect("serialize");
            out
        };

        let hits = metadata_field_matches(&bytes, &["jon".to_string()]);
        assert_eq!(hits, vec!["Author".to_string()], "Author matches; Producer is ignored");

        assert!(
            metadata_field_matches(&bytes, &["schedule".to_string()]) == vec!["Title".to_string()]
        );
        assert!(metadata_field_matches(&bytes, &["absent".to_string()]).is_empty());
        assert!(metadata_field_matches(&bytes, &["   ".to_string()]).is_empty());
    }

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
                    protection: crate::state::Protection::Plaintext,
                    linearized: false,
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

    /// `pdf_date_now` produces a well-formed PDF date string:
    /// `D:YYYYMMDDHHmmSS` followed by `Z`-style or `±HH'mm'` offset.
    #[test]
    fn pdf_date_now_is_well_formed() {
        let s = pdf_date_now();
        assert!(s.starts_with("D:"), "must start with D:, got {s}");
        // D: + 14 digits = 16 chars, then a `±HH'mm'` (7 chars) offset.
        assert!(s.len() >= 16, "too short: {s}");
        assert!(s[2..16].bytes().all(|b| b.is_ascii_digit()), "non-digit date: {s}");
        let off = &s[16..];
        assert!(
            matches!(off.as_bytes().first(), Some(b'+') | Some(b'-')),
            "offset must be signed: {s}"
        );
        assert!(off.ends_with('\''), "offset must end with an apostrophe: {s}");
    }

    /// Save's stamp sets `/ModDate` and `/Producer` but never adds a
    /// `/CreationDate` — Tumbler doesn't author documents, so a blank creation
    /// date stays blank. The result still opens in pdfium.
    #[test]
    fn stamp_save_metadata_sets_moddate_and_producer_without_adding_creationdate() {
        let bytes = std::fs::read(crate::fixture_path()).expect("read fixture");

        let stamped = stamp_save_metadata(&bytes).expect("stamp");

        // /ModDate must be read via lopdf — pdfium-render uses the wrong key.
        let mod_date = read_info_text(&stamped, b"ModDate");
        assert!(mod_date.starts_with("D:"), "ModDate not stamped: {mod_date}");

        let pdfium = crate::test_pdfium();
        let doc = pdfium
            .load_pdf_from_byte_vec(stamped, None)
            .expect("pdfium reload");
        let meta = doc.metadata();

        assert_eq!(
            read_meta_tag(&meta, PdfDocumentMetadataTagType::Producer),
            format!("Tumbler {}", env!("CARGO_PKG_VERSION"))
        );
        // The fixture carries no CreationDate; the stamp must not invent one.
        assert_eq!(
            read_meta_tag(&meta, PdfDocumentMetadataTagType::CreationDate),
            "",
            "stamp must not add a CreationDate"
        );
    }

    /// An existing `/CreationDate` is left exactly as-is — Tumbler never writes
    /// the creation date, whether the document has one or not.
    #[test]
    fn stamp_save_metadata_preserves_existing_creationdate() {
        let bytes = std::fs::read(crate::fixture_path()).expect("read fixture");
        // Seed a known CreationDate first.
        let mut doc = lopdf::Document::load_mem(&bytes).expect("load");
        let info_id = doc
            .trailer
            .get(b"Info")
            .ok()
            .and_then(|o| o.as_reference().ok());
        let mut info = match info_id.and_then(|id| doc.get_object(id).ok()) {
            Some(Object::Dictionary(d)) => d.clone(),
            _ => Dictionary::new(),
        };
        info.set("CreationDate", pdf_text_string("D:20200101000000Z"));
        match info_id {
            Some(id) => {
                doc.objects.insert(id, Object::Dictionary(info));
            }
            None => {
                let id = doc.add_object(Object::Dictionary(info));
                doc.trailer.set("Info", Object::Reference(id));
            }
        }
        doc.trailer.remove(b"Prev");
        doc.trailer.remove(b"XRefStm");
        let mut seeded = Vec::new();
        doc.save_to(&mut seeded).expect("serialize seeded");

        let stamped = stamp_save_metadata(&seeded).expect("stamp");

        let pdfium = crate::test_pdfium();
        let reloaded = pdfium
            .load_pdf_from_byte_vec(stamped, None)
            .expect("pdfium reload");
        assert_eq!(
            read_meta_tag(&reloaded.metadata(), PdfDocumentMetadataTagType::CreationDate),
            "D:20200101000000Z",
            "existing CreationDate must be preserved"
        );
    }

    /// The stamp survives a second reparse on a `/Prev`-chained PDF, same as
    /// `write_metadata` (the stale-xref caveat applies to both).
    #[test]
    fn stamp_save_metadata_survives_reparse_on_pdf_with_prev_xref() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/forms/f8946.pdf");
        let bytes = std::fs::read(&path).expect("read f8946");

        let once = stamp_save_metadata(&bytes).expect("first stamp");
        let twice = stamp_save_metadata(&once).expect("second stamp must reparse");

        let pdfium = crate::test_pdfium();
        let doc = pdfium
            .load_pdf_from_byte_vec(twice, None)
            .expect("pdfium reload");
        assert_eq!(
            read_meta_tag(&doc.metadata(), PdfDocumentMetadataTagType::Producer),
            format!("Tumbler {}", env!("CARGO_PKG_VERSION"))
        );
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
