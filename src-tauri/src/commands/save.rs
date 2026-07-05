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
}

/// Atomically writes `bytes` to `dest_path` (temp file in the same directory,
/// then rename), so a crash or disk-full error can't leave a truncated file.
fn atomic_write(dest_path: &str, bytes: &[u8]) -> Result<(), AppError> {
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
    let _ = app.emit(
        "document-dirty-changed",
        DirtyChangedPayload { doc_id, dirty: false },
    );
    Ok(())
}

pub(crate) fn save_document_impl(state: &AppState, doc_id: &str) -> Result<(), AppError> {
    let entry_arc = state.get_document(doc_id)?;
    let mut entry = lock_mutex(&entry_arc)?;
    atomic_write(&entry.file_path, &entry.buffer)?;
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
    let _ = app.emit(
        "document-dirty-changed",
        DirtyChangedPayload { doc_id, dirty: false },
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
    atomic_write(dest_path, &entry.buffer)?;

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
