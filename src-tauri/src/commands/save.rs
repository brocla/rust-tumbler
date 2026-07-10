//! Save / Save As for the non-destructive editing model (issue #31).
//!
//! Edits accumulate in `DocEntry.buffer`; these are the only commands that
//! write that buffer to disk. Both clear the dirty flag and emit
//! `document-dirty-changed` so the frontend can mirror it.

use crate::error::AppError;
use crate::state::{lock_mutex, AppState};
use serde::Serialize;
use tauri::{Emitter, State};

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DirtyChangedPayload {
    pub doc_id: String,
    pub dirty: bool,
    /// Mirrors `DocEntry.linearized` (issue #3) so the frontend's existing
    /// "something about this doc changed" listener also drives the
    /// status-bar "Linearized" badge, without a separate event/round-trip.
    pub linearized: bool,
}

/// Builds a `DirtyChangedPayload` with the document's current linearized
/// state, so every call site doesn't have to look it up by hand.
pub(crate) fn dirty_changed_payload(
    state: &AppState,
    doc_id: String,
    dirty: bool,
) -> DirtyChangedPayload {
    let linearized = state.is_linearized(&doc_id);
    DirtyChangedPayload { doc_id, dirty, linearized }
}

/// The bytes Save writes to disk: the buffer as-is for an ordinary document,
/// or the buffer re-encrypted with the document's password for a
/// password-protected one — the buffer itself is plaintext (issue #57) and
/// must never reach disk unprotected unless the user removed the password.
fn bytes_for_disk(entry: &crate::state::DocEntry) -> Result<std::borrow::Cow<'_, [u8]>, AppError> {
    match &entry.protection {
        crate::state::Protection::Plaintext => Ok(std::borrow::Cow::Borrowed(&entry.buffer)),
        crate::state::Protection::Encrypted { password, permissions } => {
            let bytes = crate::commands::encryption::encrypt_with_password(
                &entry.buffer,
                password,
                *permissions,
            )?;
            Ok(std::borrow::Cow::Owned(bytes))
        }
    }
}

/// Stamps `/ModDate`, `/Producer`, and (if missing) `/CreationDate` into the
/// document's plaintext buffer and rebuilds the pdfium view from the result
/// (issue #74), so the disk file, the in-memory buffer, and the metadata panel
/// all agree after a save. Called while holding the entry lock mid-save, so it
/// updates the fields directly instead of going through
/// `set_buffer_and_refresh` (which would re-dirty the doc and fire events —
/// wrong here, since Save is about to clear dirty).
fn stamp_and_refresh_buffer(
    state: &AppState,
    entry: &mut crate::state::DocEntry,
) -> Result<(), AppError> {
    let stamped = crate::commands::metadata::stamp_save_metadata(&entry.buffer)?;
    let document = state
        .pdfium
        .load_pdf_from_byte_vec(stamped.clone(), None)
        .map_err(|e| AppError::pdfium("Failed to reload PDF after metadata stamp", e))?;
    entry.document = document;
    entry.buffer = stamped;
    Ok(())
}

/// Atomically writes `bytes` to `dest_path` (temp file in the same directory,
/// then rename), so a crash or disk-full error can't leave a truncated file.
pub(crate) fn atomic_write(dest_path: &str, bytes: &[u8]) -> Result<(), AppError> {
    let tmp_path = format!("{dest_path}.tmp-{}", uuid::Uuid::new_v4());
    if let Err(e) = std::fs::write(&tmp_path, bytes) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(AppError::io("Failed to write temporary PDF", e));
    }
    std::fs::rename(&tmp_path, dest_path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        AppError::io("Failed to replace PDF with saved copy", e)
    })
}

// ── save_document ─────────────────────────────────────────────────────────────

#[tauri::command]
pub fn save_document(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    doc_id: String,
) -> Result<(), String> {
    save_document_impl(&state, &doc_id).map_err(String::from)?;
    // Save stamps /ModDate, /Producer, and (if missing) /CreationDate into the
    // buffer (issue #74); tell any open metadata panel to refresh so those
    // fields reflect the just-saved values.
    let _ = app.emit("document-metadata-changed", vec![doc_id.clone()]);
    let _ = app.emit(
        "document-dirty-changed",
        dirty_changed_payload(&state, doc_id, false),
    );
    Ok(())
}

pub(crate) fn save_document_impl(state: &AppState, doc_id: &str) -> Result<(), AppError> {
    let entry_arc = state.get_document(doc_id)?;
    let mut entry = lock_mutex(&entry_arc)?;
    stamp_and_refresh_buffer(state, &mut entry)?;
    let bytes = bytes_for_disk(&entry)?;
    atomic_write(&entry.file_path, &bytes)?;
    entry.dirty = false;
    Ok(())
}

// ── save_document_as ──────────────────────────────────────────────────────────

/// Writes the buffer to `dest_path` and retargets the document there.
/// Returns the canonical destination path so the frontend can update the tab.
#[tauri::command]
pub fn save_document_as(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    doc_id: String,
    dest_path: String,
) -> Result<String, String> {
    let canonical = save_document_as_impl(&state, &doc_id, &dest_path).map_err(String::from)?;
    // See save_document: Save As stamps the same Info fields (issue #74).
    let _ = app.emit("document-metadata-changed", vec![doc_id.clone()]);
    let _ = app.emit(
        "document-dirty-changed",
        dirty_changed_payload(&state, doc_id, false),
    );
    Ok(canonical)
}

pub(crate) fn save_document_as_impl(
    state: &AppState,
    doc_id: &str,
    dest_path: &str,
) -> Result<String, AppError> {
    // If the destination is a file that's open in another tab, writing there
    // would break the single-instance-per-file invariant (that tab would show
    // stale content). Compare via the canonical path when the file exists.
    let dest_canonical_existing = dunce::canonicalize(dest_path)
        .map(|p| p.to_string_lossy().into_owned())
        .ok();
    if let Some(existing) = &dest_canonical_existing {
        if state.is_path_open_elsewhere(existing, doc_id)? {
            return Err(AppError::Other(
                "That file is open in another tab. Close it first or choose a different name."
                    .to_string(),
            ));
        }
    }

    let entry_arc = state.get_document(doc_id)?;
    let mut entry = lock_mutex(&entry_arc)?;
    stamp_and_refresh_buffer(state, &mut entry)?;
    let bytes = bytes_for_disk(&entry)?;
    atomic_write(dest_path, &bytes)?;

    // Canonicalize after the write (the file may not have existed before) so
    // the stored path matches what `canonicalize_path` gives the frontend.
    let canonical = dunce::canonicalize(dest_path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| dest_path.to_string());
    entry.file_path = canonical.clone();
    entry.dirty = false;
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::DocEntry;

    fn tmp_path(name: &str) -> String {
        std::env::temp_dir().join(name).to_string_lossy().into_owned()
    }

    fn open_fixture_copy(state: &AppState, doc_id: &str, name: &str) -> String {
        let path = tmp_path(name);
        std::fs::copy(crate::fixture_path(), &path).expect("copy fixture");
        let entry = DocEntry::load(state.pdfium, &path, None).expect("load");
        state.insert_document(doc_id.to_string(), entry).expect("insert");
        path
    }

    /// A buffer edit must not touch disk; `save_document` is what commits it.
    #[test]
    fn edit_is_deferred_and_save_document_commits_it() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        let path = open_fixture_copy(&state, "doc1", "tumbler_save_test.pdf");
        let original_bytes = std::fs::read(&path).expect("read original");

        // Apply an in-memory edit (rotate page 1).
        crate::commands::pages::rotate_pages_impl(&state, "doc1".to_string(), vec![1], 1)
            .expect("rotate");

        // Dirty, buffer changed, disk byte-identical.
        assert!(state.is_dirty("doc1").expect("is_dirty"));
        let entry_arc = state.get_document("doc1").expect("get");
        {
            let entry = lock_mutex(&entry_arc).expect("lock");
            assert_ne!(entry.buffer, original_bytes, "buffer should hold the edit");
        }
        assert_eq!(
            std::fs::read(&path).expect("read"),
            original_bytes,
            "disk file must be unchanged before save"
        );

        save_document_impl(&state, "doc1").expect("save");

        assert!(!state.is_dirty("doc1").expect("is_dirty"));
        let saved = std::fs::read(&path).expect("read saved");
        assert_ne!(saved, original_bytes, "save should write the edited bytes");

        // Re-opening from disk shows the rotation.
        let reopened = pdfium.load_pdf_from_file(&path, None).expect("reopen");
        let p0 = reopened.pages().get(0).expect("page 0");
        assert_eq!(
            p0.rotation().unwrap_or(pdfium_render::prelude::PdfPageRenderRotation::None),
            pdfium_render::prelude::PdfPageRenderRotation::Degrees90
        );

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn save_document_as_writes_new_path_and_retargets() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        let src_path = open_fixture_copy(&state, "doc1", "tumbler_save_as_src.pdf");
        let original_bytes = std::fs::read(&src_path).expect("read original");
        let dest_path = tmp_path("tumbler_save_as_dest.pdf");
        std::fs::remove_file(&dest_path).ok();

        crate::commands::pages::rotate_pages_impl(&state, "doc1".to_string(), vec![1], 1)
            .expect("rotate");

        let canonical =
            save_document_as_impl(&state, "doc1", &dest_path).expect("save as");

        // Original untouched; dest holds the edit; entry retargeted; clean.
        assert_eq!(std::fs::read(&src_path).expect("read src"), original_bytes);
        assert!(std::fs::metadata(&dest_path).is_ok(), "dest should exist");
        assert!(!state.is_dirty("doc1").expect("is_dirty"));
        let entry_arc = state.get_document("doc1").expect("get");
        {
            let entry = lock_mutex(&entry_arc).expect("lock");
            assert_eq!(entry.file_path, canonical);
        }
        let reopened = pdfium.load_pdf_from_file(&dest_path, None).expect("reopen dest");
        let p0 = reopened.pages().get(0).expect("page 0");
        assert_eq!(
            p0.rotation().unwrap_or(pdfium_render::prelude::PdfPageRenderRotation::None),
            pdfium_render::prelude::PdfPageRenderRotation::Degrees90
        );

        std::fs::remove_file(&src_path).ok();
        std::fs::remove_file(&dest_path).ok();
    }

    #[test]
    fn save_document_as_rejects_path_open_in_another_tab() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        let path_a = open_fixture_copy(&state, "doc-a", "tumbler_save_as_conflict_a.pdf");
        let path_b = open_fixture_copy(&state, "doc-b", "tumbler_save_as_conflict_b.pdf");

        // Stored paths are as-loaded; canonicalize doc-b's stored path so the
        // conflict check (which canonicalizes the dest) compares like for like.
        let canonical_b = dunce::canonicalize(&path_b)
            .expect("canonicalize")
            .to_string_lossy()
            .into_owned();
        {
            let entry_arc = state.get_document("doc-b").expect("get");
            lock_mutex(&entry_arc).expect("lock").file_path = canonical_b;
        }

        let err = save_document_as_impl(&state, "doc-a", &path_b)
            .expect_err("should refuse to overwrite a file open in another tab");
        assert!(
            err.to_string().contains("open in another tab"),
            "unexpected error: {err}"
        );

        std::fs::remove_file(&path_a).ok();
        std::fs::remove_file(&path_b).ok();
    }

    /// A password-protected document is fully editable (issue #57) and Save
    /// writes a file that is still encrypted with the same password — the
    /// plaintext buffer must never reach disk while a password is set.
    #[test]
    fn save_of_encrypted_document_keeps_password_protection() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        let path = tmp_path("tumbler_save_encrypted.pdf");
        std::fs::copy(crate::encrypted_fixture_path(), &path).expect("copy fixture");
        let entry = DocEntry::load(
            pdfium,
            &path,
            Some(crate::ENCRYPTED_FIXTURE_PASSWORD),
        )
        .expect("load encrypted");
        state.insert_document("doc1".to_string(), entry).expect("insert");

        // An edit that was impossible under view-only mode (issue #12).
        crate::commands::pages::rotate_pages_impl(&state, "doc1".to_string(), vec![1], 1)
            .expect("rotate");
        save_document_impl(&state, "doc1").expect("save");

        // The saved file must reject a missing password...
        assert!(
            pdfium.load_pdf_from_file(&path, None).is_err(),
            "saved file must still require the password"
        );
        // ...and carry the edit when opened with it.
        let reopened = pdfium
            .load_pdf_from_file(&path, Some(crate::ENCRYPTED_FIXTURE_PASSWORD))
            .expect("reopen with password");
        let p0 = reopened.pages().get(0).expect("page 0");
        assert_eq!(
            p0.rotation().unwrap_or(pdfium_render::prelude::PdfPageRenderRotation::None),
            pdfium_render::prelude::PdfPageRenderRotation::Degrees90
        );

        std::fs::remove_file(&path).ok();
    }

    /// After `remove_password`, Save As writes a plaintext PDF that opens
    /// with no password and carries no `/Encrypt` (issue #57).
    #[test]
    fn save_after_remove_password_writes_plaintext() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        let path = crate::encrypted_fixture_path().to_string_lossy().into_owned();
        let entry = DocEntry::load(
            pdfium,
            &path,
            Some(crate::ENCRYPTED_FIXTURE_PASSWORD),
        )
        .expect("load encrypted");
        state.insert_document("doc1".to_string(), entry).expect("insert");

        crate::commands::encryption::remove_password_impl(&state, "doc1")
            .expect("remove password");
        let dest = tmp_path("tumbler_unlocked_plaintext.pdf");
        std::fs::remove_file(&dest).ok();
        save_document_as_impl(&state, "doc1", &dest).expect("save as");

        let saved = std::fs::read(&dest).expect("read saved");
        let doc = lopdf::Document::load_mem(&saved).expect("parse saved");
        assert!(!doc.is_encrypted(), "saved file must carry no /Encrypt");
        let reopened = pdfium.load_pdf_from_file(&dest, None).expect("open without password");
        assert_eq!(reopened.pages().len(), 1);

        std::fs::remove_file(&dest).ok();
    }

    /// The encrypt-or-not decision is total over `Protection` (issue #71):
    /// `Plaintext` writes the buffer as-is, every `Encrypted` case — including
    /// the owner-only empty password — re-encrypts.
    #[test]
    fn bytes_for_disk_encrypts_iff_protected() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let path = crate::fixture_path().to_string_lossy().into_owned();
        let mut entry = DocEntry::load(pdfium, &path, None).expect("load");

        // Plaintext → the buffer itself, byte for byte.
        let bytes = bytes_for_disk(&entry).expect("plaintext bytes");
        assert_eq!(bytes.as_ref(), entry.buffer.as_slice());

        // Encrypted (ordinary password) → carries /Encrypt.
        entry.protection = crate::state::Protection::Encrypted {
            password: "pw".to_string(),
            permissions: lopdf::Permissions::all(),
        };
        let bytes = bytes_for_disk(&entry).expect("encrypted bytes");
        let doc = lopdf::Document::load_mem(&bytes).expect("parse");
        assert!(doc.is_encrypted(), "protected save must carry /Encrypt");

        // Encrypted with the empty owner-only password → still encrypts.
        entry.protection = crate::state::Protection::Encrypted {
            password: String::new(),
            permissions: lopdf::Permissions::all(),
        };
        let bytes = bytes_for_disk(&entry).expect("owner-only bytes");
        // lopdf's loader auto-unlocks an empty user password (and drops
        // /Encrypt in the parse), so check the serialized bytes directly.
        assert!(
            bytes.windows(b"/Encrypt".len()).any(|w| w == b"/Encrypt"),
            "owner-only save must still carry /Encrypt"
        );
    }

    /// Save stamps `/ModDate` and `/Producer` into the file on disk (issue #74),
    /// and the in-memory buffer reflects the same values afterward, so the
    /// metadata panel is correct without a reload. `/ModDate` is checked via
    /// lopdf — pdfium-render reads it under the wrong Info key.
    #[test]
    fn save_stamps_moddate_and_producer_on_disk_and_in_buffer() {
        use pdfium_render::prelude::PdfDocumentMetadataTagType as Tag;
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        let path = open_fixture_copy(&state, "doc1", "tumbler_stamp_test.pdf");

        // Dirty the doc so Save actually runs.
        crate::commands::pages::rotate_pages_impl(&state, "doc1".to_string(), vec![1], 1)
            .expect("rotate");
        save_document_impl(&state, "doc1").expect("save");

        let expected_producer = format!("Tumbler {}", env!("CARGO_PKG_VERSION"));

        // /ModDate on disk (via lopdf).
        let disk = std::fs::read(&path).expect("read disk");
        let disk_doc = lopdf::Document::load_mem(&disk).expect("parse disk");
        let disk_mod = disk_doc
            .trailer
            .get(b"Info")
            .ok()
            .and_then(|o| o.as_reference().ok())
            .and_then(|id| disk_doc.get_object(id).ok())
            .and_then(|o| o.as_dict().ok())
            .and_then(|d| d.get(b"ModDate").ok())
            .and_then(|o| o.as_str().ok())
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_default();
        assert!(disk_mod.starts_with("D:"), "disk ModDate not stamped: {disk_mod}");

        // /Producer on disk (via pdfium — its key is correct).
        let reopened = pdfium.load_pdf_from_file(&path, None).expect("reopen");
        assert_eq!(
            reopened
                .metadata()
                .get(Tag::Producer)
                .map(|v| v.value().to_string())
                .unwrap_or_default(),
            expected_producer,
            "disk Producer not stamped"
        );

        // The in-memory buffer carries the same stamp (what get_metadata reads),
        // and the doc is clean after save.
        let entry_arc = state.get_document("doc1").expect("get");
        let entry = lock_mutex(&entry_arc).expect("lock");
        assert_eq!(entry.buffer, disk, "buffer must match the stamped disk bytes");
        assert!(!entry.dirty, "doc must be clean after save");
        drop(entry);

        std::fs::remove_file(&path).ok();
    }

    /// Saving to the document's *own* path via Save As is allowed (it's just
    /// Save) — the exclusion by doc_id must prevent a self-conflict.
    #[test]
    fn save_document_as_to_own_path_is_allowed() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        let path = open_fixture_copy(&state, "doc1", "tumbler_save_as_self.pdf");

        crate::commands::pages::rotate_pages_impl(&state, "doc1".to_string(), vec![1], 1)
            .expect("rotate");
        save_document_as_impl(&state, "doc1", &path).expect("save as onto own path");
        assert!(!state.is_dirty("doc1").expect("is_dirty"));

        std::fs::remove_file(&path).ok();
    }
}
