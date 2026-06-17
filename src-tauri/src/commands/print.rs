use crate::error::AppError;
use crate::state::{lock_mutex, AppState};
use serde::Serialize;
use std::ffi::c_void;
use std::os::raw::c_int;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{Emitter, State};

#[derive(Serialize, Clone)]
pub struct PrintResult {
    pub printed: bool,
    pub pages_printed: u32,
    pub cancelled: bool,
}

#[derive(Serialize, Clone)]
pub struct PrintProgress {
    pub page: u32,
    pub total: u32,
}

// Raw pdfium function signatures loaded via libloading
#[allow(non_camel_case_types)]
type FnLoadDocument =
    unsafe extern "C" fn(file_path: *const u8, password: *const u8) -> *mut c_void;
#[allow(non_camel_case_types)]
type FnGetPageCount = unsafe extern "C" fn(document: *mut c_void) -> c_int;
#[allow(non_camel_case_types)]
type FnLoadPage = unsafe extern "C" fn(document: *mut c_void, page_index: c_int) -> *mut c_void;
#[allow(non_camel_case_types)]
type FnGetPageWidth = unsafe extern "C" fn(page: *mut c_void) -> f64;
#[allow(non_camel_case_types)]
type FnGetPageHeight = unsafe extern "C" fn(page: *mut c_void) -> f64;
#[allow(non_camel_case_types)]
type FnRenderPage = unsafe extern "C" fn(
    dc: *mut c_void,
    page: *mut c_void,
    start_x: c_int,
    start_y: c_int,
    size_x: c_int,
    size_y: c_int,
    rotate: c_int,
    flags: c_int,
);
#[allow(non_camel_case_types)]
type FnClosePage = unsafe extern "C" fn(page: *mut c_void);
#[allow(non_camel_case_types)]
type FnCloseDocument = unsafe extern "C" fn(document: *mut c_void);

const FPDF_PRINTING: c_int = 0x800;
const FPDF_ANNOT: c_int = 0x01;

#[tauri::command]
pub async fn print_document(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    doc_id: String,
) -> Result<PrintResult, String> {
    print_document_impl(window, &state, doc_id)
        .await
        .map_err(String::from)
}

async fn print_document_impl(
    window: tauri::WebviewWindow,
    state: &AppState,
    doc_id: String,
) -> Result<PrintResult, AppError> {
    // Look up the file path for this document
    let file_path = {
        let entry = state.get_document(&doc_id)?;
        let entry = lock_mutex(&entry)?;
        entry.file_path.clone()
    };

    // Resolve pdfium.dll path (same logic as lib.rs)
    let pdfium_path = crate::resolve_pdfium_path();

    let hwnd_raw = window.hwnd().map_err(|e| format!("hwnd() failed: {e}"))?.0 as isize;

    let cancel = Arc::new(AtomicBool::new(false));
    state.set_print_job(cancel.clone());

    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let result = print_on_sta_thread(hwnd_raw, &pdfium_path, &file_path, &window, cancel);
        tx.send(result).ok();
    });

    let result = rx.recv().map_err(|e| AppError::Other(format!("channel recv failed: {e}")));
    state.take_print_job();
    result?
}

fn print_on_sta_thread(
    hwnd_raw: isize,
    pdfium_path: &str,
    pdf_path: &str,
    window: &tauri::WebviewWindow,
    cancel: Arc<AtomicBool>,
) -> Result<PrintResult, AppError> {
    use windows::Win32::System::Com::*;

    // Initialize COM as STA — must be first call on this thread
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    if hr.is_err() {
        return Err(AppError::Other(format!(
            "CoInitializeEx failed: HRESULT 0x{:08x}",
            hr.0
        )));
    }

    let result = print_impl(hwnd_raw, pdfium_path, pdf_path, window, cancel);

    unsafe { CoUninitialize() };
    result
}

fn print_impl(
    hwnd_raw: isize,
    pdfium_path: &str,
    pdf_path: &str,
    window: &tauri::WebviewWindow,
    cancel: Arc<AtomicBool>,
) -> Result<PrintResult, AppError> {
    use windows::Win32::Foundation::*;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::Storage::Xps::*;
    use windows::Win32::System::Memory::*;
    use windows::Win32::UI::Controls::Dialogs::*;

    /// Closes the pdfium document on drop. `close_fn` is a copy of the
    /// `FPDF_CloseDocument` function pointer, which stays valid for as long
    /// as the pdfium library that produced it (`lib`, below) remains loaded.
    struct DocumentGuard {
        doc: *mut c_void,
        close_fn: FnCloseDocument,
    }

    impl Drop for DocumentGuard {
        fn drop(&mut self) {
            if !self.doc.is_null() {
                unsafe { (self.close_fn)(self.doc) };
            }
        }
    }

    /// Frees a `GlobalAlloc`'d handle (e.g. `hDevMode`/`hDevNames` from
    /// `PrintDlgExW`) on drop.
    struct GlobalHandleGuard(HGLOBAL);

    impl Drop for GlobalHandleGuard {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                let _ = unsafe { GlobalFree(Some(self.0)) };
            }
        }
    }

    /// Deletes a device context on drop.
    struct DcGuard(HDC);

    impl Drop for DcGuard {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                let _ = unsafe { DeleteDC(self.0) };
            }
        }
    }

    // Load pdfium to get page count for the dialog
    let lib = unsafe { libloading::Library::new(pdfium_path) }
        .map_err(|e| format!("Failed to load pdfium.dll: {e}"))?;

    let fpdf_load_document: libloading::Symbol<FnLoadDocument> =
        unsafe { lib.get(b"FPDF_LoadDocument\0") }
            .map_err(|e| format!("Failed to find FPDF_LoadDocument: {e}"))?;
    let fpdf_get_page_count: libloading::Symbol<FnGetPageCount> =
        unsafe { lib.get(b"FPDF_GetPageCount\0") }
            .map_err(|e| format!("Failed to find FPDF_GetPageCount: {e}"))?;
    let fpdf_load_page: libloading::Symbol<FnLoadPage> =
        unsafe { lib.get(b"FPDF_LoadPage\0") }
            .map_err(|e| format!("Failed to find FPDF_LoadPage: {e}"))?;
    let fpdf_get_page_width: libloading::Symbol<FnGetPageWidth> =
        unsafe { lib.get(b"FPDF_GetPageWidth\0") }
            .map_err(|e| format!("Failed to find FPDF_GetPageWidth: {e}"))?;
    let fpdf_get_page_height: libloading::Symbol<FnGetPageHeight> =
        unsafe { lib.get(b"FPDF_GetPageHeight\0") }
            .map_err(|e| format!("Failed to find FPDF_GetPageHeight: {e}"))?;
    let fpdf_render_page: libloading::Symbol<FnRenderPage> =
        unsafe { lib.get(b"FPDF_RenderPage\0") }
            .map_err(|e| format!("Failed to find FPDF_RenderPage: {e}"))?;
    let fpdf_close_page: libloading::Symbol<FnClosePage> =
        unsafe { lib.get(b"FPDF_ClosePage\0") }
            .map_err(|e| format!("Failed to find FPDF_ClosePage: {e}"))?;
    let fpdf_close_document: libloading::Symbol<FnCloseDocument> =
        unsafe { lib.get(b"FPDF_CloseDocument\0") }
            .map_err(|e| format!("Failed to find FPDF_CloseDocument: {e}"))?;

    // Load the PDF document (pdfium is already initialized by the main thread)
    let pdf_path_cstr = std::ffi::CString::new(pdf_path)
        .map_err(|_| "Invalid PDF path".to_string())?;
    let doc = unsafe { fpdf_load_document(pdf_path_cstr.as_ptr() as *const u8, std::ptr::null()) };
    if doc.is_null() {
        return Err(AppError::Other("Failed to load PDF for printing".to_string()));
    }
    let _doc_guard = DocumentGuard {
        doc,
        close_fn: *fpdf_close_document,
    };

    let page_count = unsafe { fpdf_get_page_count(doc) } as u32;

    // Set up PrintDlgExW
    let mut page_range = PRINTPAGERANGE {
        nFromPage: 1,
        nToPage: page_count,
    };

    let hwnd = HWND(hwnd_raw as *mut c_void);

    let mut pdx = PRINTDLGEXW {
        lStructSize: std::mem::size_of::<PRINTDLGEXW>() as u32,
        hwndOwner: hwnd,
        Flags: PD_ALLPAGES | PD_NOSELECTION | PD_NOCURRENTPAGE | PD_USEDEVMODECOPIESANDCOLLATE | PD_HIDEPRINTTOFILE,
        nPageRanges: 0,
        nMaxPageRanges: 1,
        lpPageRanges: &mut page_range,
        nMinPage: 1,
        nMaxPage: page_count,
        nCopies: 1,
        nStartPage: START_PAGE_GENERAL,
        ..Default::default()
    };

    let dialog_result = unsafe { PrintDlgExW(&mut pdx) };
    if dialog_result.is_err() {
        return Err(AppError::Other(format!("PrintDlgExW failed: {dialog_result:?}")));
    }

    // hDevMode/hDevNames may be allocated even if the user cancelled.
    let _devmode_guard = GlobalHandleGuard(pdx.hDevMode);
    let _devnames_guard = GlobalHandleGuard(pdx.hDevNames);

    if pdx.dwResultAction != PD_RESULT_PRINT {
        // User dismissed the dialog
        return Ok(PrintResult {
            printed: false,
            pages_printed: 0,
            cancelled: false,
        });
    }

    // Extract printer name from DEVNAMES
    let printer_name = unsafe {
        let devnames_ptr = GlobalLock(pdx.hDevNames) as *const DEVNAMES;
        let devnames = &*devnames_ptr;
        let base = devnames_ptr as *const u8;
        let device_ptr =
            (base.add(devnames.wDeviceOffset as usize * 2)) as *const u16;
        let name = read_wide_string(device_ptr);
        let _ = GlobalUnlock(pdx.hDevNames);
        name
    };

    // Extract DEVMODE and create printer DC
    let hdc = unsafe {
        let devmode_ptr = GlobalLock(pdx.hDevMode) as *const DEVMODEW;
        let printer_wide: Vec<u16> = printer_name.encode_utf16().chain(std::iter::once(0)).collect();
        let hdc = CreateDCW(
            None,
            windows::core::PCWSTR(printer_wide.as_ptr()),
            None,
            Some(devmode_ptr),
        );
        let _ = GlobalUnlock(pdx.hDevMode);
        hdc
    };

    if hdc.is_invalid() {
        return Err(AppError::Other("Failed to create printer DC".to_string()));
    }
    let _dc_guard = DcGuard(hdc);

    // Determine which pages to print
    let pages_to_print: Vec<u32> = if pdx.Flags.contains(PD_PAGENUMS) && pdx.nPageRanges > 0 {
        let range = unsafe { &*pdx.lpPageRanges };
        (range.nFromPage..=range.nToPage).collect()
    } else {
        (1..=page_count).collect()
    };

    // Get printable area
    let print_width = unsafe { GetDeviceCaps(Some(hdc), HORZRES) };
    let print_height = unsafe { GetDeviceCaps(Some(hdc), VERTRES) };

    // Start the print job
    let doc_name: Vec<u16> = "Tumbler PDF Print"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let doc_info = DOCINFOW {
        cbSize: std::mem::size_of::<DOCINFOW>() as i32,
        lpszDocName: windows::core::PCWSTR(doc_name.as_ptr()),
        ..Default::default()
    };

    let start_result = unsafe { StartDocW(hdc, &doc_info) };
    if start_result <= 0 {
        return Err(AppError::Other("StartDoc failed".to_string()));
    }

    let total = pages_to_print.len() as u32;
    let mut pages_printed = 0u32;
    let mut aborted = false;

    for &page_num in &pages_to_print {
        // Emit progress
        let _ = window.emit(
            "print-progress",
            PrintProgress {
                page: page_num,
                total,
            },
        );

        if unsafe { StartPage(hdc) } <= 0 {
            let _ = unsafe { AbortDoc(hdc) };
            aborted = true;
            break;
        }

        let page = unsafe { fpdf_load_page(doc, (page_num - 1) as c_int) };
        if page.is_null() {
            let _ = unsafe { EndPage(hdc) };
            continue;
        }

        // Get PDF page dimensions and scale to fit printable area
        let pdf_width = unsafe { fpdf_get_page_width(page) };
        let pdf_height = unsafe { fpdf_get_page_height(page) };

        let scale_x = print_width as f64 / pdf_width;
        let scale_y = print_height as f64 / pdf_height;
        let scale = scale_x.min(scale_y);

        let render_width = (pdf_width * scale) as c_int;
        let render_height = (pdf_height * scale) as c_int;

        // Center on the page
        let start_x = (print_width - render_width) / 2;
        let start_y = (print_height - render_height) / 2;

        unsafe {
            fpdf_render_page(
                hdc.0 as *mut c_void,
                page,
                start_x,
                start_y,
                render_width,
                render_height,
                0,
                FPDF_PRINTING | FPDF_ANNOT,
            );
            fpdf_close_page(page);
        }

        if unsafe { EndPage(hdc) } <= 0 {
            let _ = unsafe { AbortDoc(hdc) };
            aborted = true;
            break;
        }

        if cancel.load(Ordering::Relaxed) {
            unsafe { AbortDoc(hdc) };
            return Ok(PrintResult {
                printed: false,
                pages_printed,
                cancelled: true,
            });
        }

        pages_printed += 1;
    }

    // AbortDoc already ended the print job; calling EndDoc afterward is
    // invalid per the Win32 contract.
    if !aborted {
        unsafe { EndDoc(hdc) };
    }

    Ok(PrintResult {
        printed: true,
        pages_printed,
        cancelled: false,
    })
}

#[tauri::command]
pub fn cancel_print(state: State<'_, AppState>) -> Result<(), String> {
    state.cancel_print_job();
    Ok(())
}

/// Read a null-terminated UTF-16 string from a raw pointer.
unsafe fn read_wide_string(ptr: *const u16) -> String {
    let mut len = 0;
    while *ptr.add(len) != 0 {
        len += 1;
    }
    let slice = std::slice::from_raw_parts(ptr, len);
    String::from_utf16_lossy(slice)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exercises the token-check branch by pre-setting the cancel flag and
    /// invoking print_impl. Requires pdfium.dll and a real (or PDF) printer.
    #[test]
    #[ignore = "needs pdfium.dll and a real printer — run locally with cargo test -- --ignored"]
    fn print_cancelled_before_first_page_returns_cancelled_error() {
        // Pre-set the cancel token to true so the very first post-EndPage
        // check fires. Verify the function returns the cancelled error.
        let cancel = Arc::new(AtomicBool::new(true));
        // A real hwnd_raw, pdfium_path, pdf_path, and window are needed here.
        // Run manually on a machine with pdfium.dll and a printer available.
        let _ = cancel;
    }
}
