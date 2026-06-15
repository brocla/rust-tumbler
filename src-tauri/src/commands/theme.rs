use windows::UI::ViewManagement::{UIColorType, UISettings};
use windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED};

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccentColors {
    pub accent: String,
    pub accent_dim: String,
}

#[tauri::command]
pub fn get_accent_color() -> Result<AccentColors, String> {
    // Run on a brand-new OS thread that has never had COM/WinRT touched.
    // Tauri runs sync commands on a shared blocking-thread pool; if the
    // thread picked for this invocation was previously used by the
    // file-dialog plugin or print_document (both CoInitializeEx as STA),
    // RoInitialize(MTA) here would fail with RPC_E_CHANGED_MODE. A fresh
    // thread has no prior apartment, so RoInitialize always succeeds.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        tx.send(get_accent_color_impl()).ok();
    });
    rx.recv().map_err(|e| format!("channel recv failed: {e}"))?
}

fn get_accent_color_impl() -> Result<AccentColors, String> {
    unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.map_err(|e| format!("RoInitialize failed: {e}"))?;

    let result = (|| {
        let settings = UISettings::new().map_err(|e| format!("UISettings::new failed: {e}"))?;
        let accent = settings
            .GetColorValue(UIColorType::Accent)
            .map_err(|e| format!("GetColorValue(Accent) failed: {e}"))?;
        let dim = settings
            .GetColorValue(UIColorType::AccentDark1)
            .map_err(|e| format!("GetColorValue(AccentDark1) failed: {e}"))?;

        Ok(AccentColors {
            accent: format!("#{:02x}{:02x}{:02x}", accent.R, accent.G, accent.B),
            accent_dim: format!("#{:02x}{:02x}{:02x}", dim.R, dim.G, dim.B),
        })
    })();

    unsafe { RoUninitialize() };

    result
}
