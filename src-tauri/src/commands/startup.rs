use crate::state::{lock_mutex, AppState};
use tauri::State;

/// Returns the file path passed on the command line at launch (if any),
/// and clears it so it is only consumed once.
#[tauri::command]
pub fn take_startup_file(state: State<'_, AppState>) -> Result<Option<String>, String> {
    Ok(lock_mutex(&state.startup_file)?.take())
}
