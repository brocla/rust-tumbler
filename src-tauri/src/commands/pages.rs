use crate::error::AppError;
use crate::state::{lock_mutex, AppState};
use pdfium_render::prelude::*;
use serde::Serialize;
use tauri::{Emitter, State};

#[derive(Serialize, Clone, Debug)]
pub struct PageDimension {
    pub width: f32,
    pub height: f32,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PageInfo {
    pub page_count: u32,
    pub page_dimensions: Vec<PageDimension>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct PagesChangedPayload {
    doc_ids: Vec<String>,
    page_count: u32,
    page_dimensions: Vec<PageDimension>,
}

pub(crate) fn page_info_from_doc(doc: &PdfDocument) -> Result<PageInfo, AppError> {
    let len = doc.pages().len();
    let mut dims = Vec::with_capacity(len as usize);
    for i in 0..len {
        let page = doc.pages().get(i).map_err(|e| {
            AppError::pdfium(format!("Failed to get page dimensions for index {i}"), e)
        })?;
        dims.push(PageDimension {
            width: page.width().value,
            height: page.height().value,
        });
    }
    Ok(PageInfo {
        page_count: len as u32,
        page_dimensions: dims,
    })
}

fn rotation_add_turns(
    current: PdfPageRenderRotation,
    clockwise_turns: u32,
) -> PdfPageRenderRotation {
    match (current as u32 + clockwise_turns) % 4 {
        1 => PdfPageRenderRotation::Degrees90,
        2 => PdfPageRenderRotation::Degrees180,
        3 => PdfPageRenderRotation::Degrees270,
        _ => PdfPageRenderRotation::None,
    }
}

/// Emits the pair of events every buffer-model page edit ends with: the page
/// content/layout changed for this document, and the document now has unsaved
/// changes. Also used by the compression pipeline, which rewrites every page.
pub(crate) fn emit_pages_edited(app: &tauri::AppHandle, doc_id: String, info: &PageInfo) {
    let _ = app.emit(
        "document-pages-changed",
        PagesChangedPayload {
            doc_ids: vec![doc_id.clone()],
            page_count: info.page_count,
            page_dimensions: info.page_dimensions.clone(),
        },
    );
    let _ = app.emit(
        "document-dirty-changed",
        crate::commands::save::DirtyChangedPayload { doc_id, dirty: true },
    );
}

// ── delete_pages ──────────────────────────────────────────────────────────────

#[tauri::command]
pub fn delete_pages(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    doc_id: String,
    page_numbers: Vec<u32>,
) -> Result<PageInfo, String> {
    let info =
        delete_pages_impl(&state, doc_id.clone(), page_numbers).map_err(String::from)?;
    emit_pages_edited(&app, doc_id, &info);
    Ok(info)
}

fn delete_pages_impl(
    state: &AppState,
    doc_id: String,
    page_numbers: Vec<u32>,
) -> Result<PageInfo, AppError> {
    if page_numbers.is_empty() {
        return Err(AppError::Other("No page numbers provided".to_string()));
    }

    let entry_arc = state.get_document(&doc_id)?;
    let entry = lock_mutex(&entry_arc)?;

    let page_count = entry.document.pages().len() as u32;

    let mut sorted = page_numbers.clone();
    sorted.sort_unstable();
    sorted.dedup();

    for &n in &sorted {
        if n == 0 || n > page_count {
            return Err(AppError::Other(format!(
                "Page number {n} is out of range (1..={page_count})"
            )));
        }
    }
    if sorted.len() >= page_count as usize {
        return Err(AppError::Other(
            "Cannot delete all pages from a document".to_string(),
        ));
    }

    // Delete in descending order so lower-indexed pages keep their original indices.
    for n in sorted.iter().rev().copied() {
        let page = entry
            .document
            .pages()
            .get((n - 1) as PdfPageIndex)
            .map_err(|e| AppError::pdfium(format!("Failed to get page {n}"), e))?;
        page.delete()
            .map_err(|e| AppError::pdfium(format!("Failed to delete page {n}"), e))?;
    }

    let info = page_info_from_doc(&entry.document)?;
    let bytes = entry
        .document
        .save_to_bytes()
        .map_err(|e| AppError::pdfium("Failed to save PDF after deletion", e))?;
    drop(entry);

    // Non-destructive (issue #31): the deletion lives only in the in-memory
    // buffer until the user saves. Nothing is written to disk here.
    state.set_buffer_and_refresh(&doc_id, bytes)?;
    Ok(info)
}

// ── rotate_pages ──────────────────────────────────────────────────────────────

#[tauri::command]
pub fn rotate_pages(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    doc_id: String,
    page_numbers: Vec<u32>,
    clockwise_turns: u32,
) -> Result<PageInfo, String> {
    let info = rotate_pages_impl(&state, doc_id.clone(), page_numbers, clockwise_turns)
        .map_err(String::from)?;
    emit_pages_edited(&app, doc_id, &info);
    Ok(info)
}

pub(crate) fn rotate_pages_impl(
    state: &AppState,
    doc_id: String,
    page_numbers: Vec<u32>,
    clockwise_turns: u32,
) -> Result<PageInfo, AppError> {
    if page_numbers.is_empty() {
        return Err(AppError::Other("No page numbers provided".to_string()));
    }

    let effective_turns = clockwise_turns % 4;

    let entry_arc = state.get_document(&doc_id)?;
    let entry = lock_mutex(&entry_arc)?;

    let page_count = entry.document.pages().len() as u32;
    for &n in &page_numbers {
        if n == 0 || n > page_count {
            return Err(AppError::Other(format!(
                "Page number {n} is out of range (1..={page_count})"
            )));
        }
    }

    if effective_turns != 0 {
        for n in &page_numbers {
            let mut page = entry
                .document
                .pages()
                .get((*n - 1) as PdfPageIndex)
                .map_err(|e| AppError::pdfium(format!("Failed to get page {n}"), e))?;
            let current = page.rotation().unwrap_or(PdfPageRenderRotation::None);
            page.set_rotation(rotation_add_turns(current, effective_turns));
        }
    }

    let info = page_info_from_doc(&entry.document)?;
    let bytes = entry
        .document
        .save_to_bytes()
        .map_err(|e| AppError::pdfium("Failed to save PDF after rotation", e))?;
    drop(entry);

    // Non-destructive (issue #31): the rotation lives only in the in-memory
    // buffer until the user saves. Nothing is written to disk here.
    state.set_buffer_and_refresh(&doc_id, bytes)?;
    Ok(info)
}

// ── reorder_pages ─────────────────────────────────────────────────────────────

#[tauri::command]
pub fn reorder_pages(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    doc_id: String,
    new_order: Vec<u32>,
) -> Result<PageInfo, String> {
    let info = reorder_pages_impl(&state, doc_id.clone(), new_order).map_err(String::from)?;
    emit_pages_edited(&app, doc_id, &info);
    Ok(info)
}

fn reorder_pages_impl(
    state: &AppState,
    doc_id: String,
    new_order: Vec<u32>,
) -> Result<PageInfo, AppError> {
    let entry_arc = state.get_document(&doc_id)?;
    let entry = lock_mutex(&entry_arc)?;

    let page_count = entry.document.pages().len() as u32;

    if new_order.len() != page_count as usize {
        return Err(AppError::Other(format!(
            "new_order has {} entries but document has {page_count} pages",
            new_order.len()
        )));
    }

    let mut seen = vec![false; page_count as usize + 1];
    for &n in &new_order {
        if n == 0 || n > page_count {
            return Err(AppError::Other(format!(
                "Page number {n} is out of range (1..={page_count})"
            )));
        }
        if seen[n as usize] {
            return Err(AppError::Other(format!(
                "Duplicate page number {n} in new_order"
            )));
        }
        seen[n as usize] = true;
    }

    let mut new_doc = state
        .pdfium
        .create_new_pdf()
        .map_err(|e| AppError::pdfium("Failed to create new PDF for reorder", e))?;

    for (dest_idx, &src_1based) in new_order.iter().enumerate() {
        new_doc
            .pages_mut()
            .copy_page_from_document(
                &entry.document,
                (src_1based - 1) as PdfPageIndex,
                dest_idx as PdfPageIndex,
            )
            .map_err(|e| {
                AppError::pdfium(
                    format!(
                        "Failed to copy page {src_1based} to position {}",
                        dest_idx + 1
                    ),
                    e,
                )
            })?;
    }

    let info = page_info_from_doc(&new_doc)?;
    let bytes = new_doc
        .save_to_bytes()
        .map_err(|e| AppError::pdfium("Failed to save reordered PDF", e))?;
    drop(entry);

    // Non-destructive (issue #31): the new page order lives only in the
    // in-memory buffer until the user saves.
    state.set_buffer_and_refresh(&doc_id, bytes)?;
    Ok(info)
}

// ── merge_document ────────────────────────────────────────────────────────────

#[tauri::command]
pub fn merge_document(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    doc_id: String,
    source_path: String,
    insert_after_page: u32,
) -> Result<PageInfo, String> {
    let info = merge_document_impl(&state, doc_id.clone(), source_path, insert_after_page)
        .map_err(String::from)?;
    emit_pages_edited(&app, doc_id, &info);
    Ok(info)
}

fn merge_document_impl(
    state: &AppState,
    doc_id: String,
    source_path: String,
    insert_after_page: u32,
) -> Result<PageInfo, AppError> {
    let src_doc = state
        .pdfium
        .load_pdf_from_file(&source_path, None)
        .map_err(|e| AppError::pdfium(format!("Failed to open source PDF: {source_path}"), e))?;

    let src_len = src_doc.pages().len();
    if src_len == 0 {
        return Err(AppError::Other("Source PDF has no pages".to_string()));
    }

    let entry_arc = state.get_document(&doc_id)?;
    let mut entry = lock_mutex(&entry_arc)?;

    let src_real = std::fs::canonicalize(&source_path)
        .unwrap_or_else(|_| std::path::PathBuf::from(&source_path));
    let dest_real = std::fs::canonicalize(&entry.file_path)
        .unwrap_or_else(|_| std::path::PathBuf::from(&entry.file_path));
    if src_real == dest_real {
        return Err(AppError::Other(
            "Cannot merge a document into itself".to_string(),
        ));
    }

    let page_count = entry.document.pages().len() as u32;
    if insert_after_page > page_count {
        return Err(AppError::Other(format!(
            "insert_after_page {insert_after_page} exceeds page count {page_count}"
        )));
    }

    entry
        .document
        .pages_mut()
        .copy_page_range_from_document(
            &src_doc,
            0..=(src_len - 1),
            insert_after_page as PdfPageIndex,
        )
        .map_err(|e| AppError::pdfium("Failed to import pages from source PDF", e))?;

    let info = page_info_from_doc(&entry.document)?;
    let bytes = entry
        .document
        .save_to_bytes()
        .map_err(|e| AppError::pdfium("Failed to save merged PDF", e))?;
    drop(entry);

    // Non-destructive (issue #31): the merged pages live only in the in-memory
    // buffer until the user saves.
    state.set_buffer_and_refresh(&doc_id, bytes)?;
    Ok(info)
}

// ── split_document ────────────────────────────────────────────────────────────

#[tauri::command]
pub fn split_document(
    state: State<'_, AppState>,
    doc_id: String,
    first_page: u32,
    last_page: u32,
    dest_path: String,
) -> Result<(), String> {
    split_document_impl(&state, doc_id, first_page, last_page, dest_path).map_err(String::from)
}

fn split_document_impl(
    state: &AppState,
    doc_id: String,
    first_page: u32,
    last_page: u32,
    dest_path: String,
) -> Result<(), AppError> {
    if first_page == 0 || first_page > last_page {
        return Err(AppError::Other(format!(
            "Invalid page range: {first_page}..={last_page}"
        )));
    }

    let entry_arc = state.get_document(&doc_id)?;
    let entry = lock_mutex(&entry_arc)?;

    let page_count = entry.document.pages().len() as u32;
    if last_page > page_count {
        return Err(AppError::Other(format!(
            "last_page {last_page} exceeds document page count {page_count}"
        )));
    }

    let mut new_doc = state
        .pdfium
        .create_new_pdf()
        .map_err(|e| AppError::pdfium("Failed to create new PDF for split", e))?;

    new_doc
        .pages_mut()
        .copy_page_range_from_document(
            &entry.document,
            (first_page - 1) as PdfPageIndex..=(last_page - 1) as PdfPageIndex,
            0,
        )
        .map_err(|e| AppError::pdfium("Failed to copy page range to new document", e))?;

    new_doc
        .save_to_file(&dest_path)
        .map_err(|e| AppError::pdfium("Failed to save split PDF", e))?;

    Ok(())
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::DocEntry;

    fn make_multi_page_doc(pdfium: &'static Pdfium) -> PdfDocument<'static> {
        let mut doc = pdfium.create_new_pdf().expect("create pdf");
        for (i, (w, h)) in [(200.0f32, 200.0f32), (300.0, 300.0), (400.0, 200.0)]
            .iter()
            .enumerate()
        {
            doc.pages_mut()
                .create_page_at_index(
                    PdfPagePaperSize::new_custom(PdfPoints::new(*w), PdfPoints::new(*h)),
                    i as PdfPageIndex,
                )
                .expect("create page");
        }
        doc
    }

    fn open_doc_in_state(state: &AppState, doc_id: &str, doc: PdfDocument<'static>, path: &str) {
        // The doc was just saved to `path`, so the file bytes are the buffer.
        let buffer = std::fs::read(path).expect("read saved doc");
        state
            .insert_document(
                doc_id.to_string(),
                DocEntry {
                    document: doc,
                    file_path: path.to_string(),
                    buffer,
                    dirty: false,
                    password: None,
                    encrypted: false,
                },
            )
            .expect("insert document");
    }

    fn tmp_path(name: &str) -> String {
        std::env::temp_dir()
            .join(name)
            .to_string_lossy()
            .into_owned()
    }

    fn save_doc(doc: &PdfDocument, path: &str) {
        doc.save_to_file(path).expect("save to file");
    }

    #[test]
    fn delete_page_reduces_count_and_preserves_others() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);

        let doc = make_multi_page_doc(pdfium);
        let path = tmp_path("tumbler_delete_test.pdf");
        save_doc(&doc, &path);
        open_doc_in_state(&state, "doc1", doc, &path);

        let disk_before = std::fs::read(&path).expect("read disk");
        let info = delete_pages_impl(&state, "doc1".to_string(), vec![2]).expect("delete page 2");

        assert_eq!(info.page_count, 2);
        // Page 1 stays (200x200), page 3 shifts to position 2 (400x200)
        assert_eq!(info.page_dimensions[0].width, 200.0);
        assert_eq!(info.page_dimensions[1].width, 400.0);

        let entry_arc = state.get_document("doc1").expect("get doc");
        let entry = lock_mutex(&entry_arc).expect("lock");
        assert_eq!(entry.document.pages().len(), 2);
        assert!(entry.dirty, "deletion is a buffer edit, so the doc must be dirty");
        drop(entry);
        assert_eq!(
            std::fs::read(&path).expect("read disk"),
            disk_before,
            "deletion must not touch the file until an explicit save"
        );

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn delete_pages_rejects_all_pages() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);

        let doc = make_multi_page_doc(pdfium);
        let path = tmp_path("tumbler_delete_all_test.pdf");
        save_doc(&doc, &path);
        open_doc_in_state(&state, "doc1", doc, &path);

        let err = delete_pages_impl(&state, "doc1".to_string(), vec![1, 2, 3])
            .expect_err("should reject deleting all pages");

        assert!(
            err.to_string().contains("Cannot delete all pages"),
            "unexpected error: {err}"
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn reorder_pages_changes_order() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);

        let doc = make_multi_page_doc(pdfium);
        let path = tmp_path("tumbler_reorder_test.pdf");
        save_doc(&doc, &path);
        open_doc_in_state(&state, "doc1", doc, &path);

        // Reverse: [3, 2, 1] → 400x200, 300x300, 200x200
        let info = reorder_pages_impl(&state, "doc1".to_string(), vec![3, 2, 1]).expect("reorder");

        assert_eq!(info.page_count, 3);
        assert_eq!(info.page_dimensions[0].width, 400.0);
        assert_eq!(info.page_dimensions[1].width, 300.0);
        assert_eq!(info.page_dimensions[2].width, 200.0);

        let entry_arc = state.get_document("doc1").expect("get doc");
        let entry = lock_mutex(&entry_arc).expect("lock");
        let p0 = entry.document.pages().get(0).expect("page 0");
        assert_eq!(p0.width().value, 400.0);
        assert!(entry.dirty, "reorder is a buffer edit, so the doc must be dirty");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn rotate_pages_sets_rotation() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);

        let doc = make_multi_page_doc(pdfium);
        let path = tmp_path("tumbler_rotate_test.pdf");
        save_doc(&doc, &path);
        open_doc_in_state(&state, "doc1", doc, &path);

        rotate_pages_impl(&state, "doc1".to_string(), vec![1], 1).expect("rotate 90° CW");

        let entry_arc = state.get_document("doc1").expect("get doc");
        let entry = lock_mutex(&entry_arc).expect("lock");
        let p0 = entry.document.pages().get(0).expect("page 0");
        assert_eq!(
            p0.rotation().unwrap_or(PdfPageRenderRotation::None),
            PdfPageRenderRotation::Degrees90
        );

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn split_document_creates_page_range() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);

        let doc = make_multi_page_doc(pdfium);
        let src_path = tmp_path("tumbler_split_src.pdf");
        let dest_path = tmp_path("tumbler_split_dest.pdf");
        save_doc(&doc, &src_path);
        open_doc_in_state(&state, "doc1", doc, &src_path);

        split_document_impl(&state, "doc1".to_string(), 2, 3, dest_path.clone())
            .expect("split pages 2-3");

        let split_doc = pdfium
            .load_pdf_from_file(&dest_path, None)
            .expect("load split doc");
        assert_eq!(split_doc.pages().len(), 2);
        // Original page 2 was 300x300
        let p0 = split_doc.pages().get(0).expect("page 0");
        assert_eq!(p0.width().value, 300.0);

        std::fs::remove_file(&src_path).ok();
        std::fs::remove_file(&dest_path).ok();
    }

    #[test]
    fn merge_document_appends_pages() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);

        let doc = make_multi_page_doc(pdfium); // 3 pages
        let base_path = tmp_path("tumbler_merge_base.pdf");
        save_doc(&doc, &base_path);
        open_doc_in_state(&state, "doc1", doc, &base_path);

        let mut src = pdfium.create_new_pdf().expect("create src");
        src.pages_mut()
            .create_page_at_index(
                PdfPagePaperSize::new_custom(PdfPoints::new(100.0), PdfPoints::new(100.0)),
                0,
            )
            .expect("create src page");
        let src_path = tmp_path("tumbler_merge_src.pdf");
        save_doc(&src, &src_path);

        let base_before = std::fs::read(&base_path).expect("read base");
        let info = merge_document_impl(
            &state,
            "doc1".to_string(),
            src_path.clone(),
            3, // append after last page
        )
        .expect("merge");

        assert_eq!(info.page_count, 4);
        assert_eq!(info.page_dimensions[3].width, 100.0);
        assert!(state.is_dirty("doc1").expect("is_dirty"));
        assert_eq!(
            std::fs::read(&base_path).expect("read base"),
            base_before,
            "merge must not touch the file until an explicit save"
        );

        std::fs::remove_file(&base_path).ok();
        std::fs::remove_file(&src_path).ok();
    }

    #[test]
    fn rotation_add_turns_wraps_correctly() {
        assert_eq!(
            rotation_add_turns(PdfPageRenderRotation::Degrees270, 1),
            PdfPageRenderRotation::None
        );
        assert_eq!(
            rotation_add_turns(PdfPageRenderRotation::None, 2),
            PdfPageRenderRotation::Degrees180
        );
        assert_eq!(
            rotation_add_turns(PdfPageRenderRotation::Degrees90, 3),
            PdfPageRenderRotation::None
        );
    }
}
