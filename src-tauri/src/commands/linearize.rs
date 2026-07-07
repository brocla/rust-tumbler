//! "Save Linearized Copy" — writes a linearized ("Fast Web View") copy of
//! the open document via qpdf (issue #3).
//!
//! This is an export-only feature: it never touches `DocEntry.buffer` or the
//! original file. The source is always the buffer (never `entry.file_path`),
//! because the buffer is the authoritative current bytes — it includes
//! unsaved edits and is plaintext even for a password-protected document
//! (issue #57), whereas the on-disk file may be stale or ciphertext.
//! Because the buffer is plaintext, the linearized copy is written
//! **unencrypted** — documented behavior for v1, since a web-optimized copy
//! is typically meant to be served publicly.
//!
//! Linearization must always be the last transform applied: qpdf lays out
//! the file's objects and cross-reference/hint streams to match the bytes it
//! is given, so anything that rewrites the output afterward (including a
//! plain lopdf re-save) un-linearizes it. Since every other edit (including
//! Compress) already applies to the buffer, exporting simply linearizes the
//! buffer as it stands — a prior Compress run is naturally included, with no
//! intermediate save.

use crate::error::AppError;
use crate::state::{lock_mutex, AppState};
use serde::Serialize;
use std::ffi::{c_void, CStr, CString};
use std::os::raw::{c_char, c_int};
use std::path::Path;
use tauri::State;

/// Whether `buffer` starts with a linearized PDF's marker dictionary — the
/// same fact `qpdf --check`/pdfium's `FPDFAvail_IsLinearized` surface, and
/// what the status-bar "Linearized" badge (issue #3) reflects.
///
/// A linearized file's first indirect object is a small dictionary carrying
/// `/Linearized 1` (plus `/L`, `/H`, `/O`, `/E`, `/N`), and qpdf's own docs
/// note it's discoverable from the first ~1KB (`FPDFAvail_IsLinearized`
/// needs just that much to answer). So this scans a small prefix for the
/// literal marker rather than fully parsing the document with lopdf — cheap
/// enough to call on every open and after every edit, matching how a
/// streaming viewer checks it before the rest of the file has even arrived.
pub fn buffer_is_linearized(buffer: &[u8]) -> bool {
    const SCAN_LEN: usize = 2048;
    const MARKER: &[u8] = b"/Linearized";
    let scan = &buffer[..buffer.len().min(SCAN_LEN)];
    scan.windows(MARKER.len()).any(|w| w == MARKER)
}

/// Sizes before/after linearizing, for an honest confirmation message.
/// Deliberately not a "percent reduction" like Compress's report — unlike
/// compression, linearization reorders structure and adds a hint stream, so
/// the output is often the same size or slightly larger, never a savings.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LinearizeResult {
    pub original_size: u64,
    pub linearized_size: u64,
}

/// Produces a linearized copy of a PDF at `src` and writes it to `dest`.
/// A trait seam so command logic can be tested without the real qpdf.dll
/// (absent in CI — same rationale as `OcrEngine`).
pub trait Linearizer: Send + Sync {
    fn linearize(&self, src: &Path, dest: &Path) -> Result<(), AppError>;
}

/// Test-only: copies bytes unchanged, so command logic (temp-file handoff,
/// destination guards, cleanup) can be exercised without a real qpdf.dll.
pub struct StubLinearizer;

impl Linearizer for StubLinearizer {
    fn linearize(&self, src: &Path, dest: &Path) -> Result<(), AppError> {
        std::fs::copy(src, dest).map_err(|e| AppError::io("Failed to copy PDF (stub)", e))?;
        Ok(())
    }
}

/// Production implementation, calling qpdf.dll's C API via `libloading`
/// (mirrors the raw-FFI pattern already used for pdfium in `print.rs`).
pub struct QpdfLinearizer {
    pub dll_path: std::path::PathBuf,
}

// ── qpdf C API surface (qpdf-c.h) ───────────────────────────────────────────
//
// Note: the error-polling functions named in the original scope doc
// (`qpdf_more_errors` / `qpdf_get_next_error`) do not exist in qpdf's current
// C API — error handling was reworked in qpdf 10.5. The functions below
// (`qpdf_has_error` / `qpdf_get_error` / `qpdf_get_error_full_text`) are what
// qpdf 12.x actually exports, confirmed against the shipped `qpdf-c.h` and
// the DLL's export table.

#[allow(non_camel_case_types)]
type FnInit = unsafe extern "C" fn() -> *mut c_void;
#[allow(non_camel_case_types)]
type FnCleanup = unsafe extern "C" fn(qpdf: *mut *mut c_void);
#[allow(non_camel_case_types)]
type FnHasError = unsafe extern "C" fn(qpdf: *mut c_void) -> c_int;
#[allow(non_camel_case_types)]
type FnGetError = unsafe extern "C" fn(qpdf: *mut c_void) -> *mut c_void;
#[allow(non_camel_case_types)]
type FnGetErrorFullText = unsafe extern "C" fn(qpdf: *mut c_void, e: *mut c_void) -> *const c_char;
#[allow(non_camel_case_types)]
type FnRead =
    unsafe extern "C" fn(qpdf: *mut c_void, filename: *const c_char, password: *const c_char) -> c_int;
#[allow(non_camel_case_types)]
type FnInitWrite = unsafe extern "C" fn(qpdf: *mut c_void, filename: *const c_char) -> c_int;
#[allow(non_camel_case_types)]
type FnSetLinearization = unsafe extern "C" fn(qpdf: *mut c_void, value: c_int);
#[allow(non_camel_case_types)]
type FnWrite = unsafe extern "C" fn(qpdf: *mut c_void) -> c_int;

/// The `QPDF_ERRORS` bit (`1 << 1`) that a `QPDF_ERROR_CODE` return value
/// carries when the most recent call encountered an error (as opposed to
/// merely a warning, `QPDF_WARNINGS` = `1 << 0`).
const QPDF_ERRORS: c_int = 1 << 1;

/// Calls `qpdf_cleanup` on drop. `qpdf_init` allocates a `qpdf_data` object
/// that must be freed exactly once; wrapping it in a guard means every
/// early-return via `?` in `QpdfLinearizer::linearize` still cleans up.
struct QpdfGuard {
    handle: *mut c_void,
    cleanup: FnCleanup,
}

impl Drop for QpdfGuard {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { (self.cleanup)(&mut self.handle) };
        }
    }
}

/// Builds an `AppError` from the qpdf error state, if any. `has_error` must
/// be checked after *every* qpdf call — some (like `qpdf_set_linearization`)
/// return no status code at all, and even those that do only report the
/// most recent call's outcome via the returned bits, not accumulated state.
fn check_error(
    qpdf: *mut c_void,
    has_error: &FnHasError,
    get_error: &FnGetError,
    get_error_full_text: &FnGetErrorFullText,
    context: &str,
) -> Result<(), AppError> {
    if unsafe { has_error(qpdf) } == 0 {
        return Ok(());
    }
    let err = unsafe { get_error(qpdf) };
    if err.is_null() {
        return Err(AppError::Other(format!("{context}: qpdf reported an error with no detail")));
    }
    let text = unsafe { get_error_full_text(qpdf, err) };
    let message = if text.is_null() {
        "unknown qpdf error".to_string()
    } else {
        unsafe { CStr::from_ptr(text) }.to_string_lossy().into_owned()
    };
    Err(AppError::Other(format!("{context}: {message}")))
}

impl Linearizer for QpdfLinearizer {
    fn linearize(&self, src: &Path, dest: &Path) -> Result<(), AppError> {
        let lib = unsafe { libloading::Library::new(&self.dll_path) }
            .map_err(|e| AppError::Other(format!("Failed to load qpdf.dll: {e}")))?;

        let qpdf_init: libloading::Symbol<FnInit> = unsafe { lib.get(b"qpdf_init\0") }
            .map_err(|e| AppError::Other(format!("qpdf.dll missing qpdf_init: {e}")))?;
        let qpdf_cleanup: libloading::Symbol<FnCleanup> = unsafe { lib.get(b"qpdf_cleanup\0") }
            .map_err(|e| AppError::Other(format!("qpdf.dll missing qpdf_cleanup: {e}")))?;
        let qpdf_has_error: libloading::Symbol<FnHasError> = unsafe { lib.get(b"qpdf_has_error\0") }
            .map_err(|e| AppError::Other(format!("qpdf.dll missing qpdf_has_error: {e}")))?;
        let qpdf_get_error: libloading::Symbol<FnGetError> = unsafe { lib.get(b"qpdf_get_error\0") }
            .map_err(|e| AppError::Other(format!("qpdf.dll missing qpdf_get_error: {e}")))?;
        let qpdf_get_error_full_text: libloading::Symbol<FnGetErrorFullText> =
            unsafe { lib.get(b"qpdf_get_error_full_text\0") }
                .map_err(|e| AppError::Other(format!("qpdf.dll missing qpdf_get_error_full_text: {e}")))?;
        let qpdf_read: libloading::Symbol<FnRead> = unsafe { lib.get(b"qpdf_read\0") }
            .map_err(|e| AppError::Other(format!("qpdf.dll missing qpdf_read: {e}")))?;
        let qpdf_init_write: libloading::Symbol<FnInitWrite> = unsafe { lib.get(b"qpdf_init_write\0") }
            .map_err(|e| AppError::Other(format!("qpdf.dll missing qpdf_init_write: {e}")))?;
        let qpdf_set_linearization: libloading::Symbol<FnSetLinearization> =
            unsafe { lib.get(b"qpdf_set_linearization\0") }
                .map_err(|e| AppError::Other(format!("qpdf.dll missing qpdf_set_linearization: {e}")))?;
        let qpdf_write: libloading::Symbol<FnWrite> = unsafe { lib.get(b"qpdf_write\0") }
            .map_err(|e| AppError::Other(format!("qpdf.dll missing qpdf_write: {e}")))?;

        // qpdf's narrow-char API takes UTF-8 filenames directly on Windows
        // (verified against non-ASCII paths) — no wide-string variant needed.
        let src_cstr = CString::new(src.to_string_lossy().into_owned())
            .map_err(|e| AppError::Other(format!("Source path contains a null byte: {e}")))?;
        let dest_cstr = CString::new(dest.to_string_lossy().into_owned())
            .map_err(|e| AppError::Other(format!("Destination path contains a null byte: {e}")))?;

        let handle = unsafe { qpdf_init() };
        if handle.is_null() {
            return Err(AppError::Other("qpdf_init returned a null handle".to_string()));
        }
        let guard = QpdfGuard {
            handle,
            cleanup: *qpdf_cleanup,
        };

        let read_result = unsafe { qpdf_read(guard.handle, src_cstr.as_ptr(), std::ptr::null()) };
        if read_result & QPDF_ERRORS != 0 {
            check_error(
                guard.handle,
                &qpdf_has_error,
                &qpdf_get_error,
                &qpdf_get_error_full_text,
                "Failed to read source PDF",
            )?;
        }

        let write_init_result = unsafe { qpdf_init_write(guard.handle, dest_cstr.as_ptr()) };
        if write_init_result & QPDF_ERRORS != 0 {
            check_error(
                guard.handle,
                &qpdf_has_error,
                &qpdf_get_error,
                &qpdf_get_error_full_text,
                "Failed to initialize output PDF",
            )?;
        }

        unsafe { qpdf_set_linearization(guard.handle, 1) };
        check_error(
            guard.handle,
            &qpdf_has_error,
            &qpdf_get_error,
            &qpdf_get_error_full_text,
            "Failed to enable linearization",
        )?;

        let write_result = unsafe { qpdf_write(guard.handle) };
        if write_result & QPDF_ERRORS != 0 {
            check_error(
                guard.handle,
                &qpdf_has_error,
                &qpdf_get_error,
                &qpdf_get_error_full_text,
                "Failed to write linearized PDF",
            )?;
        }

        Ok(())
    }
}

// ── export_linearized_copy ──────────────────────────────────────────────────

#[tauri::command]
pub fn export_linearized_copy(
    state: State<'_, AppState>,
    doc_id: String,
    dest_path: String,
) -> Result<LinearizeResult, String> {
    export_linearized_copy_impl(&state, &doc_id, &dest_path).map_err(String::from)
}

pub(crate) fn export_linearized_copy_impl(
    state: &AppState,
    doc_id: &str,
    dest_path: &str,
) -> Result<LinearizeResult, AppError> {
    // Refuse writing over the open document's own file, or a path open in
    // another tab — this is a copy operation (unlike Save As), so there is
    // no "saving to your own path is fine" case here.
    let entry_arc = state.get_document(doc_id)?;
    {
        let entry = lock_mutex(&entry_arc)?;
        if paths_match(&entry.file_path, dest_path) {
            return Err(AppError::Other(
                "Can't export over the file that's currently open. Choose a different name."
                    .to_string(),
            ));
        }
    }
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

    // The buffer is authoritative (unsaved edits included, plaintext even for
    // an encrypted document — issue #57/#31) and is handed to qpdf's
    // path-based API via a temp file, following the same handoff shape as
    // the print feature. The document lock is released before the (slow)
    // linearize call so other tabs aren't blocked while qpdf runs.
    let tmp = std::env::temp_dir().join(format!("tumbler-linearize-{}.pdf", uuid::Uuid::new_v4()));
    let original_size = {
        let entry = lock_mutex(&entry_arc)?;
        std::fs::write(&tmp, &entry.buffer)
            .map_err(|e| AppError::io("Failed to write temporary PDF for linearization", e))?;
        entry.buffer.len() as u64
    };

    let result = state.linearizer.linearize(&tmp, Path::new(dest_path));
    let _ = std::fs::remove_file(&tmp);
    result?;

    let linearized_size = std::fs::metadata(dest_path)
        .map_err(|e| AppError::io("Failed to read linearized output size", e))?
        .len();

    Ok(LinearizeResult {
        original_size,
        linearized_size,
    })
}

/// True if `dest_path` refers to the same file as `open_path`, comparing
/// canonically when both exist (so different-but-equivalent spellings, e.g.
/// short vs. long path forms, are still recognized as a conflict).
fn paths_match(open_path: &str, dest_path: &str) -> bool {
    match (dunce::canonicalize(open_path), dunce::canonicalize(dest_path)) {
        (Ok(a), Ok(b)) => a == b,
        _ => open_path == dest_path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::DocEntry;

    fn tmp_path(name: &str) -> String {
        std::env::temp_dir().join(name).to_string_lossy().into_owned()
    }

    fn state_with_stub_linearizer() -> AppState {
        let pdfium = crate::test_pdfium();
        AppState::new(pdfium, None).with_linearizer(std::sync::Arc::new(StubLinearizer))
    }

    /// StubLinearizer test — no DLL needed, always passes in CI.
    #[test]
    fn export_linearized_copy_writes_a_file() {
        let _guard = crate::test_pdfium_guard();
        let state = state_with_stub_linearizer();
        let src = crate::fixture_path();
        let entry = DocEntry::load(state.pdfium, &src.to_string_lossy(), None).expect("load");
        state.insert_document("doc1".to_string(), entry).expect("insert");

        let dest = tmp_path("tumbler_linearized_out.pdf");
        std::fs::remove_file(&dest).ok();

        let result = export_linearized_copy_impl(&state, "doc1", &dest).expect("export");

        assert!(std::path::Path::new(&dest).exists(), "output file should be created");
        assert!(std::fs::metadata(&dest).expect("metadata").len() > 0);
        assert!(result.original_size > 0, "should report the source buffer size");
        assert_eq!(
            result.linearized_size,
            std::fs::metadata(&dest).expect("metadata").len(),
            "reported size should match the written file"
        );

        std::fs::remove_file(&dest).ok();
    }

    #[test]
    fn export_linearized_copy_refuses_own_open_path() {
        let _guard = crate::test_pdfium_guard();
        let state = state_with_stub_linearizer();
        let path = tmp_path("tumbler_linearize_self.pdf");
        std::fs::copy(crate::fixture_path(), &path).expect("copy fixture");
        let entry = DocEntry::load(state.pdfium, &path, None).expect("load");
        state.insert_document("doc1".to_string(), entry).expect("insert");

        let err = export_linearized_copy_impl(&state, "doc1", &path)
            .expect_err("should refuse to export over the open document's own path");
        assert!(err.to_string().contains("currently open"), "unexpected error: {err}");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn export_linearized_copy_refuses_path_open_in_another_tab() {
        let _guard = crate::test_pdfium_guard();
        let state = state_with_stub_linearizer();
        let path_a = tmp_path("tumbler_linearize_conflict_a.pdf");
        let path_b = tmp_path("tumbler_linearize_conflict_b.pdf");
        std::fs::copy(crate::fixture_path(), &path_a).expect("copy fixture a");
        std::fs::copy(crate::fixture_path(), &path_b).expect("copy fixture b");

        let entry_a = DocEntry::load(state.pdfium, &path_a, None).expect("load a");
        state.insert_document("doc-a".to_string(), entry_a).expect("insert a");
        let entry_b = DocEntry::load(state.pdfium, &path_b, None).expect("load b");
        state.insert_document("doc-b".to_string(), entry_b).expect("insert b");

        let err = export_linearized_copy_impl(&state, "doc-a", &path_b)
            .expect_err("should refuse a path open in another tab");
        assert!(err.to_string().contains("open in another tab"), "unexpected error: {err}");

        std::fs::remove_file(&path_a).ok();
        std::fs::remove_file(&path_b).ok();
    }

    #[test]
    fn buffer_is_linearized_detects_the_marker_dictionary() {
        let linearized = b"%PDF-1.5\n1 0 obj\n<< /Linearized 1 /L 1298 >>\nendobj\n";
        assert!(buffer_is_linearized(linearized));
    }

    #[test]
    fn buffer_is_linearized_is_false_for_an_ordinary_pdf() {
        let bytes = std::fs::read(crate::fixture_path()).expect("read fixture");
        assert!(!buffer_is_linearized(&bytes));
    }

    #[test]
    fn buffer_is_linearized_is_false_for_a_marker_beyond_the_scan_window() {
        // The marker exists in the file, but far past where a streaming
        // viewer (or this scan) would have looked — must not false-positive
        // by scanning the whole buffer.
        let mut bytes = vec![b'x'; 4096];
        bytes.extend_from_slice(b"/Linearized");
        assert!(!buffer_is_linearized(&bytes));
    }

    /// Real qpdf — ignored in CI (no DLL there). Exercises the actual FFI
    /// call sequence and verifies the output is genuinely linearized.
    #[test]
    #[ignore = "needs real qpdf.dll at src-tauri/resources/qpdf.dll"]
    fn linearized_output_has_linearized_marker() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let linearizer = QpdfLinearizer {
            dll_path: std::path::PathBuf::from("resources/qpdf.dll"),
        };
        let state = AppState::new(pdfium, None).with_linearizer(std::sync::Arc::new(linearizer));

        let src = crate::fixture_path();
        let entry = DocEntry::load(pdfium, &src.to_string_lossy(), None).expect("load");
        state.insert_document("doc1".to_string(), entry).expect("insert");

        let dest = tmp_path("tumbler_linearized_real.pdf");
        std::fs::remove_file(&dest).ok();

        export_linearized_copy_impl(&state, "doc1", &dest).expect("export");

        let bytes = std::fs::read(&dest).expect("read output");
        let doc = lopdf::Document::load_mem(&bytes).expect("parse output with lopdf");
        // qpdf doesn't guarantee the linearization dictionary is object 1 (in
        // this fixture it's object 2, with /O pointing at object 5 as the
        // first page) — scan for whichever object carries /Linearized.
        let has_linearization_dict = doc
            .objects
            .values()
            .any(|obj| obj.as_dict().is_ok_and(|d| d.has(b"Linearized")));
        assert!(has_linearization_dict, "output should carry a /Linearized dictionary");
        // The cheap prefix-scan used for the status-bar badge should agree
        // with the full lopdf-based check above.
        assert!(buffer_is_linearized(&bytes), "prefix scan should also detect it");

        // pdfium must still be able to load the linearized output.
        let reopened = pdfium.load_pdf_from_file(&dest, None).expect("pdfium should load linearized output");
        assert_eq!(reopened.pages().len(), 1);

        std::fs::remove_file(&dest).ok();
    }
}
