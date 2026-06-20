use crate::commands::ocr::{cache_get, cache_set, OcrCache, OcrEngine, OcrWord, WindowsOcrEngine};
use crate::error::AppError;
use pdfium_render::prelude::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

pub struct DocEntry {
    pub document: PdfDocument<'static>,
    pub file_path: String,
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
    /// Optimized PDF bytes staged by `run_optimization_steps`, awaiting an
    /// explicit "Save As..." (`save_optimized_copy`). Keyed by `doc_id`; the
    /// entry is cleared on save and when the document is closed. Nothing here
    /// touches the on-disk file until the user saves.
    pending_optimized: Mutex<HashMap<String, Vec<u8>>>,
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
            pending_optimized: Mutex::new(HashMap::new()),
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

    pub fn set_ocr_words(&self, doc_id: &str, page: u32, words: Vec<OcrWord>) {
        cache_set(&self.ocr_cache, doc_id, page, words);
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
        // Staged optimization output shouldn't outlive the document it belongs
        // to.
        self.clear_pending_optimized(doc_id);
        Ok(())
    }

    /// Stages optimized PDF bytes for `doc_id`, awaiting `save_optimized_copy`.
    /// Replaces any previously staged output for that document.
    pub fn set_pending_optimized(&self, doc_id: String, bytes: Vec<u8>) {
        if let Ok(mut pending) = self.pending_optimized.lock() {
            pending.insert(doc_id, bytes);
        }
    }

    /// A clone of the staged optimized bytes for `doc_id`, if any. Cloning lets
    /// the caller write to disk and only clear the entry on success.
    pub fn get_pending_optimized(&self, doc_id: &str) -> Option<Vec<u8>> {
        self.pending_optimized.lock().ok()?.get(doc_id).cloned()
    }

    pub fn clear_pending_optimized(&self, doc_id: &str) {
        if let Ok(mut pending) = self.pending_optimized.lock() {
            pending.remove(doc_id);
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

    /// Reloads every open document whose file matches `file_path` from disk.
    /// Used after an in-place edit (e.g. metadata) so other tabs viewing the
    /// same file pick up the change. Returns the doc_ids that were reloaded.
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
            let document = self
                .pdfium
                .load_pdf_from_file(file_path, None)
                .map_err(|e| AppError::pdfium("Failed to reload PDF", e))?;
            lock_mutex(&entry)?.document = document;
            // The page set may have changed (delete/reorder/merge), so any
            // page-keyed OCR words for this doc are now stale.
            self.clear_ocr_cache_for_doc(&doc_id);
            reloaded_ids.push(doc_id);
        }
        Ok(reloaded_ids)
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

        let document = pdfium
            .load_pdf_from_file(src.to_str().unwrap(), None)
            .expect("load pdf");
        let file_path = src.to_string_lossy().into_owned();
        state
            .insert_document(
                "doc1".to_string(),
                DocEntry {
                    document,
                    file_path: file_path.clone(),
                },
            )
            .expect("insert");

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
            let document = pdfium
                .load_pdf_from_file(&file_path, None)
                .expect("load pdf");
            state
                .insert_document(
                    doc_id.to_string(),
                    DocEntry {
                        document,
                        file_path: file_path.clone(),
                    },
                )
                .expect("insert");
        }

        // A third tab with an unrelated file should not be touched.
        let other_path = std::env::temp_dir()
            .join("tumbler_reload_test_other.pdf")
            .to_string_lossy()
            .into_owned();
        std::fs::copy(&src, &other_path).expect("copy fixture");
        let other_document = pdfium
            .load_pdf_from_file(&other_path, None)
            .expect("load pdf");
        state
            .insert_document(
                "tab-c".to_string(),
                DocEntry {
                    document: other_document,
                    file_path: other_path.clone(),
                },
            )
            .expect("insert");

        let reloaded = state
            .reload_documents_with_path(&file_path)
            .expect("reload");

        assert_eq!(reloaded.len(), 2);
        assert!(reloaded.contains(&"tab-a".to_string()));
        assert!(reloaded.contains(&"tab-b".to_string()));

        std::fs::remove_file(&other_path).ok();
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
            let document = pdfium
                .load_pdf_from_file(&file_path, None)
                .expect("load pdf");
            state
                .insert_document(
                    doc_id.to_string(),
                    DocEntry {
                        document,
                        file_path: file_path.clone(),
                    },
                )
                .expect("insert");
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
