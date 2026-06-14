use crate::state::AppState;
use tauri::State;

/// Returns the file path passed on the command line at launch (if any),
/// and clears it so it is only consumed once.
#[tauri::command]
pub fn take_startup_file(state: State<'_, AppState>) -> Result<Option<String>, String> {
    Ok(state
        .startup_file
        .lock()
        .map_err(|e| format!("Lock error: {e}"))?
        .take())
}
