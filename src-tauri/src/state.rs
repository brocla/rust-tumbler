use crate::error::AppError;
use pdfium_render::prelude::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

pub struct DocEntry {
    pub document: PdfDocument<'static>,
    pub file_path: String,
}

pub struct AppState {
    pub pdfium: &'static Pdfium,
    documents: Mutex<HashMap<String, Arc<Mutex<DocEntry>>>>,
    pub startup_file: Mutex<Option<String>>,
}

impl AppState {
    pub fn new(pdfium: &'static Pdfium, startup_file: Option<String>) -> Self {
        Self {
            pdfium,
            documents: Mutex::new(HashMap::new()),
            startup_file: Mutex::new(startup_file),
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
    use std::path::PathBuf;

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
        let src = PathBuf::from(std::env::var("USERPROFILE").unwrap())
            .join("AppData\\Local\\Temp\\tumbler_print.pdf");
        if !src.exists() {
            eprintln!("skipping: {} not found", src.display());
            return;
        }

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
        let src = PathBuf::from(std::env::var("USERPROFILE").unwrap())
            .join("AppData\\Local\\Temp\\tumbler_print.pdf");
        if !src.exists() {
            eprintln!("skipping: {} not found", src.display());
            return;
        }

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
}
