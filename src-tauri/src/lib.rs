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
            commands::encryption::remove_password,
            commands::encryption::set_password,
            commands::render::render_page,
            commands::text::extract_page_text,
            commands::text::search_document,
            commands::text::export_text,
            commands::text::count_pages_without_text,
            commands::text_layer::add_text_layer,
            commands::typewriter::apply_typewriter,
            commands::typewriter::read_typewriter,
            commands::ocr::ocr_page,
            commands::ocr::ocr_document,
            commands::ocr::cancel_ocr,
            commands::metadata::get_metadata,
            commands::metadata::set_metadata,
            commands::metadata::find_redaction_metadata_matches,
            commands::forms::get_form_fields,
            commands::forms::set_form_field_value,
            commands::forms::document_has_form,
            commands::forms::clear_form,
            commands::forms::reset_form_via_button,
            commands::forms::set_signature_strokes,
            commands::conformance::get_conformance,
            commands::signature::get_signature_info,
            commands::pages::delete_pages,
            commands::pages::rotate_pages,
            commands::pages::reorder_pages,
            commands::pages::merge_document,
            commands::pages::split_document,
            commands::save::save_document,
            commands::save::save_document_as,
            commands::optimize::inspect_images,
            commands::optimize::run_optimization_steps,
            commands::optimize::cancel_compress,
            commands::margins::analyze_margins,
            commands::margins::expand_margins,
            commands::margins::cancel_margins,
            commands::redact::find_redaction_matches,
            commands::redact::apply_redactions,
            commands::redact::render_redacted_page,
            commands::redact::save_redacted_copy,
            commands::redact::discard_redaction,
            commands::redact::cancel_redact,
            commands::print::print_document,
            commands::print::cancel_print,
            commands::linearize::export_linearized_copy,
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

/// Resolve the path to qpdf.dll, used by "Save Web-Optimized Copy" (issue #3).
/// Same resolution order as `resolve_pdfium_path`; unlike pdfium, qpdf.dll is
/// loaded lazily per export call rather than at startup, so a missing DLL
/// only fails that one command.
pub fn resolve_qpdf_path() -> String {
    let dev_path = std::path::Path::new("resources/qpdf.dll");
    if dev_path.exists() {
        return dev_path.to_string_lossy().into_owned();
    }

    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let bundled = exe_dir.join("resources").join("qpdf.dll");
            if bundled.exists() {
                return bundled.to_string_lossy().into_owned();
            }
            let beside_exe = exe_dir.join("qpdf.dll");
            if beside_exe.exists() {
                return beside_exe.to_string_lossy().into_owned();
            }
        }
    }

    "qpdf.dll".to_string()
}

/// Exclusive access to the process-wide test `Pdfium` instance.
///
/// pdfium-render's `thread_safe` feature serializes individual API calls, but
/// multi-step operations (create + copy-pages + save + reload) interleave
/// between threads in ways that trigger pdfium's internal races, surfacing as
/// `STATUS_HEAP_CORRUPTION` — intermittently, and often at teardown, so a
/// single green run proves nothing.
///
/// The lock is bundled with the instance rather than offered alongside it
/// because the alternative did not work. A separate `test_pdfium_guard()` that
/// tests were asked to remember left 47 of them unguarded (PR #109); the gap
/// was invisible only because the whole suite ran with `--test-threads=1`.
/// Here, holding the lock is the *only* way to obtain a `Pdfium`, so a test
/// that forgets simply does not compile.
///
/// Access goes through [`Self::get`]. A `Deref` impl would be more ergonomic
/// but hands back a borrow tied to the handle, and most pdfium calls produce
/// `PdfDocument<'static>` — so it would compile for trivial uses and fail with
/// lifetime errors for real ones. One accessor is easier to explain.
#[cfg(test)]
pub(crate) struct TestPdfium {
    pdfium: &'static Pdfium,
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl TestPdfium {
    /// The shared instance. Keep the handle bound for as long as anything
    /// derived from this is alive — dropping it releases the lock.
    pub(crate) fn get(&self) -> &'static Pdfium {
        self.pdfium
    }
}

/// Locks the process-wide `Pdfium` instance for the duration of a test.
///
/// `Pdfium::bind_to_library` can only succeed once per process, so every test
/// shares one binding. The returned handle holds the lock until it drops —
/// bind it to a named local (`let pdfium = test_pdfium();`), never to `_`,
/// which would drop it immediately and release the lock.
///
/// # Call it once per test
///
/// The lock is **not reentrant**. Calling this twice while the first handle is
/// still in scope deadlocks, and so does calling a helper that acquires it
/// while holding one. That is the cost of the design: the old failure was an
/// intermittent heap corruption that a green run could hide, and this one is a
/// deterministic hang on the very first run. Loud beats silent, but it is
/// still a trap worth knowing about.
///
/// So: helpers should take `&'static Pdfium` from the caller rather than
/// acquire (most already receive `&AppState` and can use `state.pdfium`). The
/// exception is a fully self-contained helper that acquires, does all its
/// pdfium work, and returns plain data — `margins::tests::detect_bytes` is the
/// one such case. Those are safe only while no caller holds a handle.
#[cfg(test)]
pub(crate) fn test_pdfium() -> TestPdfium {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    static PDFIUM: OnceLock<Pdfium> = OnceLock::new();

    // Taken before handing out the instance, and poison is ignored: a panicking
    // test must not cascade into every later one failing to acquire.
    let _lock = LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner());

    let pdfium = PDFIUM.get_or_init(|| {
        let bindings = Pdfium::bind_to_library(resolve_pdfium_path()).expect("bind pdfium");
        Pdfium::new(bindings)
    });

    TestPdfium { pdfium, _lock }
}

/// Path to the small checked-in PDF used by tests that need a real,
/// pdfium- and lopdf-loadable document.
#[cfg(test)]
pub(crate) fn fixture_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.pdf")
}

/// Path to a user-password-protected copy of `sample.pdf` (AESv3/256-bit).
/// The user password is [`ENCRYPTED_FIXTURE_PASSWORD`]. Used by the
/// encrypted-open tests (issue #12).
#[cfg(test)]
pub(crate) fn encrypted_fixture_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample-encrypted.pdf")
}

/// The user password baked into [`encrypted_fixture_path`].
#[cfg(test)]
pub(crate) const ENCRYPTED_FIXTURE_PASSWORD: &str = "open-sesame";
