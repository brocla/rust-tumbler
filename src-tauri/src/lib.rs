mod commands;
mod state;

use pdfium_render::prelude::*;
use state::AppState;
use std::collections::HashMap;
use std::sync::Mutex;
use tauri::Manager;

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

    let app_state = AppState {
        pdfium,
        documents: Mutex::new(HashMap::new()),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::document::open_document,
            commands::document::close_document,
            commands::render::render_page,
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

/// Resolve the path to pdfium.dll.
/// In dev mode: look relative to the src-tauri directory.
/// In production: look in the bundled resources directory.
fn resolve_pdfium_path() -> String {
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
