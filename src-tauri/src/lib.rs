mod commands;
mod error;
mod state;
mod thumbnailer_reg;

use pdfium_render::prelude::*;
use state::AppState;
use tauri::{Emitter, Manager};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Initialize pdfium with a 'static lifetime by leaking the Box.
    // This is intentional — pdfium lives for the entire application lifetime.
    let pdfium: &'static Pdfium = {
        let pdfium_path = resolve_pdfium_path();
        let bindings = Pdfium::bind_to_library(&pdfium_path)
            .unwrap_or_else(|e| panic!("Failed to load pdfium.dll from {pdfium_path}: {e}"));
        Box::leak(Box::new(Pdfium::new(bindings)))
    };

    thumbnailer_reg::ensure_registered();

    let startup_file = pdf_path_from_args(&std::env::args().collect::<Vec<_>>());

    let app_state = AppState::new(pdfium, startup_file);

    tauri::Builder::default()
        // Must be registered first: forwards the command line of a second
        // launch (e.g. double-clicking another PDF) to this instance instead
        // of starting a new process.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
            if let Some(path) = pdf_path_from_args(&argv) {
                let _ = app.emit("open-file", path);
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::document::open_document,
            commands::document::close_document,
            commands::document::canonicalize_path,
            commands::render::render_page,
            commands::text::extract_page_text,
            commands::text::search_document,
            commands::text::export_text,
            commands::text::count_pages_without_text,
            commands::save_searchable::save_searchable_copy,
            commands::ocr::ocr_page,
            commands::ocr::ocr_document,
            commands::ocr::cancel_ocr,
            commands::metadata::get_metadata,
            commands::metadata::set_metadata,
            commands::conformance::get_conformance,
            commands::signature::get_signature_info,
            commands::pages::delete_pages,
            commands::pages::rotate_pages,
            commands::pages::reorder_pages,
            commands::pages::merge_document,
            commands::pages::split_document,
            commands::optimize::run_optimization_steps,
            commands::optimize::save_optimized_copy,
            commands::optimize::cancel_compress,
            commands::print::print_document,
            commands::print::cancel_print,
            commands::startup::take_startup_file,
            commands::theme::get_accent_color,
            commands::app::get_app_version,
        ])
        .setup(|app| {
            #[cfg(debug_assertions)]
            {
                if let Some(window) = app.get_webview_window("main") {
                    window.open_devtools();
                }
            }
            let _ = app;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Extracts a file path passed on the command line, as set up by a Windows
/// file-association launch (`Tumbler.exe "C:\path\to\file.pdf"`). `args[0]`
/// is the executable path, so the file path (if any) is `args[1]`.
fn pdf_path_from_args(args: &[String]) -> Option<String> {
    if args.len() > 2 {
        eprintln!("pdf_path_from_args: ignoring extra arguments: {:?}", &args[2..]);
    }
    args.get(1).filter(|p| !p.is_empty()).cloned()
}

/// Formats a `PdfiumError` as a short identifier (e.g. "FormatError") instead
/// of pdfium-render's default `Display` impl, which pretty-prints the full
/// `Debug` representation across multiple lines (e.g.
/// "PdfiumLibraryInternalError(\n    FormatError,\n)").
pub fn describe_pdfium_error(e: &PdfiumError) -> String {
    match e {
        PdfiumError::PdfiumLibraryInternalError(inner) => format!("{inner:?}"),
        other => format!("{other:?}"),
    }
}

/// Resolve the path to pdfium.dll.
/// In dev mode: look relative to the src-tauri directory.
/// In production: look in the bundled resources directory.
pub fn resolve_pdfium_path() -> String {
    // In dev mode, the DLL is in src-tauri/resources/
    let dev_path = std::path::Path::new("resources/pdfium.dll");
    if dev_path.exists() {
        return dev_path.to_string_lossy().into_owned();
    }

    // Try alongside the executable (for bundled builds)
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let bundled = exe_dir.join("resources").join("pdfium.dll");
            if bundled.exists() {
                return bundled.to_string_lossy().into_owned();
            }
            // Also try directly next to the exe
            let beside_exe = exe_dir.join("pdfium.dll");
            if beside_exe.exists() {
                return beside_exe.to_string_lossy().into_owned();
            }
        }
    }

    // Fallback
    "pdfium.dll".to_string()
}

/// A process-wide mutex that serializes tests which create or mutate pdfium
/// documents. pdfium-render's `thread_safe` feature serializes individual
/// API calls, but multi-step operations (create + copy-pages + save + reload)
/// can interleave between threads in ways that trigger pdfium internal races.
/// Tests that do more than a single read should hold this guard for their
/// duration.
#[cfg(test)]
pub(crate) fn test_pdfium_guard() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// Returns a process-wide `Pdfium` instance for use in tests.
///
/// pdfium-render's `Pdfium::bind_to_library` can only succeed once per
/// process, so tests across multiple modules must share a single binding
/// rather than each binding their own.
#[cfg(test)]
pub(crate) fn test_pdfium() -> &'static Pdfium {
    use std::sync::OnceLock;
    static PDFIUM: OnceLock<Pdfium> = OnceLock::new();
    PDFIUM.get_or_init(|| {
        let bindings = Pdfium::bind_to_library(resolve_pdfium_path()).expect("bind pdfium");
        Pdfium::new(bindings)
    })
}

/// Path to the small checked-in PDF used by tests that need a real,
/// pdfium- and lopdf-loadable document.
#[cfg(test)]
pub(crate) fn fixture_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.pdf")
}
