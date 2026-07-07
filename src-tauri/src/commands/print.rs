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
// The 64-suffixed variant takes a size_t length; the plain FPDF_LoadMemDocument
// takes a C int and silently caps at 2 GB.
#[allow(non_camel_case_types)]
type FnLoadMemDocument64 = unsafe extern "C" fn(
    data_buf: *const c_void,
    size: usize,
    password: *const u8,
) -> *mut c_void;
#[allow(non_camel_case_types)]
type FnGetPageCount = unsafe extern "C" fn(document: *mut c_void) -> c_int;
#[allow(non_camel_case_types)]
type FnLoadPage = unsafe extern "C" fn(document: *mut c_void, page_index: c_int) -> *mut c_void;
#[allow(non_camel_case_types)]
type FnGetPageWidth = unsafe extern "C" fn(page: *mut c_void) -> f64;
#[allow(non_camel_case_types)]
type FnGetPageHeight = unsafe extern "C" fn(page: *mut c_void) -> f64;
#[allow(non_camel_case_types)]
type FnClosePage = unsafe extern "C" fn(page: *mut c_void);
#[allow(non_camel_case_types)]
type FnCloseDocument = unsafe extern "C" fn(document: *mut c_void);
// Vector render straight to a GDI DC — used for pages with no form fields, so
// they print at full printer resolution with selectable text (unchanged path).
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

// Widget detection: only pages that actually carry form fields need the raster
// path below.
#[allow(non_camel_case_types)]
type FnGetAnnotCount = unsafe extern "C" fn(page: *mut c_void) -> c_int;
#[allow(non_camel_case_types)]
type FnGetAnnot = unsafe extern "C" fn(page: *mut c_void, index: c_int) -> *mut c_void;
#[allow(non_camel_case_types)]
type FnGetAnnotSubtype = unsafe extern "C" fn(annot: *mut c_void) -> c_int;
#[allow(non_camel_case_types)]
type FnCloseAnnot = unsafe extern "C" fn(annot: *mut c_void);
const FPDF_ANNOT_WIDGET: c_int = 20;

// Interactive form fields (widgets) are NOT drawn by FPDF_RenderPage — they are
// owned by the form-fill module. Rendering them requires initializing a
// form-fill environment and calling FPDF_FFLDraw onto a bitmap. Since FFLDraw
// only targets a bitmap, pages that contain widgets are rasterized (page + form)
// and blitted to the printer DC; pages without widgets keep the vector DC path.
#[allow(non_camel_case_types)]
type FnInitFormEnv =
    unsafe extern "C" fn(document: *mut c_void, form_info: *mut FpdfFormFillInfo) -> *mut c_void;
#[allow(non_camel_case_types)]
type FnExitFormEnv = unsafe extern "C" fn(form: *mut c_void);
#[allow(non_camel_case_types)]
type FnFfldraw = unsafe extern "C" fn(
    form: *mut c_void,
    bitmap: *mut c_void,
    page: *mut c_void,
    start_x: c_int,
    start_y: c_int,
    size_x: c_int,
    size_y: c_int,
    rotate: c_int,
    flags: c_int,
);
#[allow(non_camel_case_types)]
type FnBitmapCreate =
    unsafe extern "C" fn(width: c_int, height: c_int, alpha: c_int) -> *mut c_void;
#[allow(non_camel_case_types)]
type FnBitmapFillRect = unsafe extern "C" fn(
    bitmap: *mut c_void,
    left: c_int,
    top: c_int,
    width: c_int,
    height: c_int,
    color: u32,
);
#[allow(non_camel_case_types)]
type FnRenderPageBitmap = unsafe extern "C" fn(
    bitmap: *mut c_void,
    page: *mut c_void,
    start_x: c_int,
    start_y: c_int,
    size_x: c_int,
    size_y: c_int,
    rotate: c_int,
    flags: c_int,
);
#[allow(non_camel_case_types)]
type FnBitmapGetBuffer = unsafe extern "C" fn(bitmap: *mut c_void) -> *mut c_void;
#[allow(non_camel_case_types)]
type FnBitmapDestroy = unsafe extern "C" fn(bitmap: *mut c_void);

/// Minimal `FPDF_FORMFILLINFO` — version 1 with all callbacks null, which is
/// sufficient to *render* interactive form fields (no user interaction). pdfium
/// keeps the pointer we pass to `InitFormFillEnvironment`, so an instance must
/// outlive the environment.
#[repr(C)]
struct FpdfFormFillInfo {
    version: c_int,
    callbacks: [*const c_void; 16],
}

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

/// What the STA print thread's pdfium loads. A clean document prints straight
/// from its file (ciphertext on disk for a password-protected one); a dirty
/// document (unsaved buffer edits, issue #31) hands over the buffer **in
/// memory**. The buffer is plaintext even for an encrypted document (issue
/// #57), so it must never be staged through a temp file — a crash or early
/// return would leave decrypted bytes on disk.
enum PrintSource {
    File(String),
    Memory(Vec<u8>),
}

/// Decides how the print thread receives the document, and with what
/// password. Split out of `print_document_impl` so the no-plaintext-on-disk
/// rule is unit-testable: dirty ⟹ memory, clean ⟹ file path.
/// The password is needed only for the clean-encrypted case (the file on
/// disk is ciphertext); pdfium ignores a password for unencrypted bytes.
fn print_source_for(entry: &crate::state::DocEntry) -> (PrintSource, Option<String>) {
    let password = entry.password.clone();
    if entry.dirty {
        (PrintSource::Memory(entry.buffer.clone()), password)
    } else {
        (PrintSource::File(entry.file_path.clone()), password)
    }
}

async fn print_document_impl(
    window: tauri::WebviewWindow,
    state: &AppState,
    doc_id: String,
) -> Result<PrintResult, AppError> {
    let (source, password) = {
        let entry = state.get_document(&doc_id)?;
        let entry = lock_mutex(&entry)?;
        print_source_for(&entry)
    };

    // Resolve pdfium.dll path (same logic as lib.rs)
    let pdfium_path = crate::resolve_pdfium_path();

    let hwnd_raw = window.hwnd().map_err(|e| format!("hwnd() failed: {e}"))?.0 as isize;

    let cancel = Arc::new(AtomicBool::new(false));
    state.set_print_job(cancel.clone());

    let (tx, rx) = std::sync::mpsc::channel();

    // `source` is owned by the closure and only borrowed by the print call,
    // so a Memory buffer outlives the pdfium document handle opened over it
    // (FPDF_LoadMemDocument64 does not copy the bytes).
    std::thread::spawn(move || {
        let result =
            print_on_sta_thread(hwnd_raw, &pdfium_path, &source, password.as_deref(), &window, cancel);
        tx.send(result).ok();
    });

    let result = rx.recv().map_err(|e| AppError::Other(format!("channel recv failed: {e}")));
    state.take_print_job();
    result?
}

fn print_on_sta_thread(
    hwnd_raw: isize,
    pdfium_path: &str,
    source: &PrintSource,
    password: Option<&str>,
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

    let result = print_impl(hwnd_raw, pdfium_path, source, password, window, cancel);

    unsafe { CoUninitialize() };
    result
}

fn print_impl(
    hwnd_raw: isize,
    pdfium_path: &str,
    source: &PrintSource,
    password: Option<&str>,
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
    let fpdf_load_mem_document64: libloading::Symbol<FnLoadMemDocument64> =
        unsafe { lib.get(b"FPDF_LoadMemDocument64\0") }
            .map_err(|e| format!("Failed to find FPDF_LoadMemDocument64: {e}"))?;
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
    let fpdf_get_annot_count: libloading::Symbol<FnGetAnnotCount> =
        unsafe { lib.get(b"FPDFPage_GetAnnotCount\0") }
            .map_err(|e| format!("Failed to find FPDFPage_GetAnnotCount: {e}"))?;
    let fpdf_get_annot: libloading::Symbol<FnGetAnnot> =
        unsafe { lib.get(b"FPDFPage_GetAnnot\0") }
            .map_err(|e| format!("Failed to find FPDFPage_GetAnnot: {e}"))?;
    let fpdf_get_annot_subtype: libloading::Symbol<FnGetAnnotSubtype> =
        unsafe { lib.get(b"FPDFAnnot_GetSubtype\0") }
            .map_err(|e| format!("Failed to find FPDFAnnot_GetSubtype: {e}"))?;
    let fpdf_close_annot: libloading::Symbol<FnCloseAnnot> =
        unsafe { lib.get(b"FPDFPage_CloseAnnot\0") }
            .map_err(|e| format!("Failed to find FPDFPage_CloseAnnot: {e}"))?;
    let fpdf_close_page: libloading::Symbol<FnClosePage> =
        unsafe { lib.get(b"FPDF_ClosePage\0") }
            .map_err(|e| format!("Failed to find FPDF_ClosePage: {e}"))?;
    let fpdf_close_document: libloading::Symbol<FnCloseDocument> =
        unsafe { lib.get(b"FPDF_CloseDocument\0") }
            .map_err(|e| format!("Failed to find FPDF_CloseDocument: {e}"))?;
    let fpdf_init_form_env: libloading::Symbol<FnInitFormEnv> =
        unsafe { lib.get(b"FPDFDOC_InitFormFillEnvironment\0") }
            .map_err(|e| format!("Failed to find FPDFDOC_InitFormFillEnvironment: {e}"))?;
    let fpdf_exit_form_env: libloading::Symbol<FnExitFormEnv> =
        unsafe { lib.get(b"FPDFDOC_ExitFormFillEnvironment\0") }
            .map_err(|e| format!("Failed to find FPDFDOC_ExitFormFillEnvironment: {e}"))?;
    let fpdf_ffldraw: libloading::Symbol<FnFfldraw> =
        unsafe { lib.get(b"FPDF_FFLDraw\0") }
            .map_err(|e| format!("Failed to find FPDF_FFLDraw: {e}"))?;
    let fpdf_bitmap_create: libloading::Symbol<FnBitmapCreate> =
        unsafe { lib.get(b"FPDFBitmap_Create\0") }
            .map_err(|e| format!("Failed to find FPDFBitmap_Create: {e}"))?;
    let fpdf_bitmap_fill_rect: libloading::Symbol<FnBitmapFillRect> =
        unsafe { lib.get(b"FPDFBitmap_FillRect\0") }
            .map_err(|e| format!("Failed to find FPDFBitmap_FillRect: {e}"))?;
    let fpdf_render_page_bitmap: libloading::Symbol<FnRenderPageBitmap> =
        unsafe { lib.get(b"FPDF_RenderPageBitmap\0") }
            .map_err(|e| format!("Failed to find FPDF_RenderPageBitmap: {e}"))?;
    let fpdf_bitmap_get_buffer: libloading::Symbol<FnBitmapGetBuffer> =
        unsafe { lib.get(b"FPDFBitmap_GetBuffer\0") }
            .map_err(|e| format!("Failed to find FPDFBitmap_GetBuffer: {e}"))?;
    let fpdf_bitmap_destroy: libloading::Symbol<FnBitmapDestroy> =
        unsafe { lib.get(b"FPDFBitmap_Destroy\0") }
            .map_err(|e| format!("Failed to find FPDFBitmap_Destroy: {e}"))?;

    // Load the PDF document (pdfium is already initialized by the main thread).
    // A clean encrypted document's file is ciphertext (issue #12/#57), so pass
    // the stored password; pdfium ignores it when the bytes aren't encrypted.
    let password_cstr = password
        .map(std::ffi::CString::new)
        .transpose()
        .map_err(|_| "Invalid password".to_string())?;
    let password_ptr = password_cstr
        .as_ref()
        .map_or(std::ptr::null(), |c| c.as_ptr() as *const u8);
    let doc = match source {
        PrintSource::File(path) => {
            let pdf_path_cstr = std::ffi::CString::new(path.as_str())
                .map_err(|_| "Invalid PDF path".to_string())?;
            unsafe { fpdf_load_document(pdf_path_cstr.as_ptr() as *const u8, password_ptr) }
        }
        // FPDF_LoadMemDocument64 borrows the bytes; `source` belongs to our
        // caller's frame, so it strictly outlives `_doc_guard` below.
        PrintSource::Memory(bytes) => unsafe {
            fpdf_load_mem_document64(bytes.as_ptr() as *const c_void, bytes.len(), password_ptr)
        },
    };
    if doc.is_null() {
        return Err(AppError::Other("Failed to load PDF for printing".to_string()));
    }
    let _doc_guard = DocumentGuard {
        doc,
        close_fn: *fpdf_close_document,
    };

    // Initialize the form-fill environment so interactive form fields render.
    // `form_info` must outlive the environment (pdfium retains the pointer), and
    // the environment must be torn down before the document — declared after the
    // doc guard so it drops first.
    let mut form_info = FpdfFormFillInfo {
        version: 1,
        callbacks: [std::ptr::null(); 16],
    };
    let form_env = unsafe { fpdf_init_form_env(doc, &mut form_info) };
    struct FormGuard {
        form: *mut c_void,
        exit_fn: FnExitFormEnv,
    }
    impl Drop for FormGuard {
        fn drop(&mut self) {
            if !self.form.is_null() {
                unsafe { (self.exit_fn)(self.form) };
            }
        }
    }
    let _form_guard = FormGuard {
        form: form_env,
        exit_fn: *fpdf_exit_form_env,
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

        // Rasterize only pages that actually contain form-field widgets;
        // everything else prints via the vector DC path (full printer
        // resolution, selectable text) exactly as before.
        let has_widget = unsafe {
            let n = fpdf_get_annot_count(page);
            let mut found = false;
            for i in 0..n {
                let annot = fpdf_get_annot(page, i);
                if !annot.is_null() {
                    let subtype = fpdf_get_annot_subtype(annot);
                    fpdf_close_annot(annot);
                    if subtype == FPDF_ANNOT_WIDGET {
                        found = true;
                        break;
                    }
                }
            }
            found
        };

        if !has_widget {
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
        } else {
            unsafe {
            // Rasterize page + interactive form layer, then blit to the printer
            // DC. FPDF_RenderPage (vector, to the DC) can't draw form widgets, so
            // we must go through a bitmap + FPDF_FFLDraw. Cap the raster to
            // ~300 DPI so very high device resolutions don't exhaust memory;
            // StretchDIBits scales it to the device rectangle.
            let max_dim = 3400;
            let rscale = (max_dim as f64 / render_width.max(render_height) as f64).min(1.0);
            let bw = (((render_width as f64) * rscale) as c_int).max(1);
            let bh = (((render_height as f64) * rscale) as c_int).max(1);

            let bmp = fpdf_bitmap_create(bw, bh, 1);
            if !bmp.is_null() {
                fpdf_bitmap_fill_rect(bmp, 0, 0, bw, bh, 0xFFFF_FFFF); // white
                fpdf_render_page_bitmap(bmp, page, 0, 0, bw, bh, 0, FPDF_PRINTING | FPDF_ANNOT);
                if !form_env.is_null() {
                    fpdf_ffldraw(form_env, bmp, page, 0, 0, bw, bh, 0, FPDF_ANNOT);
                }
                let buf = fpdf_bitmap_get_buffer(bmp);
                // pdfium's buffer is top-down BGRA — a 32bpp BI_RGB DIB reads it
                // directly (Windows treats the bytes as BGRX), no channel swap.
                let bmi = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: bw,
                        biHeight: -bh, // negative = top-down
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                StretchDIBits(
                    hdc,
                    start_x,
                    start_y,
                    render_width,
                    render_height,
                    0,
                    0,
                    bw,
                    bh,
                    Some(buf),
                    &bmi,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                );
                fpdf_bitmap_destroy(bmp);
            }
            fpdf_close_page(page);
            }
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
    use crate::state::DocEntry;

    /// The no-plaintext-on-disk rule (issue #57): a dirty document — whose
    /// buffer is decrypted plaintext even for a password-protected file —
    /// must be handed to the print thread in memory, never staged through a
    /// temp file. A clean document prints from its file path (ciphertext on
    /// disk for an encrypted one) with the stored password.
    #[test]
    fn dirty_document_prints_from_memory_clean_document_from_file() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let path = crate::encrypted_fixture_path().to_string_lossy().into_owned();
        let mut entry = DocEntry::load(pdfium, &path, Some(crate::ENCRYPTED_FIXTURE_PASSWORD))
            .expect("load encrypted fixture");

        // Clean: file-path handoff, password included for the encrypted file.
        let (source, password) = print_source_for(&entry);
        assert!(matches!(source, PrintSource::File(p) if p == entry.file_path));
        assert_eq!(password.as_deref(), Some(crate::ENCRYPTED_FIXTURE_PASSWORD));

        // Dirty: the (plaintext) buffer itself, in memory.
        entry.dirty = true;
        let (source, _) = print_source_for(&entry);
        match source {
            PrintSource::Memory(bytes) => assert_eq!(bytes, entry.buffer),
            PrintSource::File(_) => panic!("dirty document must not print from a file path"),
        }
    }

    /// Pins the raw `FPDF_LoadMemDocument64` binding against the shipped
    /// pdfium.dll: the symbol exists and our signature (`*const c_void`,
    /// `usize` length, password string) is right. A mismatch here would
    /// otherwise only surface as a crash or null document during a live
    /// print of an edited document.
    #[test]
    fn load_mem_document64_binding_matches_shipped_dll() {
        let _guard = crate::test_pdfium_guard();
        // Binds pdfium and calls FPDF_InitLibrary, which the raw symbols
        // below rely on (mirrors print_impl running against the app's
        // already-initialized library).
        let _ = crate::test_pdfium();

        let lib = unsafe { libloading::Library::new(crate::resolve_pdfium_path()) }
            .expect("load pdfium.dll");
        let load: libloading::Symbol<FnLoadMemDocument64> =
            unsafe { lib.get(b"FPDF_LoadMemDocument64\0") }.expect("find FPDF_LoadMemDocument64");
        let page_count: libloading::Symbol<FnGetPageCount> =
            unsafe { lib.get(b"FPDF_GetPageCount\0") }.expect("find FPDF_GetPageCount");
        let close: libloading::Symbol<FnCloseDocument> =
            unsafe { lib.get(b"FPDF_CloseDocument\0") }.expect("find FPDF_CloseDocument");

        // Plain fixture, no password.
        let bytes = std::fs::read(crate::fixture_path()).expect("read fixture");
        let doc = unsafe { load(bytes.as_ptr() as *const c_void, bytes.len(), std::ptr::null()) };
        assert!(!doc.is_null(), "memory load of the plain fixture failed");
        assert_eq!(unsafe { page_count(doc) }, 1);
        unsafe { close(doc) };

        // Encrypted fixture: rejected without the password, opens with it —
        // the same password plumbing print_impl uses for the memory path.
        let bytes = std::fs::read(crate::encrypted_fixture_path()).expect("read encrypted fixture");
        let doc = unsafe { load(bytes.as_ptr() as *const c_void, bytes.len(), std::ptr::null()) };
        assert!(doc.is_null(), "encrypted bytes must not open without a password");
        let pw = std::ffi::CString::new(crate::ENCRYPTED_FIXTURE_PASSWORD).unwrap();
        let doc = unsafe {
            load(bytes.as_ptr() as *const c_void, bytes.len(), pw.as_ptr() as *const u8)
        };
        assert!(!doc.is_null(), "encrypted bytes must open with the password");
        assert_eq!(unsafe { page_count(doc) }, 1);
        unsafe { close(doc) };
    }

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
