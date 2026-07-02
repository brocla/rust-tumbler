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
    /// not-yet-saved edits. Differs from the file on disk exactly when
    /// `dirty` is true. (issue #31)
    pub buffer: Vec<u8>,
    /// True once an in-memory edit has been applied and not yet saved.
    pub dirty: bool,
}

impl DocEntry {
    /// Loads a document from `path`: the file bytes become `buffer` and the
    /// pdfium view is built from those bytes (not from the file), so render
    /// state and buffer can never diverge.
    pub fn load(pdfium: &'static Pdfium, path: &str) -> Result<Self, AppError> {
        let buffer =
            std::fs::read(path).map_err(|e| AppError::io("Failed to read PDF file", e))?;
        let document = pdfium
            .load_pdf_from_byte_vec(buffer.clone(), None)
            .map_err(|e| AppError::pdfium("Failed to load PDF", e))?;
        Ok(Self {
            document,
            file_path: path.to_string(),
            buffer,
            dirty: false,
        })
    }
}

pub struct AppState {
    pub pdfium: &'static Pdfium,
    documents: Mutex<HashMap<String, Arc<Mutex<DocEntry>>>>,
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
}

impl AppState {
    pub fn new(pdfium: &'static Pdfium, startup_file: Option<String>) -> Self {
        Self {
            pdfium,
            documents: Mutex::new(HashMap::new()),
            startup_file: Mutex::new(startup_file),
            print_job: Mutex::new(None),
            ocr_job: Mutex::new(None),
            compress_job: Mutex::new(None),
            ocr_engine: Arc::new(WindowsOcrEngine::new()),
            ocr_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Replaces the OCR engine with an injected one. Test-only: production wires
    /// the real `WindowsOcrEngine` via `new`.
    #[cfg(test)]
    pub fn with_ocr_engine(mut self, engine: Arc<dyn OcrEngine>) -> Self {
        self.ocr_engine = engine;
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
        Ok(())
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

    /// Reloads every open document whose file matches `file_path` from disk.
    ///
    /// No production callers remain after issue #31 Phase 2 (all edits are
    /// buffer-based now); kept until Phase 3 decides whether external-change
    /// detection wants it. Returns the doc_ids that were reloaded.
    #[allow(dead_code)]
    pub fn reload_documents_with_path(&self, file_path: &str) -> Result<Vec<String>, AppError> {
        let matches: Vec<(String, Arc<Mutex<DocEntry>>)> = {
            let docs = lock_mutex(&self.documents)?;
            docs.iter()
                .filter_map(|(doc_id, entry)| {
                    let matches_path = lock_mutex(entry)
                        .map(|e| e.file_path == file_path)
                        .unwrap_or(false);
                    matches_path.then(|| (doc_id.clone(), entry.clone()))
                })
                .collect()
        };

        let mut reloaded_ids = Vec::with_capacity(matches.len());
        for (doc_id, entry) in matches {
            let buffer = std::fs::read(file_path)
                .map_err(|e| AppError::io("Failed to read PDF for reload", e))?;
            let document = self
                .pdfium
                .load_pdf_from_byte_vec(buffer.clone(), None)
                .map_err(|e| AppError::pdfium("Failed to reload PDF", e))?;
            {
                let mut e = lock_mutex(&entry)?;
                e.document = document;
                // Disk now defines the document again, so any unsaved buffer
                // state is superseded and the doc is clean.
                e.buffer = buffer;
                e.dirty = false;
            }
            // The page set may have changed (delete/reorder/merge), so any
            // page-keyed OCR words for this doc are now stale.
            self.clear_ocr_cache_for_doc(&doc_id);
            reloaded_ids.push(doc_id);
        }
        Ok(reloaded_ids)
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
        let entry = DocEntry::load(pdfium, &file_path).expect("load pdf");
        state.insert_document("doc1".to_string(), entry).expect("insert");

        let entry = state.get_document("doc1").expect("get");
        assert_eq!(lock_mutex(&entry).unwrap().file_path, file_path);

        state.remove_document("doc1").expect("remove");
        assert!(matches!(state.get_document("doc1"), Err(AppError::NotFound(_))));
    }

    /// Two tabs with the same file open: after the underlying file changes on
    /// disk, `reload_documents_with_path` should refresh both `DocEntry`s.
    #[test]
    fn reload_documents_with_path_refreshes_all_matching_tabs() {
        let src = crate::fixture_path();

        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        let file_path = src.to_string_lossy().into_owned();

        for doc_id in ["tab-a", "tab-b"] {
            let entry = DocEntry::load(pdfium, &file_path).expect("load pdf");
            state.insert_document(doc_id.to_string(), entry).expect("insert");
        }

        // A third tab with an unrelated file should not be touched.
        let other_path = std::env::temp_dir()
            .join("tumbler_reload_test_other.pdf")
            .to_string_lossy()
            .into_owned();
        std::fs::copy(&src, &other_path).expect("copy fixture");
        let other_entry = DocEntry::load(pdfium, &other_path).expect("load pdf");
        state
            .insert_document("tab-c".to_string(), other_entry)
            .expect("insert");

        let reloaded = state
            .reload_documents_with_path(&file_path)
            .expect("reload");

        assert_eq!(reloaded.len(), 2);
        assert!(reloaded.contains(&"tab-a".to_string()));
        assert!(reloaded.contains(&"tab-b".to_string()));

        std::fs::remove_file(&other_path).ok();
    }

    /// `DocEntry::load` seeds the buffer with the file's bytes and starts clean.
    #[test]
    fn doc_entry_load_seeds_buffer_from_file_and_is_clean() {
        let src = crate::fixture_path();
        let pdfium = crate::test_pdfium();

        let entry = DocEntry::load(pdfium, &src.to_string_lossy()).expect("load");

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
        let entry = DocEntry::load(pdfium, &file_path).expect("load");
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

    /// A disk reload supersedes any unsaved buffer state: buffer tracks the
    /// file again and the document is clean.
    #[test]
    fn reload_documents_with_path_resets_buffer_and_dirty() {
        let _guard = crate::test_pdfium_guard();
        let src = crate::fixture_path();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);

        let file_path = src.to_string_lossy().into_owned();
        let entry = DocEntry::load(pdfium, &file_path).expect("load");
        state.insert_document("doc1".to_string(), entry).expect("insert");

        // Dirty the buffer with arbitrary (still valid) bytes.
        let bytes = std::fs::read(&src).expect("read fixture");
        state.set_buffer_and_refresh("doc1", bytes).expect("set buffer");
        assert!(state.is_dirty("doc1").expect("is_dirty"));

        state.reload_documents_with_path(&file_path).expect("reload");

        let entry_arc = state.get_document("doc1").expect("get");
        let entry = lock_mutex(&entry_arc).expect("lock");
        assert!(!entry.dirty);
        assert_eq!(entry.buffer, std::fs::read(&src).expect("read fixture"));
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
            let entry = DocEntry::load(pdfium, &file_path).expect("load pdf");
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
