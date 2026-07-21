use crate::error::AppError;
use crate::state::{lock_mutex, AppState};
use pdfium_render::prelude::*;
use tauri::State;

#[tauri::command]
pub fn render_page(
    state: State<'_, AppState>,
    doc_id: String,
    page: u32,
    width: u32,
) -> Result<tauri::ipc::Response, String> {
    render_page_impl(&state, doc_id, page, width).map_err(String::from)
}

fn render_page_impl(
    state: &AppState,
    doc_id: String,
    page: u32,
    width: u32,
) -> Result<tauri::ipc::Response, AppError> {
    let entry = state.get_document(&doc_id)?;
    let entry = lock_mutex(&entry)?;

    let pdf_page = entry
        .document
        .pages()
        .get(page.saturating_sub(1) as i32)
        .map_err(|e| AppError::pdfium(format!("Failed to get page {page}"), e))?;

    // Render the page content and form-field values, but NOT annotation
    // appearances. Tumbler draws its own interactive overlays on top — text
    // selection, form controls, and typewriter notes (issue #99). Typewriter
    // notes are stored as FreeText annotations so they print and open correctly
    // in other readers; letting pdfium paint that appearance here too would
    // double it against the HTML overlay. (Printing keeps annotations on via
    // its own FPDF_ANNOT path, so notes still print.) Trade-off: annotation
    // markup authored elsewhere — highlights, sticky notes, stamps — is not
    // shown in the viewer, though it still prints and appears in other readers.
    let config = PdfRenderConfig::new()
        .set_target_width(width as Pixels)
        .render_annotations(false);

    let bitmap = pdf_page
        .render_with_config(&config)
        .map_err(|e| AppError::pdfium(format!("Failed to render page {page}"), e))?;

    let rgba_bytes = bitmap.as_rgba_bytes();

    Ok(tauri::ipc::Response::new(rgba_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::DocEntry;
    use tauri::ipc::{InvokeResponseBody, IpcResponse};

    fn open_fixture(state: &AppState, doc_id: &str) {
        let pdfium = crate::test_pdfium();
        let src = crate::fixture_path();
        let entry = DocEntry::load(pdfium, &src.to_string_lossy(), None).expect("load pdf");
        state.insert_document(doc_id.to_string(), entry).expect("insert");
    }

    fn raw_body_len(response: tauri::ipc::Response) -> usize {
        match response.body().expect("body") {
            InvokeResponseBody::Raw(bytes) => bytes.len(),
            other => panic!("expected raw bytes, got {other:?}"),
        }
    }

    /// `sample.pdf` is a single 200x200 page (see `commands::text` tests).
    /// Rendering it at a target width of 200 should produce an RGBA buffer
    /// sized `width * height * 4` for the resulting (square) bitmap.
    #[test]
    fn render_page_produces_rgba_buffer_of_expected_size() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        open_fixture(&state, "doc1");

        let response = render_page_impl(&state, "doc1".to_string(), 1, 200).expect("render");
        assert_eq!(raw_body_len(response), 200 * 200 * 4);
    }

    /// Rendering at a smaller target width should scale the output buffer
    /// proportionally (the fixture page is square, so width == height).
    #[test]
    fn render_page_scales_buffer_with_target_width() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        open_fixture(&state, "doc1");

        let response = render_page_impl(&state, "doc1".to_string(), 1, 100).expect("render");
        assert_eq!(raw_body_len(response), 100 * 100 * 4);
    }

    #[test]
    fn render_page_for_missing_page_is_error() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        open_fixture(&state, "doc1");

        match render_page_impl(&state, "doc1".to_string(), 99, 200) {
            Err(AppError::Pdfium { .. }) => {}
            Err(other) => panic!("expected AppError::Pdfium, got {other:?}"),
            Ok(_) => panic!("expected an error for an out-of-range page"),
        }
    }
}
