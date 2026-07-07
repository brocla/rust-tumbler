use crate::commands::linearize::{Linearizer, QpdfLinearizer};
use crate::commands::ocr::{cache_get, OcrCache, OcrEngine, OcrWord, WindowsOcrEngine};
use crate::error::AppError;
use pdfium_render::prelude::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

pub struct DocEntry {
    /// pdfium render view of `buffer` — always kept in sync with it.
    pub document: PdfDocument<'static>,
    /// Where Save writes; updated by Save As.
    pub file_path: String,
    /// The authoritative current bytes of the document, including any
    /// not-yet-saved edits. (issue #31)
    ///
    /// Always **plaintext**: a password-protected file is decrypted into this
    /// buffer at load time (issue #57), so every buffer-model feature works
    /// on it unchanged. Save re-encrypts with `password` on the way to disk,
    /// so for an encrypted document these bytes never byte-match the file —
    /// `dirty` means "there are unsaved changes", not "buffer != disk".
    pub buffer: Vec<u8>,
    /// True once an in-memory edit has been applied and not yet saved.
    pub dirty: bool,
    /// The password that unlocked this document, kept in memory only (never
    /// written to disk). Save re-encrypts the buffer with it, and Print's GDI
    /// path needs it when printing the still-encrypted file on disk. `None`
    /// for unencrypted documents and after `remove_password`. (issues #12, #57)
    pub password: Option<String>,
    /// True while the document is password-protected (i.e. Save will encrypt).
    /// Mirrored to the frontend for the lock badge and the remove-password
    /// action. Cleared by `remove_password`. (issues #12, #57)
    pub encrypted: bool,
    /// The permission bits of the original file, re-applied when Save
    /// re-encrypts. `None` when `encrypted` is false.
    pub permissions: Option<lopdf::Permissions>,
}

impl DocEntry {
    /// Loads a document from `path`: the file bytes become `buffer` and the
    /// pdfium view is built from those bytes (not from the file), so render
    /// state and buffer can never diverge.
    ///
    /// `password` is supplied on a retry after a first attempt reported
    /// [`AppError::PasswordRequired`]. A password-protected file loaded without
    /// one yields `PasswordRequired`; loaded with a rejected one yields
    /// [`AppError::WrongPassword`]. (issue #12)
    ///
    /// An encrypted file's bytes are decrypted into the buffer here, so the
    /// rest of the app never sees ciphertext (issue #57).
    pub fn load(
        pdfium: &'static Pdfium,
        path: &str,
        password: Option<&str>,
    ) -> Result<Self, AppError> {
        let buffer =
            std::fs::read(path).map_err(|e| AppError::io("Failed to read PDF file", e))?;
        let document = pdfium
            .load_pdf_from_byte_vec(buffer.clone(), password)
            .map_err(|e| {
                if crate::error::is_password_error(&e) {
                    // Distinguish "no password given yet" from "you guessed wrong",
                    // so the frontend can prompt vs. re-prompt-with-error.
                    if password.is_some() {
                        AppError::WrongPassword
                    } else {
                        AppError::PasswordRequired
                    }
                } else {
                    AppError::pdfium("Failed to load PDF", e)
                }
            })?;

        // A file can be encrypted yet open with no password (owner-password-
        // only protection uses an empty user password), so ask pdfium instead
        // of inferring from `password`.
        let encrypted = document
            .permissions()
            .security_handler_revision()
            .map(|r| r != PdfSecurityHandlerRevision::Unprotected)
            .unwrap_or(password.is_some());

        if encrypted {
            // pdfium accepted the password, so lopdf gets the validated one
            // ("" for owner-only protection). The plaintext becomes the
            // buffer; the pdfium view is rebuilt from it, password-free.
            let pw = password.unwrap_or("");
            let (plaintext, permissions) =
                crate::commands::encryption::decrypt_to_plaintext(&buffer, pw)?;
            let document = pdfium
                .load_pdf_from_byte_vec(plaintext.clone(), None)
                .map_err(|e| AppError::pdfium("Failed to reload decrypted PDF", e))?;
            return Ok(Self {
                document,
                file_path: path.to_string(),
                buffer: plaintext,
                dirty: false,
                password: Some(pw.to_string()),
                encrypted: true,
                permissions: Some(permissions),
            });
        }

        Ok(Self {
            document,
            file_path: path.to_string(),
            buffer,
            dirty: false,
            password: None,
            encrypted: false,
            permissions: None,
        })
    }
}

/// A staged redacted copy of an open document (issue #1). Produced by
/// `apply_redactions`, rendered by `render_redacted_page` for the post-Apply
/// preview, and written to disk only by `save_redacted_copy`. These bytes
/// never enter `DocEntry.buffer` — that is what structurally guarantees a
/// plain Save can never overwrite the original with redacted content.
pub struct PendingRedaction {
    /// pdfium render view of `bytes`, for the preview.
    pub document: PdfDocument<'static>,
    /// The final redacted output (post re-OCR), exactly what Save As writes
    /// (modulo re-encryption for a password-protected document).
    pub bytes: Vec<u8>,
    /// True only if post-redaction verification found nothing recoverable.
    /// `save_redacted_copy` refuses when false; the preview still works so the
    /// user can inspect what leaked.
    pub verified: bool,
}

pub struct AppState {
    pub pdfium: &'static Pdfium,
    documents: Mutex<HashMap<String, Arc<Mutex<DocEntry>>>>,
    /// Staged redacted copies keyed by `doc_id` (issue #1). Same two-level
    /// locking rationale as `documents`.
    pending_redactions: Mutex<HashMap<String, Arc<Mutex<PendingRedaction>>>>,
    /// Cancel token for the current redaction run (flatten + re-OCR + verify).
    redact_job: Mutex<Option<Arc<AtomicBool>>>,
    pub startup_file: Mutex<Option<String>>,
    print_job: Mutex<Option<Arc<AtomicBool>>>,
    /// Cancel token for the current document-wide OCR run, shared by the
    /// "Make Searchable" action and Export Text's OCR pass (only one runs at a
    /// time — both are modal).
    ocr_job: Mutex<Option<Arc<AtomicBool>>>,
    /// Cancel token for the current compression run (the Compress panel's "Run"),
    /// so the modal progress overlay's Cancel button can stop it mid-pass.
    compress_job: Mutex<Option<Arc<AtomicBool>>>,
    /// OCR backend behind a trait seam so tests can inject a fake (the real
    /// `WindowsOcrEngine` needs a WinRT language pack). See `commands::ocr`.
    pub ocr_engine: Arc<dyn OcrEngine>,
    /// Recognized words per `(doc_id, page_1based)`. Lets `search_document`,
    /// `extract_page_text`, and text export fall back to OCR for image-only
    /// pages without re-running recognition. Shared (`Arc`) so the blocking
    /// export task can hold a handle without borrowing `AppState`.
    ocr_cache: OcrCache,
    /// Linearization backend behind a trait seam so tests can inject a fake
    /// (the real `QpdfLinearizer` needs qpdf.dll, absent in CI). See
    /// `commands::linearize`.
    pub linearizer: Arc<dyn Linearizer>,
}

impl AppState {
    pub fn new(pdfium: &'static Pdfium, startup_file: Option<String>) -> Self {
        Self {
            pdfium,
            documents: Mutex::new(HashMap::new()),
            pending_redactions: Mutex::new(HashMap::new()),
            redact_job: Mutex::new(None),
            startup_file: Mutex::new(startup_file),
            print_job: Mutex::new(None),
            ocr_job: Mutex::new(None),
            compress_job: Mutex::new(None),
            ocr_engine: Arc::new(WindowsOcrEngine::new()),
            ocr_cache: Arc::new(Mutex::new(HashMap::new())),
            linearizer: Arc::new(QpdfLinearizer {
                dll_path: std::path::PathBuf::from(crate::resolve_qpdf_path()),
            }),
        }
    }

    /// Replaces the OCR engine with an injected one. Test-only: production wires
    /// the real `WindowsOcrEngine` via `new`.
    #[cfg(test)]
    pub fn with_ocr_engine(mut self, engine: Arc<dyn OcrEngine>) -> Self {
        self.ocr_engine = engine;
        self
    }

    /// Replaces the linearizer with an injected one. Test-only: production wires
    /// the real `QpdfLinearizer` via `new`.
    #[cfg(test)]
    pub fn with_linearizer(mut self, linearizer: Arc<dyn Linearizer>) -> Self {
        self.linearizer = linearizer;
        self
    }

    /// A cloneable handle to the OCR cache, for code that can't borrow
    /// `AppState` (e.g. the blocking text-export task).
    pub fn ocr_cache_handle(&self) -> OcrCache {
        self.ocr_cache.clone()
    }

    /// Cached OCR words for a page, if any.
    pub fn get_ocr_words(&self, doc_id: &str, page: u32) -> Option<Vec<OcrWord>> {
        cache_get(&self.ocr_cache, doc_id, page)
    }

    #[cfg(test)]
    pub fn set_ocr_words(&self, doc_id: &str, page: u32, words: Vec<OcrWord>) {
        crate::commands::ocr::cache_set(&self.ocr_cache, doc_id, page, words);
    }

    /// Drops every cached page for a document. Called on close (and after an
    /// edit/reload) so the cache neither grows unbounded nor serves words keyed
    /// to a now-stale page layout.
    pub fn clear_ocr_cache_for_doc(&self, doc_id: &str) {
        if let Ok(mut cache) = self.ocr_cache.lock() {
            cache.retain(|(d, _), _| d != doc_id);
        }
    }

    pub fn set_ocr_job(&self, token: Arc<AtomicBool>) {
        if let Ok(mut guard) = self.ocr_job.lock() {
            *guard = Some(token);
        }
    }

    pub fn take_ocr_job(&self) -> Option<Arc<AtomicBool>> {
        self.ocr_job.lock().ok()?.take()
    }

    pub fn cancel_ocr_job(&self) {
        if let Ok(guard) = self.ocr_job.lock() {
            if let Some(token) = guard.as_ref() {
                token.store(true, Ordering::Relaxed);
            }
        }
    }

    pub fn set_compress_job(&self, token: Arc<AtomicBool>) {
        if let Ok(mut guard) = self.compress_job.lock() {
            *guard = Some(token);
        }
    }

    pub fn take_compress_job(&self) -> Option<Arc<AtomicBool>> {
        self.compress_job.lock().ok()?.take()
    }

    pub fn cancel_compress_job(&self) {
        if let Ok(guard) = self.compress_job.lock() {
            if let Some(token) = guard.as_ref() {
                token.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Returns the shared handle for an open document. The document-map lock
    /// is held only long enough to clone the `Arc`, so commands operating on
    /// different documents (e.g. different tabs) don't contend on each other.
    pub fn get_document(&self, doc_id: &str) -> Result<Arc<Mutex<DocEntry>>, AppError> {
        lock_mutex(&self.documents)?
            .get(doc_id)
            .cloned()
            .ok_or_else(|| AppError::NotFound(doc_id.to_string()))
    }

    pub fn insert_document(&self, doc_id: String, entry: DocEntry) -> Result<(), AppError> {
        lock_mutex(&self.documents)?.insert(doc_id, Arc::new(Mutex::new(entry)));
        Ok(())
    }

    pub fn remove_document(&self, doc_id: &str) -> Result<(), AppError> {
        lock_mutex(&self.documents)?.remove(doc_id);
        self.clear_pending_redaction(doc_id);
        Ok(())
    }

    /// Stages (or replaces) the redacted copy for a document.
    pub fn set_pending_redaction(
        &self,
        doc_id: &str,
        pending: PendingRedaction,
    ) -> Result<(), AppError> {
        lock_mutex(&self.pending_redactions)?
            .insert(doc_id.to_string(), Arc::new(Mutex::new(pending)));
        Ok(())
    }

    /// The staged redacted copy for a document, if any.
    pub fn get_pending_redaction(&self, doc_id: &str) -> Option<Arc<Mutex<PendingRedaction>>> {
        lock_mutex(&self.pending_redactions).ok()?.get(doc_id).cloned()
    }

    /// Drops the staged redacted copy (Discard, close, or a buffer edit that
    /// makes it stale).
    pub fn clear_pending_redaction(&self, doc_id: &str) {
        if let Ok(mut map) = lock_mutex(&self.pending_redactions) {
            map.remove(doc_id);
        }
    }

    pub fn set_redact_job(&self, token: Arc<AtomicBool>) {
        if let Ok(mut guard) = self.redact_job.lock() {
            *guard = Some(token);
        }
    }

    pub fn take_redact_job(&self) -> Option<Arc<AtomicBool>> {
        self.redact_job.lock().ok()?.take()
    }

    pub fn cancel_redact_job(&self) {
        if let Ok(guard) = self.redact_job.lock() {
            if let Some(token) = guard.as_ref() {
                token.store(true, Ordering::Relaxed);
            }
        }
    }

    pub fn set_print_job(&self, token: Arc<AtomicBool>) {
        if let Ok(mut guard) = self.print_job.lock() {
            *guard = Some(token);
        }
    }

    pub fn take_print_job(&self) -> Option<Arc<AtomicBool>> {
        self.print_job.lock().ok()?.take()
    }

    pub fn cancel_print_job(&self) {
        if let Ok(guard) = self.print_job.lock() {
            if let Some(token) = guard.as_ref() {
                token.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Applies an in-memory edit: `bytes` become the document's authoritative
    /// buffer, the pdfium view is rebuilt from them, the OCR cache is dropped
    /// (page layout may have changed), and the document is marked dirty.
    /// Every buffer-model edit command ends by calling this. (issue #31)
    pub fn set_buffer_and_refresh(&self, doc_id: &str, bytes: Vec<u8>) -> Result<(), AppError> {
        let entry = self.get_document(doc_id)?;
        let document = self
            .pdfium
            .load_pdf_from_byte_vec(bytes.clone(), None)
            .map_err(|e| AppError::pdfium("Failed to reload PDF from edited bytes", e))?;
        {
            let mut e = lock_mutex(&entry)?;
            e.document = document;
            e.buffer = bytes;
            e.dirty = true;
        }
        self.clear_ocr_cache_for_doc(doc_id);
        // A staged redacted copy was built from the pre-edit buffer; it no
        // longer reflects the document, so drop it rather than let the user
        // save a stale redaction.
        self.clear_pending_redaction(doc_id);
        Ok(())
    }

    /// True if `file_path` is already held by an open document other than
    /// `exclude_doc_id`. Used by Save As to preserve the single-instance-
    /// per-file invariant (paths are compared as stored, i.e. canonical).
    pub fn is_path_open_elsewhere(
        &self,
        file_path: &str,
        exclude_doc_id: &str,
    ) -> Result<bool, AppError> {
        let docs = lock_mutex(&self.documents)?;
        for (doc_id, entry) in docs.iter() {
            if doc_id == exclude_doc_id {
                continue;
            }
            if lock_mutex(entry)?.file_path == file_path {
                return Ok(true);
            }
        }
        Ok(false)
    }

    #[cfg(test)]
    pub fn is_dirty(&self, doc_id: &str) -> Result<bool, AppError> {
        let entry = self.get_document(doc_id)?;
        let e = lock_mutex(&entry)?;
        Ok(e.dirty)
    }
}

/// Locks a mutex, converting a poison error into `AppError::Lock`.
pub fn lock_mutex<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, AppError> {
    mutex.lock().map_err(|e| AppError::Lock(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_print_job_sets_token_to_true() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        let token = Arc::new(AtomicBool::new(false));
        state.set_print_job(token.clone());
        state.cancel_print_job();
        assert!(token.load(Ordering::Relaxed));
    }

    #[test]
    fn take_print_job_removes_token_from_state() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        let token = Arc::new(AtomicBool::new(false));
        state.set_print_job(token.clone());
        let taken = state.take_print_job();
        assert!(taken.is_some());
        assert!(state.take_print_job().is_none());
    }

    #[test]
    fn lock_mutex_returns_guard() {
        let m = Mutex::new(42);
        let guard = lock_mutex(&m).expect("lock");
        assert_eq!(*guard, 42);
    }

    #[test]
    fn lock_mutex_reports_poison_as_app_error() {
        let m = Mutex::new(0);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = m.lock().unwrap();
            panic!("poison the mutex");
        }));

        let err = lock_mutex(&m).expect_err("expected poison error");
        assert!(matches!(err, AppError::Lock(_)));
    }

    /// Exercises `get_document`/`insert_document`/`remove_document` against a
    /// real pdfium-backed `DocEntry`, confirming an unknown `doc_id` is
    /// reported as `AppError::NotFound` both before insertion and after
    /// removal.
    #[test]
    fn document_map_get_insert_remove() {
        let src = crate::fixture_path();

        let pdfium = crate::test_pdfium();

        let state = AppState::new(pdfium, None);

        assert!(matches!(state.get_document("missing"), Err(AppError::NotFound(_))));

        let file_path = src.to_string_lossy().into_owned();
        let entry = DocEntry::load(pdfium, &file_path, None).expect("load pdf");
        state.insert_document("doc1".to_string(), entry).expect("insert");

        let entry = state.get_document("doc1").expect("get");
        assert_eq!(lock_mutex(&entry).unwrap().file_path, file_path);

        state.remove_document("doc1").expect("remove");
        assert!(matches!(state.get_document("doc1"), Err(AppError::NotFound(_))));
    }

    /// The password prompt flow (issue #12) rests on two pdfium facts: an
    /// encrypted file is rejected with `PasswordError` when no password is
    /// given (so we can prompt), and opens normally when the correct password
    /// is supplied. (The buffer itself is decrypted at load — issue #57.)
    #[test]
    fn encrypted_fixture_rejects_missing_password_and_opens_with_correct_one() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let encrypted = std::fs::read(crate::encrypted_fixture_path()).expect("read fixture");

        // No password → pdfium password error.
        let err = pdfium
            .load_pdf_from_byte_vec(encrypted.clone(), None)
            .expect_err("encrypted file must reject a missing password");
        assert!(
            matches!(
                err,
                PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError)
            ),
            "expected PasswordError, got {err:?}",
        );

        // Correct password → opens and renders, buffer untouched.
        let doc = pdfium
            .load_pdf_from_byte_vec(encrypted, Some(crate::ENCRYPTED_FIXTURE_PASSWORD))
            .expect("correct password must open the document");
        assert_eq!(doc.pages().len(), 1);
    }

    /// `DocEntry::load` seeds the buffer with the file's bytes and starts clean.
    #[test]
    fn doc_entry_load_seeds_buffer_from_file_and_is_clean() {
        let src = crate::fixture_path();
        let pdfium = crate::test_pdfium();

        let entry = DocEntry::load(pdfium, &src.to_string_lossy(), None).expect("load");

        assert_eq!(entry.buffer, std::fs::read(&src).expect("read fixture"));
        assert!(!entry.dirty);
    }

    /// `set_buffer_and_refresh` swaps the buffer, rebuilds the pdfium view
    /// from it, and marks the document dirty — the shape every buffer-model
    /// edit ends with.
    #[test]
    fn set_buffer_and_refresh_marks_dirty_and_rebuilds_document() {
        let _guard = crate::test_pdfium_guard();
        let src = crate::fixture_path();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);

        let file_path = src.to_string_lossy().into_owned();
        let entry = DocEntry::load(pdfium, &file_path, None).expect("load");
        state.insert_document("doc1".to_string(), entry).expect("insert");

        // A distinguishable two-page document as the "edited" bytes.
        let mut edited = pdfium.create_new_pdf().expect("create pdf");
        for i in 0..2 {
            edited
                .pages_mut()
                .create_page_at_index(
                    PdfPagePaperSize::new_custom(PdfPoints::new(300.0), PdfPoints::new(300.0)),
                    i,
                )
                .expect("create page");
        }
        let edited_bytes = edited.save_to_bytes().expect("save to bytes");

        state
            .set_buffer_and_refresh("doc1", edited_bytes.clone())
            .expect("set buffer");

        let entry_arc = state.get_document("doc1").expect("get");
        let entry = lock_mutex(&entry_arc).expect("lock");
        assert!(entry.dirty);
        assert_eq!(entry.buffer, edited_bytes);
        assert_eq!(entry.document.pages().len(), 2, "pdfium view must show the edit");
    }

    /// The whole point of `documents: Mutex<HashMap<String, Arc<Mutex<DocEntry>>>>`
    /// (rather than e.g. `Mutex<HashMap<String, DocEntry>>`) is that a
    /// long-running operation on one document's `Mutex<DocEntry>` must not
    /// block other tabs from getting/locking *their* documents. Hold doc-a's
    /// lock on a background thread and confirm doc-b remains immediately
    /// accessible on the main thread.
    #[test]
    fn locking_one_document_does_not_block_access_to_another() {
        let src = crate::fixture_path();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        let file_path = src.to_string_lossy().into_owned();

        for doc_id in ["doc-a", "doc-b"] {
            let entry = DocEntry::load(pdfium, &file_path, None).expect("load pdf");
            state.insert_document(doc_id.to_string(), entry).expect("insert");
        }

        let entry_a = state.get_document("doc-a").expect("get doc-a");
        let entry_b = state.get_document("doc-b").expect("get doc-b");

        // Hold doc-a's lock on another thread until the main thread says so.
        let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let holder = std::thread::spawn(move || {
            let _guard = lock_mutex(&entry_a).expect("lock doc-a");
            held_tx.send(()).expect("signal held");
            release_rx.recv().expect("wait for release");
        });

        held_rx.recv().expect("holder thread should acquire doc-a's lock");

        // doc-b must still be retrievable and lockable while doc-a is held.
        assert!(state.get_document("doc-b").is_ok());
        let guard_b = lock_mutex(&entry_b).expect("doc-b should not be blocked by doc-a's lock");
        drop(guard_b);

        release_tx.send(()).expect("signal release");
        holder.join().expect("holder thread");
    }
}
