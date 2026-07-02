use crate::error::AppError;
use crate::state::{AppState, DocEntry};
use serde::Serialize;
use tauri::State;

#[derive(Serialize)]
pub struct PageDimension {
    pub width: f32,
    pub height: f32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocInfo {
    pub doc_id: String,
    pub page_count: u32,
    pub page_dimensions: Vec<PageDimension>,
}

#[tauri::command]
pub fn open_document(state: State<'_, AppState>, path: String) -> Result<DocInfo, String> {
    open_document_impl(&state, path).map_err(String::from)
}

fn open_document_impl(state: &AppState, path: String) -> Result<DocInfo, AppError> {
    let entry = DocEntry::load(state.pdfium, &path)?;

    let page_count = entry.document.pages().len() as u32;
    let mut page_dimensions = Vec::with_capacity(page_count as usize);

    for i in 0..page_count {
        let page = entry
            .document
            .pages()
            .get(i as i32)
            .map_err(|e| AppError::pdfium(format!("Failed to get page {i}"), e))?;
        page_dimensions.push(PageDimension {
            width: page.width().value,
            height: page.height().value,
        });
    }

    let doc_id = uuid::Uuid::new_v4().to_string();
    state.insert_document(doc_id.clone(), entry)?;

    Ok(DocInfo {
        doc_id,
        page_count,
        page_dimensions,
    })
}

/// Resolves a path to its canonical form (absolute, symlinks resolved,
/// Windows case/8.3 normalized) so the frontend can compare paths for the
/// single-instance-per-file guard. `dunce` avoids the `\\?\` extended-length
/// prefix that `std::fs::canonicalize` produces on Windows.
#[tauri::command]
pub fn canonicalize_path(path: String) -> Result<String, String> {
    canonicalize_path_impl(&path).map_err(String::from)
}

fn canonicalize_path_impl(path: &str) -> Result<String, AppError> {
    let canonical = dunce::canonicalize(path)
        .map_err(|e| AppError::io(format!("Failed to canonicalize path {path:?}"), e))?;
    Ok(canonical.to_string_lossy().into_owned())
}

#[tauri::command]
pub fn close_document(state: State<'_, AppState>, doc_id: String) -> Result<(), String> {
    state.clear_ocr_cache_for_doc(&doc_id);
    state.remove_document(&doc_id).map_err(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `sample.pdf` is a single 200x200 page (see `commands::text` tests for
    /// its text content). Opening it should report that size and register
    /// the document in `state` under the returned `doc_id`.
    #[test]
    fn open_document_loads_fixture_with_page_dimensions() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        let src = crate::fixture_path();

        let info = open_document_impl(&state, src.to_string_lossy().into_owned()).expect("open");

        assert_eq!(info.page_count, 1);
        assert_eq!(info.page_dimensions.len(), 1);
        assert_eq!(info.page_dimensions[0].width, 200.0);
        assert_eq!(info.page_dimensions[0].height, 200.0);
        assert!(!info.doc_id.is_empty());

        assert!(state.get_document(&info.doc_id).is_ok());
    }

    /// The same file expressed with different case and an inserted `..`
    /// segment must canonicalize to the identical string, and the result
    /// must be a plain `C:\...` path (no `\\?\` extended-length prefix),
    /// since it is stored on tabs and shown in the UI.
    #[test]
    fn canonicalize_path_unifies_spellings_without_unc_prefix() {
        let src = crate::fixture_path();
        let direct = canonicalize_path_impl(&src.to_string_lossy()).expect("canonicalize");

        // Different case (Windows paths are case-insensitive).
        let upper = src.to_string_lossy().to_uppercase();
        let via_upper = canonicalize_path_impl(&upper).expect("canonicalize uppercase");
        assert_eq!(direct, via_upper);

        // A redundant `dir\..\dir` segment.
        let parent = src.parent().unwrap();
        let dir_name = parent.file_name().unwrap();
        let dotted = parent
            .join("..")
            .join(dir_name)
            .join(src.file_name().unwrap());
        let via_dotted = canonicalize_path_impl(&dotted.to_string_lossy()).expect("canonicalize ..");
        assert_eq!(direct, via_dotted);

        assert!(!direct.starts_with(r"\\?\"), "unexpected UNC prefix: {direct}");
    }

    #[test]
    fn canonicalize_path_for_missing_file_is_error() {
        let missing = std::env::temp_dir().join("tumbler_does_not_exist.pdf");
        assert!(canonicalize_path_impl(&missing.to_string_lossy()).is_err());
    }

    #[test]
    fn open_document_for_missing_file_is_error() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);

        let missing = std::env::temp_dir().join("tumbler_does_not_exist.pdf");
        match open_document_impl(&state, missing.to_string_lossy().into_owned()) {
            Err(AppError::Io { .. }) => {}
            Err(other) => panic!("expected AppError::Io, got {other:?}"),
            Ok(_) => panic!("expected an error for a missing file"),
        }
    }
}
