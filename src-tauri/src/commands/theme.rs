use windows::UI::ViewManagement::{UIColorType, UISettings};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccentColors {
    pub accent: String,
    pub accent_dim: String,
}

#[tauri::command]
pub fn get_accent_color() -> Result<AccentColors, String> {
    // TODO: RoInitialize(RO_INIT_MULTITHREADED) can fail with RPC_E_CHANGED_MODE
    // if this command runs on a tokio blocking thread that the file-dialog
    // plugin already CoInitialize'd as STA (e.g. after Ctrl+O reuses the
    // thread). Accepted as a known fragility for now rather than detecting
    // the existing apartment and skipping re-init.
    unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.map_err(|e| format!("RoInitialize failed: {e}"))?;

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
}
